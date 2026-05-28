//! Hyperliquid-specific types and config.

use bot_core::Environment;
use serde::{Deserialize, Serialize};

/// Builder fee configuration for Hyperliquid orders.
///
/// The builder fee is specified in tenths of a basis point.
/// For example, `fee_tenths_bp: 30` represents 3 basis points (0.03%).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuilderFee {
    /// Builder address to receive the fee (e.g., "0x36be02a397e969e010ccbd7333f4169f66b8989f")
    pub address: String,
    /// Fee in tenths of a basis point (e.g., 30 = 3 bp = 0.03%)
    pub fee_tenths_bp: u32,
}

/// HIP-3 (builder-deployed perp DEX) configuration.
///
/// HIP-3 DEXes are separate perpetual markets deployed by third parties.
/// They can use different collateral tokens (USDC, USDH, etc.).
///
/// Asset ID calculation for HIP-3:
///   `110000 + ((hip_index - 1) * 10000) + asset_index_in_dex_meta`
///
/// For example, DEX at index 1 with asset at index 5:
///   `110000 + ((1 - 1) * 10000) + 5 = 110005`
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Hip3Config {
    /// The HIP-3 DEX name (e.g., "xyz", "flx", "trv").
    /// This is used in API calls as the `dex` parameter.
    pub dex_name: String,

    /// The index of the DEX in the `perpDexs()` response array.
    /// Index 0 is null (main Hyperliquid), so actual DEXes start at index 1.
    pub dex_index: u32,

    /// The quote currency for this HIP-3 DEX.
    /// Common values: "USDC" (default), "USDH"
    /// This determines which clearinghouse to query for balances.
    #[serde(default = "default_quote_currency")]
    pub quote_currency: String,

    /// The asset index within this DEX's meta.universe array.
    /// This is combined with dex_index to calculate the final asset ID.
    #[serde(default)]
    pub asset_index: u32,
}

fn default_quote_currency() -> String {
    "USDC".to_string()
}

impl Hip3Config {
    /// Calculate the HIP-3 asset ID for order placement.
    ///
    /// Formula: `110000 + ((dex_index - 1) * 10000) + asset_index`
    pub fn calculate_asset_id(&self) -> u32 {
        let offset = 110_000 + (self.dex_index.saturating_sub(1) * 10_000);
        offset + self.asset_index
    }

    /// Check if this DEX uses a non-USDC collateral (e.g., USDH).
    /// For non-USDC DEXes, we need to query the DEX-specific clearinghouse.
    pub fn uses_alternate_collateral(&self) -> bool {
        self.quote_currency.to_uppercase() != "USDC"
    }
}

/// Prediction market outcome configuration.
///
/// Outcomes are binary event markets. Each outcome has two sides (Yes/No).
/// Live outcome order books are quoted in USDH.
/// Asset ID calculation: `100_000_000 + (10 * outcome_id + side)`
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OutcomeConfig {
    /// Outcome ID from outcomeMeta (e.g., 516)
    pub outcome_id: u32,
    /// Side: 0 = Yes, 1 = No
    pub side: u8,
    /// Human-readable name (e.g., "BTC > 69070")
    pub name: String,
}

impl OutcomeConfig {
    /// Calculate the encoding: `10 * outcome_id + side`.
    pub fn encoding(&self) -> u32 {
        10 * self.outcome_id + self.side as u32
    }

    /// Calculate the asset ID for order placement: `100_000_000 + encoding`.
    pub fn asset_id(&self) -> u32 {
        100_000_000 + self.encoding()
    }

    /// Get the coin name used in allMids/fills: `#<encoding>`.
    pub fn coin_name(&self) -> String {
        format!("#{}", self.encoding())
    }

    /// Get the token name used in spot balances: `+<encoding>`.
    pub fn token_name(&self) -> String {
        format!("+{}", self.encoding())
    }
}

/// Hyperliquid client configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidConfig {
    /// Environment (mainnet/testnet)
    pub environment: Environment,

    /// Private key for signing (hex, without 0x prefix)
    pub private_key: String,

    /// Optional vault address for vault trading
    pub vault_address: Option<String>,

    /// Optional main address for API wallet scenarios
    pub main_address: Option<String>,

    /// HTTP timeout in seconds
    pub timeout_secs: u64,

    /// Optional HTTP proxy URL
    pub proxy_url: Option<String>,

    /// Optional base URL override (if you want to use a custom gateway)
    pub base_url_override: Option<String>,

    /// Optional builder fee configuration.
    /// If set, all orders will include this builder fee.
    pub builder_fee: Option<BuilderFee>,

    /// Optional HIP-3 (builder-deployed perp DEX) configuration.
    /// If set, the client will use HIP-3 asset IDs and query the appropriate DEX.
    #[serde(default)]
    pub hip3: Option<Hip3Config>,

    /// Whether this is a spot market (no leverage, no margin).
    /// When true:
    /// - Balance queries use spotClearinghouseState instead of perps clearinghouse
    /// - Fill parsing uses -SPOT suffix instead of -PERP
    /// - Skips leverage-related settings
    #[serde(default)]
    pub is_spot: bool,

    /// The expected coin name for spot markets (e.g., "HYPE", "PURR").
    /// Hyperliquid may return fills with an alias like "@107" instead of "HYPE".
    /// If set, this value is used to resolve the alias to the proper coin name.
    #[serde(default)]
    pub spot_coin: Option<String>,

    /// The spot market index (e.g., 10107 for HYPE-SPOT).
    /// Used to derive the @xxx key for spot price lookups from allMids.
    /// Formula: @{spot_market_index - 10000} e.g., 10107 -> @107
    #[serde(default)]
    pub spot_market_index: Option<u32>,

    /// Whether this is a prediction market outcome.
    /// When true:
    /// - Balance queries use spotClearinghouseState (same as spot)
    /// - Fill parsing uses -OUTCOME suffix
    /// - Asset ID uses 100_000_000 + encoding scheme
    #[serde(default)]
    pub is_outcome: bool,

    /// Optional prediction market outcome configuration.
    /// Must be set when is_outcome is true.
    #[serde(default)]
    pub outcome: Option<OutcomeConfig>,
}

impl HyperliquidConfig {
    /// Get the base URL for the API
    pub fn base_url(&self) -> &str {
        if let Some(ref url) = self.base_url_override {
            url
        } else {
            match self.environment {
                Environment::Mainnet => "https://api.hyperliquid.xyz",
                Environment::Testnet => "https://api.hyperliquid-testnet.xyz",
            }
        }
    }
}

impl Default for HyperliquidConfig {
    fn default() -> Self {
        Self {
            environment: Environment::Testnet,
            private_key: String::new(),
            vault_address: None,
            main_address: None,
            timeout_secs: 10,
            proxy_url: None,
            base_url_override: None,
            builder_fee: None,
            hip3: None,
            is_spot: false,
            spot_coin: None,
            spot_market_index: None,
            is_outcome: false,
            outcome: None,
        }
    }
}

/// Hyperliquid order response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidOrderResponse {
    /// Top-level exchange status, usually `ok` or `err`.
    pub status: String,
    #[serde(default)]
    /// Optional typed response body.
    pub response: Option<HyperliquidOrderResponseData>,
}

/// Nested Hyperliquid exchange response payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidOrderResponseData {
    #[serde(rename = "type")]
    /// Response type returned by Hyperliquid, such as `order`.
    pub response_type: String,
    /// Optional order result data.
    pub data: Option<HyperliquidOrderData>,
}

/// Order result data containing one status per submitted order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidOrderData {
    /// Per-order statuses returned in request order.
    pub statuses: Vec<HyperliquidOrderStatus>,
}

/// Status for a single submitted order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidOrderStatus {
    /// Resting order details when the order entered the book.
    pub resting: Option<HyperliquidRestingOrder>,
    /// Immediate fill details when the order executed.
    pub filled: Option<HyperliquidFilledOrder>,
    /// Error string when the order was rejected.
    pub error: Option<String>,
}

/// Resting order response data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidRestingOrder {
    /// Exchange order ID.
    pub oid: u64,
}

/// Filled order response data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidFilledOrder {
    #[serde(rename = "totalSz")]
    /// Total filled size as a wire-format decimal string.
    pub total_sz: String,
    #[serde(rename = "avgPx")]
    /// Average fill price as a wire-format decimal string.
    pub avg_px: String,
    /// Exchange order ID.
    pub oid: u64,
}

/// Hyperliquid user fill from userFills endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidUserFill {
    /// Hyperliquid coin symbol.
    pub coin: String,
    /// Fill price as a wire-format decimal string.
    pub px: String,
    /// Fill size as a wire-format decimal string.
    pub sz: String,
    /// Fill side string from Hyperliquid.
    pub side: String,
    /// Exchange timestamp in milliseconds.
    pub time: u64,
    /// Transaction hash associated with the fill.
    pub hash: String,
    /// Exchange order ID.
    pub oid: u64,
    /// Optional client order ID.
    pub cloid: Option<String>,
    /// Fee amount as a wire-format decimal string.
    pub fee: String,
    #[serde(rename = "feeToken")]
    /// Fee token symbol when present.
    pub fee_token: Option<String>,
    /// Optional exchange trade ID.
    pub tid: Option<u64>,
}

/// Hyperliquid clearinghouse state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidClearinghouseState {
    #[serde(rename = "marginSummary")]
    /// Account-level margin summary.
    pub margin_summary: HyperliquidMarginSummary,
    #[serde(rename = "assetPositions")]
    /// Per-asset positions.
    pub asset_positions: Vec<HyperliquidAssetPosition>,
}

/// Account-level Hyperliquid margin summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidMarginSummary {
    #[serde(rename = "accountValue")]
    /// Account value as a wire-format decimal string.
    pub account_value: String,
    #[serde(rename = "totalMarginUsed")]
    /// Margin currently used as a wire-format decimal string.
    pub total_margin_used: String,
    #[serde(rename = "totalNtlPos")]
    /// Total notional position value as a wire-format decimal string.
    pub total_ntl_pos: String,
    #[serde(rename = "totalRawUsd")]
    /// Raw USD value as a wire-format decimal string.
    pub total_raw_usd: String,
}

/// Hyperliquid asset position wrapper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidAssetPosition {
    /// Position details.
    pub position: HyperliquidPosition,
    #[serde(rename = "type")]
    /// Position category returned by the API.
    pub position_type: String,
}

/// Hyperliquid position details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidPosition {
    /// Coin symbol.
    pub coin: String,
    /// Signed position size as a wire-format decimal string.
    pub szi: String,
    #[serde(rename = "entryPx")]
    /// Entry price as a wire-format decimal string.
    pub entry_px: Option<String>,
    #[serde(rename = "positionValue")]
    /// Position value as a wire-format decimal string.
    pub position_value: String,
    #[serde(rename = "unrealizedPnl")]
    /// Unrealized PnL as a wire-format decimal string.
    pub unrealized_pnl: String,
}
