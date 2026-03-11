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

/// Prediction market outcome configuration (testnet-only).
///
/// Outcomes are binary event markets. Each outcome has two sides (Yes/No).
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

    /// Whether this is a prediction market outcome (testnet-only).
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
                Environment::Mainnet => "http://node.supurr.app",
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
    pub status: String,
    #[serde(default)]
    pub response: Option<HyperliquidOrderResponseData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidOrderResponseData {
    #[serde(rename = "type")]
    pub response_type: String,
    pub data: Option<HyperliquidOrderData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidOrderData {
    pub statuses: Vec<HyperliquidOrderStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidOrderStatus {
    pub resting: Option<HyperliquidRestingOrder>,
    pub filled: Option<HyperliquidFilledOrder>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidRestingOrder {
    pub oid: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidFilledOrder {
    #[serde(rename = "totalSz")]
    pub total_sz: String,
    #[serde(rename = "avgPx")]
    pub avg_px: String,
    pub oid: u64,
}

/// Hyperliquid user fill from userFills endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidUserFill {
    pub coin: String,
    pub px: String,
    pub sz: String,
    pub side: String,
    pub time: u64,
    pub hash: String,
    pub oid: u64,
    pub cloid: Option<String>,
    pub fee: String,
    #[serde(rename = "feeToken")]
    pub fee_token: Option<String>,
    pub tid: Option<u64>,
}

/// Hyperliquid clearinghouse state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidClearinghouseState {
    #[serde(rename = "marginSummary")]
    pub margin_summary: HyperliquidMarginSummary,
    #[serde(rename = "assetPositions")]
    pub asset_positions: Vec<HyperliquidAssetPosition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidMarginSummary {
    #[serde(rename = "accountValue")]
    pub account_value: String,
    #[serde(rename = "totalMarginUsed")]
    pub total_margin_used: String,
    #[serde(rename = "totalNtlPos")]
    pub total_ntl_pos: String,
    #[serde(rename = "totalRawUsd")]
    pub total_raw_usd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidAssetPosition {
    pub position: HyperliquidPosition,
    #[serde(rename = "type")]
    pub position_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperliquidPosition {
    pub coin: String,
    pub szi: String,
    #[serde(rename = "entryPx")]
    pub entry_px: Option<String>,
    #[serde(rename = "positionValue")]
    pub position_value: String,
    #[serde(rename = "unrealizedPnl")]
    pub unrealized_pnl: String,
}
