//! Hyperliquid HTTP client implementation.

use bot_core::{
    AccountState, AssetId, ClientOrderId, Environment, Exchange, ExchangeError, ExchangeId,
    ExchangeOrderId, Fill, InstrumentId, MarketIndex, OrderInput, OrderSide, PlaceOrderResult,
    PositionSnapshot, Price, Qty, Quote, TimeInForce, TradeId,
};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use crate::signing::{decimal_to_wire, timestamp_ms, HyperliquidSigner};
use crate::types::{Hip3Config, HyperliquidConfig, OutcomeConfig};

const ADDRESS_LIMIT_REFILL_MS_PER_ACTION: u64 = 10_000;
const ADDRESS_LIMIT_RETRY_BUFFER_MS: u64 = 500;

fn retry_after_for_action_units(action_units: u32) -> u64 {
    ADDRESS_LIMIT_REFILL_MS_PER_ACTION
        .saturating_mul(action_units.max(1) as u64)
        .saturating_add(ADDRESS_LIMIT_RETRY_BUFFER_MS)
}

fn is_cumulative_address_limit_error(reason: &str) -> bool {
    let reason = reason.to_ascii_lowercase();
    reason.contains("too many cumulative requests") || reason.contains("cumulative requests sent")
}

/// Response types from Hyperliquid API (info endpoints)
#[derive(Debug, Clone, serde::Deserialize)]
pub struct HyperliquidUserFill {
    pub coin: String,
    pub px: String,
    pub sz: String,
    pub side: String,
    pub time: u64,
    pub hash: String,
    pub oid: u64,
    #[serde(default)]
    pub tid: Option<u64>,
    #[serde(default)]
    pub fee: String,
    #[serde(rename = "feeToken", default)]
    pub fee_token: Option<String>,
    #[serde(default)]
    pub cloid: Option<String>,
    #[serde(rename = "closedPnl", default)]
    pub closed_pnl: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct HyperliquidOrder {
    pub coin: String,
    pub side: String,
    #[serde(rename = "limitPx")]
    pub limit_px: String,
    pub sz: String,
    pub oid: u64,
    pub timestamp: u64,
    #[serde(default)]
    pub cloid: Option<String>,
}

// NOTE: We intentionally parse /exchange responses as `serde_json::Value` rather than
// a rigid enum. Hyperliquid’s /exchange can return multiple shapes (e.g. `status: "err"`
// with `response` as a string), and strict enums cause avoidable runtime failures.

/// Hyperliquid client
pub struct HyperliquidClient {
    config: HyperliquidConfig,
    signer: HyperliquidSigner,
    http_client: reqwest::Client,
    exchange_id: ExchangeId,
    /// Last seen fill time for cursor-based polling
    last_fill_time: Arc<RwLock<u64>>,
    /// Seen trade IDs for deduplication
    seen_trades: Arc<RwLock<std::collections::HashSet<String>>>,
    /// Cached HIP-3 config for quick access
    hip3: Option<Hip3Config>,
    /// Cached outcome config for quick access
    outcome: Option<OutcomeConfig>,
}

impl HyperliquidClient {
    pub fn new(config: HyperliquidConfig) -> Result<Self, ExchangeError> {
        let is_mainnet = config.environment == Environment::Mainnet;

        let signer = HyperliquidSigner::new(&config.private_key, is_mainnet)
            .map_err(|e| ExchangeError::Configuration(e.to_string()))?;

        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| ExchangeError::Configuration(e.to_string()))?;

        // Clone HIP-3 config for quick access
        let hip3 = config.hip3.clone();

        // Clone outcome config for quick access
        let outcome = config.outcome.clone();

        // Log HIP-3 configuration if present
        if let Some(ref h) = hip3 {
            tracing::info!(
                "[HIP3] Configured: dex_name={}, dex_index={}, quote={}, asset_index={}, calculated_asset_id={}",
                h.dex_name,
                h.dex_index,
                h.quote_currency,
                h.asset_index,
                h.calculate_asset_id()
            );
        }

        // Log outcome configuration if present
        if let Some(ref o) = outcome {
            tracing::info!(
                "[OUTCOME] Configured: name='{}', outcome_id={}, side={}, encoding={}, asset_id={}, coin='{}'",
                o.name,
                o.outcome_id,
                o.side,
                o.encoding(),
                o.asset_id(),
                o.coin_name()
            );
        }

        Ok(Self {
            config,
            signer,
            http_client,
            exchange_id: ExchangeId::new("hyperliquid"),
            last_fill_time: Arc::new(RwLock::new(0)),
            seen_trades: Arc::new(RwLock::new(std::collections::HashSet::new())),
            hip3,
            outcome,
        })
    }

    /// Check if this client is configured for HIP-3
    pub fn is_hip3(&self) -> bool {
        self.hip3.is_some()
    }

    /// Check if this client is configured for prediction markets
    pub fn is_outcome(&self) -> bool {
        self.outcome.is_some()
    }

    /// Get the DEX name for API calls (None for default Hyperliquid)
    fn dex_name(&self) -> Option<&str> {
        self.hip3.as_ref().map(|h| h.dex_name.as_str())
    }

    /// Get the quote currency (defaults to "USDC")
    pub fn quote_currency(&self) -> &str {
        self.hip3
            .as_ref()
            .map(|h| h.quote_currency.as_str())
            .unwrap_or("USDC")
    }

    /// Check if we need to query DEX-specific clearinghouse
    fn uses_alternate_collateral(&self) -> bool {
        self.hip3
            .as_ref()
            .map(|h| h.uses_alternate_collateral())
            .unwrap_or(false)
    }

    /// Get the effective asset ID for order placement.
    /// For outcomes, uses 100_000_000 + encoding.
    /// For HIP-3, uses the calculated HIP-3 asset ID.
    /// For regular perps/spot, returns the provided market_index.
    fn effective_asset_id(&self, market_index: &MarketIndex) -> u32 {
        if let Some(ref outcome) = self.outcome {
            outcome.asset_id()
        } else if let Some(ref hip3) = self.hip3 {
            hip3.calculate_asset_id()
        } else {
            market_index.value()
        }
    }

    /// Get the base URL for API requests
    fn base_url(&self) -> &str {
        if let Some(ref url) = self.config.base_url_override {
            return url;
        }
        match self.config.environment {
            Environment::Mainnet => "http://node.supurr.app",
            Environment::Testnet => "https://api.hyperliquid-testnet.xyz",
        }
    }

    /// Get the user address for API requests.
    /// Prefers vault_address (subaccount/vault) for reads when configured.
    fn user_address(&self) -> String {
        self.config
            .vault_address
            .clone()
            .or_else(|| self.config.main_address.clone())
            .unwrap_or_else(|| self.signer.address_string())
    }

    /// Make an info (read) request
    async fn info_request<T: serde::de::DeserializeOwned>(
        &self,
        payload: serde_json::Value,
    ) -> Result<T, ExchangeError> {
        let url = format!("{}/info", self.base_url());

        let response = self
            .http_client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| ExchangeError::Network(e.to_string()))?;

        let status = response.status();
        if status == reqwest::StatusCode::BAD_GATEWAY
            || status == reqwest::StatusCode::SERVICE_UNAVAILABLE
        {
            return Err(ExchangeError::Unavailable);
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(ExchangeError::RateLimited);
        }
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ExchangeError::Http(format!("HTTP {}: {}", status, text)));
        }

        response
            .json::<T>()
            .await
            .map_err(|e| ExchangeError::Parse(e.to_string()))
    }

    /// Make an exchange (write) request with signing
    async fn exchange_request(
        &self,
        action: serde_json::Value,
        action_units: u32,
    ) -> Result<serde_json::Value, ExchangeError> {
        let url = format!("{}/exchange", self.base_url());
        let nonce = timestamp_ms();

        let signature = self
            .signer
            .sign_l1_action(
                &action,
                nonce,
                self.config.vault_address.as_deref(),
                None, // expires_after
            )
            .await
            .map_err(|e| ExchangeError::Signing(e.to_string()))?;

        let payload = serde_json::json!({
            "action": action,
            "nonce": nonce,
            "signature": signature.to_json(),
            "vaultAddress": self.config.vault_address,
        });

        tracing::info!("=== EXCHANGE REQUEST ===");
        tracing::info!("URL: {}", url);
        tracing::info!("Signer address: {:?}", self.signer.address);
        tracing::info!(
            "Action: {}",
            serde_json::to_string(&action).unwrap_or_default()
        );
        tracing::info!("Nonce: {}", nonce);
        tracing::info!(
            "Signature: r={}, s={}, v={}",
            signature.r,
            signature.s,
            signature.v
        );
        tracing::info!(
            "Full payload: {}",
            serde_json::to_string(&payload).unwrap_or_default()
        );

        let response = self
            .http_client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| ExchangeError::Network(e.to_string()))?;

        let status = response.status();
        if status == reqwest::StatusCode::BAD_GATEWAY
            || status == reqwest::StatusCode::SERVICE_UNAVAILABLE
        {
            return Err(ExchangeError::Unavailable);
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(ExchangeError::RateLimited);
        }

        let text = response.text().await.unwrap_or_default();
        tracing::info!("=== EXCHANGE RESPONSE ===");
        tracing::info!("HTTP Status: {}", status);
        tracing::info!("Response: {}", text);

        if !status.is_success() {
            return Err(ExchangeError::Http(format!("HTTP {}: {}", status, text)));
        }

        let value = serde_json::from_str::<serde_json::Value>(&text).map_err(|e| {
            ExchangeError::Parse(format!(
                "{} | raw={}",
                e,
                text.chars().take(2_000).collect::<String>()
            ))
        })?;

        if let Some(error) = Self::address_limit_error(&value, action_units) {
            return Err(error);
        }

        Ok(value)
    }

    fn exchange_status(resp: &serde_json::Value) -> Option<&str> {
        resp.get("status").and_then(|v| v.as_str())
    }

    fn exchange_reject_reason(resp: &serde_json::Value) -> String {
        // Common patterns:
        // - {"error": "..."}
        // - {"status":"err","response":"..."}
        // - {"status":"err","response":{"error":"..."}} or other object
        if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
            return err.to_string();
        }
        if let Some(r) = resp.get("response") {
            if let Some(s) = r.as_str() {
                return s.to_string();
            }
            if let Some(err) = r.get("error").and_then(|v| v.as_str()) {
                return err.to_string();
            }
            if let Ok(s) = serde_json::to_string(r) {
                return s;
            }
        }
        serde_json::to_string(resp).unwrap_or_else(|_| format!("{:?}", resp))
    }

    fn address_limit_error(resp: &serde_json::Value, action_units: u32) -> Option<ExchangeError> {
        if Self::exchange_status(resp) == Some("ok") {
            return None;
        }

        let reason = Self::exchange_reject_reason(resp);
        if !is_cumulative_address_limit_error(&reason) {
            return None;
        }

        let needed = action_units.max(1);
        Some(ExchangeError::WouldExceedUserActionLimit {
            retry_after_ms: retry_after_for_action_units(needed),
            needed,
        })
    }

    fn extract_order_statuses(resp: &serde_json::Value) -> Option<&Vec<serde_json::Value>> {
        // Hyperliquid has multiple observed shapes across endpoints and error cases.
        // For order placement, the common shape is:
        // {"status":"ok","response":{"type":"order","data":{"statuses":[...]}}}
        //
        // We try a few reasonable JSON pointer paths to be resilient to minor variations.
        resp.pointer("/response/data/statuses")
            .and_then(|v| v.as_array())
            .or_else(|| {
                resp.pointer("/response/response/data/statuses")
                    .and_then(|v| v.as_array())
            })
            .or_else(|| {
                resp.pointer("/response/statuses")
                    .and_then(|v| v.as_array())
            })
    }

    /// Build order wire format
    fn build_order_wire(
        &self,
        market_index: &MarketIndex,
        client_id: &ClientOrderId,
        side: OrderSide,
        price: Price,
        qty: Qty,
        tif: TimeInForce,
        post_only: bool,
        reduce_only: bool,
    ) -> serde_json::Value {
        let is_buy = side == OrderSide::Buy;

        let order_type = match tif {
            TimeInForce::Gtc if post_only => {
                serde_json::json!({ "limit": { "tif": "Alo" } })
            }
            TimeInForce::Gtc => {
                serde_json::json!({ "limit": { "tif": "Gtc" } })
            }
            TimeInForce::Ioc => {
                serde_json::json!({ "limit": { "tif": "Ioc" } })
            }
            TimeInForce::Fok => {
                serde_json::json!({ "limit": { "tif": "Ioc" } })
            }
        };

        // Use effective asset ID (HIP-3 calculated ID or regular market_index)
        let asset_id = self.effective_asset_id(market_index);

        let mut order_wire = serde_json::json!({
            "a": asset_id,
            "b": is_buy,
            "p": decimal_to_wire(&price.0),
            "s": decimal_to_wire(&qty.0),
            "r": reduce_only,
            "t": order_type,
        });

        // Add client order ID if it starts with 0x (required format)
        let cloid = client_id.to_string();
        if cloid.starts_with("0x") && cloid.len() == 34 {
            order_wire["c"] = serde_json::Value::String(cloid);
        }

        order_wire
    }

    /// Parse fills from Hyperliquid format
    fn parse_fills(&self, fills: Vec<HyperliquidUserFill>) -> Vec<Fill> {
        fills
            .into_iter()
            .map(|f| {
                // Detect market type from fill coin format:
                //   "#5160"  → outcome fill
                //   "@107"   → spot fill
                //   "BTC"    → perp fill
                let is_outcome_fill = f.coin.starts_with('#');
                let is_spot_fill = f.coin.starts_with('@');
                let suffix = if is_outcome_fill {
                    "OUTCOME"
                } else if is_spot_fill {
                    "SPOT"
                } else {
                    "PERP"
                };
                let trade_id = f
                    .tid
                    .map(|tid| TradeId::new(tid.to_string()))
                    .unwrap_or_else(|| TradeId::new(f.hash.clone()));

                let client_id = f.cloid.map(|c| ClientOrderId::new(c));

                let exchange_order_id = Some(ExchangeOrderId::new(f.oid.to_string()));

                let side = if f.side == "B" {
                    OrderSide::Buy
                } else {
                    OrderSide::Sell
                };

                let price = Price::new(Decimal::from_str(&f.px).unwrap_or_default());
                let gross_qty = Decimal::from_str(&f.sz).unwrap_or_default();

                // For fee asset, use the configured quote currency or default to feeToken
                let fee_amount = Decimal::from_str(&f.fee).unwrap_or_default();
                let fee_token = f.fee_token.clone();
                let fee_asset = AssetId::new(
                    fee_token
                        .clone()
                        .unwrap_or_else(|| self.quote_currency().to_string()),
                );
                let fee = bot_core::Fee::new(fee_amount, fee_asset);

                // Always use gross qty from the exchange here.
                // Fee deduction for position tracking is handled downstream
                // in the engine runner's apply_fill_and_emit_events.
                let qty = Qty::new(gross_qty);

                // Map coin to instrument_id with correct suffix.
                // Outcome fills use the coin name directly (e.g., "#5160-OUTCOME").
                // Spot fills may need alias resolution ("@107" → configured coin name).
                let coin_name = if is_outcome_fill {
                    // Outcome fills: coin is already "#5160" format, use as-is
                    f.coin.clone()
                } else if is_spot_fill && f.coin.starts_with('@') {
                    if let Some(ref configured_coin) = self.config.spot_coin {
                        tracing::debug!("Resolving spot alias: {} -> {}", f.coin, configured_coin);
                        configured_coin.clone()
                    } else {
                        tracing::warn!(
                            "Spot fill has alias '{}' but spot_coin not configured - using alias",
                            f.coin
                        );
                        f.coin.clone()
                    }
                } else {
                    f.coin.clone()
                };
                let instrument = InstrumentId::new(format!("{}-{}", coin_name, suffix));

                Fill {
                    trade_id,
                    client_id,
                    exchange_order_id,
                    instrument,
                    side,
                    price,
                    qty,
                    fee,
                    ts: f.time as i64,
                }
            })
            .collect()
    }

    /// Fetch all mid prices
    pub async fn fetch_all_mids(&self) -> Result<HashMap<String, Decimal>, ExchangeError> {
        // For HIP-3, we need to pass the dex parameter
        let payload = if let Some(dex) = self.dex_name() {
            serde_json::json!({
                "type": "allMids",
                "dex": dex
            })
        } else {
            serde_json::json!({
                "type": "allMids"
            })
        };

        let mids: HashMap<String, String> = self.info_request(payload).await?;

        let mut result = HashMap::new();
        for (coin, price_str) in mids {
            if let Ok(price) = Decimal::from_str(&price_str) {
                result.insert(coin, price);
            }
        }

        Ok(result)
    }

    /// Fetch open orders for user
    pub async fn fetch_open_orders(&self) -> Result<Vec<HyperliquidOrder>, ExchangeError> {
        // For HIP-3, we need to pass the dex parameter
        let payload = if let Some(dex) = self.dex_name() {
            serde_json::json!({
                "type": "openOrders",
                "user": self.user_address(),
                "dex": dex
            })
        } else {
            serde_json::json!({
                "type": "openOrders",
                "user": self.user_address()
            })
        };

        let orders: Vec<HyperliquidOrder> = self.info_request(payload).await?;
        Ok(orders)
    }

    /// Fetch user state (positions, balances)
    ///
    /// For spot markets, uses spotClearinghouseState.
    /// For HIP-3 DEXes with non-USDC collateral (e.g., USDH), we query the DEX-specific
    /// clearinghouse. For USDC-collateral HIP-3 DEXes with DEX abstraction enabled,
    /// the main clearinghouse is used.
    pub async fn fetch_user_state(&self) -> Result<serde_json::Value, ExchangeError> {
        // For outcome trading, query the spot clearinghouse (outcomes are spot-like)
        if self.config.is_outcome {
            return self.fetch_spot_user_state().await;
        }

        // For spot trading, query the spot clearinghouse
        if self.config.is_spot {
            return self.fetch_spot_user_state().await;
        }

        // For HIP-3 with alternate collateral (non-USDC), query the DEX-specific clearinghouse
        let payload = if self.uses_alternate_collateral() {
            if let Some(dex) = self.dex_name() {
                serde_json::json!({
                    "type": "clearinghouseState",
                    "user": self.user_address(),
                    "dex": dex
                })
            } else {
                serde_json::json!({
                    "type": "clearinghouseState",
                    "user": self.user_address()
                })
            }
        } else {
            // For default perps or USDC-collateral HIP-3, query main clearinghouse
            serde_json::json!({
                "type": "clearinghouseState",
                "user": self.user_address()
            })
        };

        self.info_request(payload).await
    }

    /// Fetch user state for the HIP-3 DEX specifically (regardless of collateral type).
    /// Useful when you need to query both the main and DEX-specific clearinghouses.
    pub async fn fetch_hip3_user_state(&self) -> Result<serde_json::Value, ExchangeError> {
        if let Some(dex) = self.dex_name() {
            let payload = serde_json::json!({
                "type": "clearinghouseState",
                "user": self.user_address(),
                "dex": dex
            });
            self.info_request(payload).await
        } else {
            // Fallback to main clearinghouse if not HIP-3
            self.fetch_user_state().await
        }
    }

    /// Fetch spot user state
    pub async fn fetch_spot_user_state(&self) -> Result<serde_json::Value, ExchangeError> {
        let payload = serde_json::json!({
            "type": "spotClearinghouseState",
            "user": self.user_address()
        });

        self.info_request(payload).await
    }

    /// Register this user with hl-proxy for fills tracking.
    /// Call this on strategy startup.
    pub async fn register_user(&self) -> Result<(), ExchangeError> {
        let payload = serde_json::json!({
            "type": "register",
            "user": self.user_address()
        });

        let response: serde_json::Value = self.info_request(payload).await?;

        match response.get("status").and_then(|v| v.as_str()) {
            Some("ok") => {
                tracing::info!("Registered user {} for fills tracking", self.user_address());
                Ok(())
            }
            _ => {
                let msg = response
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                tracing::warn!("Failed to register user: {}", msg);
                // Don't fail startup - proxy might not be running
                Ok(())
            }
        }
    }

    /// Deregister this user from hl-proxy fills tracking.
    /// Call this on strategy shutdown.
    pub async fn deregister_user(&self) -> Result<(), ExchangeError> {
        let payload = serde_json::json!({
            "type": "deregister",
            "user": self.user_address()
        });

        let response: serde_json::Value = self.info_request(payload).await?;

        match response.get("status").and_then(|v| v.as_str()) {
            Some("ok") => {
                tracing::info!(
                    "Deregistered user {} from fills tracking",
                    self.user_address()
                );
                Ok(())
            }
            _ => {
                let msg = response
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                tracing::warn!("Failed to deregister user: {}", msg);
                Ok(())
            }
        }
    }

    /// Fetch user fills with deduplication
    pub async fn fetch_user_fills(&self) -> Result<Vec<Fill>, ExchangeError> {
        let payload = serde_json::json!({
            "type": "userFills",
            "user": self.user_address()
        });

        let fills: Vec<HyperliquidUserFill> = self.info_request(payload).await?;

        // Deduplicate fills
        let mut seen = self.seen_trades.write().await;
        let new_fills: Vec<HyperliquidUserFill> = fills
            .into_iter()
            .filter(|f| {
                let key = f
                    .tid
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| f.hash.clone());
                seen.insert(key)
            })
            .collect();

        // Update last fill time
        if let Some(last) = new_fills.last() {
            let mut last_time = self.last_fill_time.write().await;
            if last.time > *last_time {
                *last_time = last.time;
            }
        }

        Ok(self.parse_fills(new_fills))
    }

    /// Cancel an order by exchange OID
    async fn cancel_by_oid(
        &self,
        market_index: &MarketIndex,
        oid: u64,
    ) -> Result<(), ExchangeError> {
        // Use effective asset ID (HIP-3 calculated ID or regular market_index)
        let asset_id = self.effective_asset_id(market_index);

        let action = serde_json::json!({
            "type": "cancel",
            "cancels": [{
                "a": asset_id,
                "o": oid
            }]
        });

        let response = self.exchange_request(action, 1).await?;

        match Self::exchange_status(&response) {
            Some("ok") => Ok(()),
            Some(_) | None => Err(ExchangeError::Rejected(format!(
                "Cancel failed: {}",
                Self::exchange_reject_reason(&response)
            ))),
        }
    }

    /// Cancel an order by client order ID (cloid)
    async fn cancel_by_cloid(
        &self,
        market_index: &MarketIndex,
        cloid: &str,
    ) -> Result<(), ExchangeError> {
        // Use effective asset ID (HIP-3 calculated ID or regular market_index)
        let asset_id = self.effective_asset_id(market_index);

        let action = serde_json::json!({
            "type": "cancelByCloid",
            "cancels": [{
                "asset": asset_id,
                "cloid": cloid
            }]
        });

        let response = self.exchange_request(action, 1).await?;

        match Self::exchange_status(&response) {
            Some("ok") => Ok(()),
            Some(_) | None => Err(ExchangeError::Rejected(format!(
                "Cancel failed: {}",
                Self::exchange_reject_reason(&response)
            ))),
        }
    }

    /// Update leverage for the given asset on Hyperliquid.
    ///
    /// This must be called before placing orders to ensure the account has the correct
    /// leverage setting. Hyperliquid stores leverage as a per-asset account setting.
    ///
    /// # Arguments
    /// * `market_index` - The market index (asset ID) to update leverage for
    /// * `leverage` - The new leverage value (e.g., 10 for 10x)
    /// * `is_cross` - Whether to use cross-margin (true) or isolated margin (false)
    pub async fn update_leverage(
        &self,
        market_index: &MarketIndex,
        leverage: u32,
        is_cross: bool,
    ) -> Result<(), ExchangeError> {
        if leverage == 0 {
            return Err(ExchangeError::Configuration(
                "Leverage must be greater than zero".to_string(),
            ));
        }

        // Use effective asset ID (HIP-3 calculated ID or regular market_index)
        let asset_id = self.effective_asset_id(market_index);

        let action = serde_json::json!({
            "type": "updateLeverage",
            "asset": asset_id,
            "isCross": is_cross,
            "leverage": leverage
        });

        tracing::info!(
            "Updating leverage: asset={} leverage={}x is_cross={}",
            asset_id,
            leverage,
            is_cross
        );

        let response = self.exchange_request(action, 1).await?;

        match Self::exchange_status(&response) {
            Some("ok") => {
                tracing::info!(
                    "Leverage updated successfully: asset={} leverage={}x",
                    asset_id,
                    leverage
                );
                Ok(())
            }
            Some(_) | None => Err(ExchangeError::Rejected(format!(
                "Leverage update failed: {}",
                Self::exchange_reject_reason(&response)
            ))),
        }
    }
}

#[async_trait::async_trait]
impl Exchange for HyperliquidClient {
    fn exchange_id(&self) -> &ExchangeId {
        &self.exchange_id
    }

    fn environment(&self) -> Environment {
        self.config.environment
    }

    async fn init(&self) -> Result<(), ExchangeError> {
        tracing::info!(
            "[init] Validating exchange connection for {}",
            self.user_address()
        );

        // 1. Validate connectivity — fetch account state
        let state = self.fetch_user_state().await?;
        tracing::info!("[init] Connected. Account state fetched successfully.");

        // 2. If vault_address configured, log vault info
        if let Some(ref vault) = self.config.vault_address {
            tracing::info!("[init] Vault/subaccount configured: {}", vault);

            let account_value = state
                .pointer("/marginSummary/accountValue")
                .and_then(|v| v.as_str());

            match account_value {
                Some(val) => tracing::info!("[init] Vault account value: {}", val),
                None => {
                    tracing::warn!(
                        "[init] Could not read vault account value — vault may be empty or inaccessible"
                    );
                }
            }
        }

        Ok(())
    }

    async fn place_orders(
        &self,
        orders: &[OrderInput],
    ) -> Result<Vec<PlaceOrderResult>, ExchangeError> {
        if orders.is_empty() {
            return Ok(Vec::new());
        }

        // Build order wires for all orders
        let order_wires: Vec<serde_json::Value> = orders
            .iter()
            .map(|o| {
                self.build_order_wire(
                    &o.market_index,
                    &o.client_id,
                    o.side,
                    o.price,
                    o.qty,
                    o.tif,
                    o.post_only,
                    o.reduce_only,
                )
            })
            .collect();

        // Build batch order action
        let mut action = serde_json::json!({
            "type": "order",
            "orders": order_wires,
            "grouping": "na"
        });

        // Add builder fee if configured
        if let Some(ref builder_fee) = self.config.builder_fee {
            action["builder"] = serde_json::json!({
                "b": builder_fee.address,
                "f": builder_fee.fee_tenths_bp
            });
            tracing::info!(
                "[builder-fee] attaching builder fee: address={}, fee={} (tenths-of-bp)",
                builder_fee.address,
                builder_fee.fee_tenths_bp
            );
        }

        tracing::info!("=== PLACING {} ORDERS (BATCH) ===", orders.len());
        for (i, o) in orders.iter().enumerate() {
            let effective_asset = self.effective_asset_id(&o.market_index);
            tracing::info!(
                "  Order {}: side={}, qty={}, price={}, client_id={}, asset_id={}",
                i,
                o.side,
                o.qty,
                o.price,
                o.client_id,
                effective_asset
            );
        }
        tracing::info!(
            "Order Wires: {}",
            serde_json::to_string(&order_wires).unwrap_or_default()
        );

        let response = self.exchange_request(action, orders.len() as u32).await?;

        match Self::exchange_status(&response) {
            Some("ok") => {
                // Parse results for each order from the response
                // Expected shape: {"status":"ok","response":{"type":"order","data":{"statuses":[...]}}}
                let statuses = match Self::extract_order_statuses(&response) {
                    Some(statuses) => statuses,
                    None => {
                        return Err(ExchangeError::Rejected(format!(
                            "Order response missing statuses: {}",
                            Self::exchange_reject_reason(&response)
                        )));
                    }
                };

                let has_address_limit_error = statuses.iter().any(|status| {
                    status
                        .get("error")
                        .and_then(|error| error.as_str())
                        .map(is_cumulative_address_limit_error)
                        .unwrap_or(false)
                });
                let all_order_statuses_are_errors = statuses
                    .iter()
                    .take(orders.len())
                    .all(|status| status.get("error").is_some());

                if has_address_limit_error && all_order_statuses_are_errors {
                    let needed = orders.len().max(1) as u32;
                    return Err(ExchangeError::WouldExceedUserActionLimit {
                        retry_after_ms: retry_after_for_action_units(needed),
                        needed,
                    });
                }

                let mut results = Vec::with_capacity(orders.len());

                for (i, order) in orders.iter().enumerate() {
                    let status = statuses.get(i);

                    // Check for error in this order's status
                    let error = status
                        .and_then(|s| s.get("error"))
                        .and_then(|e| e.as_str())
                        .map(|s| s.to_string());

                    if let Some(err) = error {
                        tracing::warn!("Order {} rejected: {}", order.client_id, err);
                        results.push(PlaceOrderResult::Rejected { reason: err });
                        continue;
                    }

                    // Try to extract OID from resting or filled status
                    let resting_status = status.and_then(|s| s.get("resting"));
                    let filled_status = status.and_then(|s| s.get("filled"));

                    let oid = resting_status
                        .and_then(|r| r.get("oid"))
                        .and_then(|o| o.as_u64())
                        .or_else(|| {
                            filled_status
                                .and_then(|f| f.get("oid"))
                                .and_then(|o| o.as_u64())
                        });

                    // Extract fill info from filled status (for IOC orders)
                    let filled_qty = filled_status
                        .and_then(|f| f.get("totalSz"))
                        .and_then(|s| s.as_str())
                        .and_then(|s| s.parse::<rust_decimal::Decimal>().ok())
                        .map(Qty::new);

                    let avg_fill_px = filled_status
                        .and_then(|f| f.get("avgPx"))
                        .and_then(|s| s.as_str())
                        .and_then(|s| s.parse::<rust_decimal::Decimal>().ok())
                        .map(Price::new);

                    if let Some(oid) = oid {
                        if filled_qty.is_some() {
                            tracing::info!(
                                "Order {} filled: oid={} qty={:?} px={:?}",
                                order.client_id,
                                oid,
                                filled_qty,
                                avg_fill_px
                            );
                        } else {
                            tracing::info!("Order {} accepted: oid={}", order.client_id, oid);
                        }
                        results.push(PlaceOrderResult::Accepted {
                            exchange_order_id: Some(ExchangeOrderId::new(oid.to_string())),
                            filled_qty,
                            avg_fill_px,
                        });
                    } else {
                        // If we can't parse detailed status, assume success
                        tracing::warn!(
                            "Order {} accepted but could not parse oid",
                            order.client_id
                        );
                        results.push(PlaceOrderResult::Accepted {
                            exchange_order_id: None,
                            filled_qty: None,
                            avg_fill_px: None,
                        });
                    }
                }

                Ok(results)
            }
            Some(_) | None => {
                let reason = Self::exchange_reject_reason(&response);
                tracing::warn!("Batch order rejected: {}", reason);
                Err(ExchangeError::Rejected(reason))
            }
        }
    }

    async fn cancel_order(
        &self,
        _instrument: &InstrumentId,
        market_index: &MarketIndex,
        client_id: &ClientOrderId,
        exchange_order_id: Option<&ExchangeOrderId>,
    ) -> Result<(), ExchangeError> {
        // Prefer canceling by exchange OID if available (more reliable)
        if let Some(oid) = exchange_order_id {
            if let Ok(oid_num) = oid.to_string().parse::<u64>() {
                tracing::info!("Canceling order by oid: {}", oid_num);
                return self.cancel_by_oid(market_index, oid_num).await;
            }
        }

        // Fall back to cancel by cloid
        let cloid = client_id.to_string();
        if cloid.starts_with("0x") {
            tracing::info!("Canceling order by cloid: {}", cloid);
            return self.cancel_by_cloid(market_index, &cloid).await;
        }

        Err(ExchangeError::Rejected(
            "No valid OID or cloid for cancel".to_string(),
        ))
    }

    async fn cancel_all_orders(
        &self,
        _instrument: &InstrumentId,
        market_index: &MarketIndex,
    ) -> Result<u32, ExchangeError> {
        // Fetch all open orders for this market
        let orders = self.fetch_open_orders().await?;

        if orders.is_empty() {
            return Ok(0);
        }

        // Use effective asset ID (HIP-3 calculated ID or regular market_index)
        let asset_id = self.effective_asset_id(market_index);

        // Build batch cancel request - all cancels in a single API call
        let cancels: Vec<serde_json::Value> = orders
            .iter()
            .map(|order| {
                serde_json::json!({
                    "a": asset_id,
                    "o": order.oid
                })
            })
            .collect();

        let num_orders = cancels.len() as u32;

        tracing::info!(
            "Batch canceling {} orders with asset_id={}",
            num_orders,
            asset_id
        );

        let action = serde_json::json!({
            "type": "cancel",
            "cancels": cancels
        });

        let response = self.exchange_request(action, num_orders).await?;

        match Self::exchange_status(&response) {
            Some("ok") => {
                tracing::info!("Batch cancel successful: {} orders", num_orders);
                Ok(num_orders)
            }
            Some(_) | None => {
                let reason = Self::exchange_reject_reason(&response);
                tracing::warn!("Batch cancel failed: {}", reason);
                Err(ExchangeError::Rejected(format!(
                    "Batch cancel failed: {}",
                    reason
                )))
            }
        }
    }

    async fn poll_user_fills(&self, _cursor: Option<&str>) -> Result<Vec<Fill>, ExchangeError> {
        self.fetch_user_fills().await
    }

    async fn poll_quotes(&self, instruments: &[InstrumentId]) -> Result<Vec<Quote>, ExchangeError> {
        let mids = self.fetch_all_mids().await?;
        let now = bot_core::now_ms();

        let mut quotes = Vec::new();
        for instrument in instruments {
            // Extract coin name from instrument (e.g., "BTC-PERP" -> "BTC")
            // For HIP-3: "xyz:AAPL-PERP" -> "xyz:AAPL"
            // For Outcome: "#5160-OUTCOME" -> "#5160"
            let instrument_str = instrument.to_string();
            let is_spot_instrument = instrument_str.ends_with("-SPOT");
            let is_outcome_instrument = instrument_str.ends_with("-OUTCOME");

            // Determine the lookup key based on market type
            let lookup_key = if is_outcome_instrument {
                // Outcome format: "#5160-OUTCOME" -> "#5160"
                instrument_str
                    .rsplit_once('-')
                    .map(|(base, _)| base)
                    .unwrap_or(&instrument_str)
                    .to_string()
            } else if is_spot_instrument && self.config.spot_market_index.is_some() {
                // For spot instruments with configured market index, use @{index-10000}
                // e.g., market_index 10107 -> lookup key "@107"
                let spot_index = self.config.spot_market_index.unwrap() - 10000;
                format!("@{}", spot_index)
            } else if instrument_str.contains(':') {
                // HIP-3 format: "xyz:AAPL-PERP" -> "xyz:AAPL"
                instrument_str
                    .rsplit_once('-')
                    .map(|(base, _)| base)
                    .unwrap_or(&instrument_str)
                    .to_string()
            } else {
                // Regular format: "BTC-PERP" -> "BTC"
                instrument_str.split('-').next().unwrap_or("").to_string()
            };

            if let Some(mid) = mids.get(&lookup_key) {
                // For simplicity, use mid as both bid and ask
                // In production, you'd want to fetch L2 book for proper bid/ask
                quotes.push(Quote {
                    instrument: instrument.clone(),
                    bid: Price::new(*mid),
                    ask: Price::new(*mid),
                    bid_size: Qty::new(Decimal::ZERO),
                    ask_size: Qty::new(Decimal::ZERO),
                    ts: now,
                });
            } else {
                tracing::warn!(
                    "No mid price found for {} (lookup_key={})",
                    instrument,
                    lookup_key
                );
            }
        }

        Ok(quotes)
    }

    async fn poll_account_state(&self) -> Result<AccountState, ExchangeError> {
        let raw_state = self.fetch_user_state().await?;

        // Transform raw JSON to AccountState
        let mut positions = Vec::new();
        let mut account_value = None;
        let mut unrealized_pnl = None;

        // Extract clearinghouseState if available
        if let Some(clearing) = raw_state.get("clearinghouseState") {
            // Extract account value
            if let Some(margin_summary) = clearing.get("marginSummary") {
                if let Some(av) = margin_summary.get("accountValue").and_then(|v| v.as_str()) {
                    account_value = Decimal::from_str(av).ok();
                }
                if let Some(upnl) = margin_summary.get("unrealizedPnl").and_then(|v| v.as_str()) {
                    unrealized_pnl = Decimal::from_str(upnl).ok();
                }
            }

            // Extract asset positions
            if let Some(asset_positions) = clearing.get("assetPositions").and_then(|v| v.as_array())
            {
                for pos in asset_positions {
                    if let (Some(coin), Some(szi_str)) = (
                        pos.get("position")
                            .and_then(|p| p.get("coin"))
                            .and_then(|v| v.as_str()),
                        pos.get("position")
                            .and_then(|p| p.get("szi"))
                            .and_then(|v| v.as_str()),
                    ) {
                        if let Ok(qty) = Decimal::from_str(szi_str) {
                            let instrument = InstrumentId::new(coin.to_string());
                            let avg_entry = pos
                                .get("position")
                                .and_then(|p| p.get("entryPx"))
                                .and_then(|v| v.as_str())
                                .and_then(|s| Decimal::from_str(s).ok())
                                .map(Price::new);

                            let pos_pnl = pos
                                .get("position")
                                .and_then(|p| p.get("unrealizedPnl"))
                                .and_then(|v| v.as_str())
                                .and_then(|s| Decimal::from_str(s).ok());

                            let liq_px = pos
                                .get("position")
                                .and_then(|p| p.get("liquidationPx"))
                                .and_then(|v| v.as_str())
                                .and_then(|s| Decimal::from_str(s).ok());

                            positions.push(PositionSnapshot {
                                instrument,
                                qty,
                                avg_entry_px: avg_entry,
                                unrealized_pnl: pos_pnl,
                                liquidation_px: liq_px,
                            });
                        }
                    }
                }
            }
        }

        Ok(AccountState {
            positions,
            account_value,
            unrealized_pnl,
        })
    }
}

// =============================================================================
// Outcome-specific methods
// =============================================================================

impl HyperliquidClient {
    /// Fetch outcome metadata from the testnet info endpoint.
    ///
    /// Returns the full outcomeMeta response including outcomes and questions.
    /// This is only available on testnet.
    pub async fn fetch_outcome_meta(&self) -> Result<serde_json::Value, ExchangeError> {
        let payload = serde_json::json!({
            "type": "outcomeMeta"
        });

        self.info_request(payload).await
    }
}

/// Create a new Hyperliquid client
pub fn new_client(
    config: HyperliquidConfig,
) -> Result<std::sync::Arc<dyn Exchange>, ExchangeError> {
    let client = HyperliquidClient::new(config)?;
    Ok(std::sync::Arc::new(client))
}

/// Create a new Hyperliquid client and return a separate handle for registration.
/// Returns (exchange_trait_object, registration_handle).
/// The registration_handle exposes `register_user()` and `deregister_user()`.
pub fn new_client_with_registration(
    config: HyperliquidConfig,
) -> Result<
    (
        std::sync::Arc<dyn Exchange>,
        std::sync::Arc<HyperliquidClient>,
    ),
    ExchangeError,
> {
    let client = std::sync::Arc::new(HyperliquidClient::new(config)?);
    let exchange: std::sync::Arc<dyn Exchange> = client.clone();
    Ok((exchange, client))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cumulative_address_limit_error_maps_to_deferred_retry() {
        let response = serde_json::json!({
            "status": "err",
            "response": "Too many cumulative requests sent. Rate limited."
        });

        let error = HyperliquidClient::address_limit_error(&response, 10);

        assert!(matches!(
            error,
            Some(ExchangeError::WouldExceedUserActionLimit {
                retry_after_ms: 100_500,
                needed: 10
            })
        ));
    }

    #[test]
    fn non_limit_exchange_error_is_not_reclassified() {
        let response = serde_json::json!({
            "status": "err",
            "response": "Insufficient margin"
        });

        assert!(HyperliquidClient::address_limit_error(&response, 1).is_none());
    }

    #[test]
    fn retry_delay_uses_one_action_minimum() {
        assert_eq!(retry_after_for_action_units(0), 10_500);
        assert_eq!(retry_after_for_action_units(1), 10_500);
        assert_eq!(retry_after_for_action_units(3), 30_500);
    }
}
