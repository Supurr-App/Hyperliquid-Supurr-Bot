//! Bot CLI configuration types - V2 format.
//!
//! Strategy config structs (`GridConfigJson`, `MMConfigJson`, etc.) are defined
//! once in `bot-engine` and re-exported here. Only CLI/schema-specific types
//! (`BotConfig`, `MarketConfig`, `Hip3ConfigJson`, `InstrumentMetaConfig`) live here.
//!
//! This avoids duplication: adding a new strategy config means adding it once
//! in `bot-engine/src/config.rs` — it's automatically available here.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// Re-export strategy config structs from bot-engine (single source of truth)
pub use bot_engine::{
    ArbitrageConfigJson, BuilderFeeConfig, DCAConfigJson, GridConfigJson, MMConfigJson,
    SyncConfigJson,
};

/// V2 Bot configuration - the full config format for all strategies.
///
/// This is the **schema-generation** version of BotConfig. It uses flat
/// `MarketConfig` structs (for clean JSON Schema output), unlike the engine's
/// `BotConfig` which uses typed `bot_core::Market` enums for runtime parsing.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BotConfig {
    /// Environment: "testnet" or "mainnet"
    pub environment: String,

    /// Private key (hex, with or without 0x prefix)
    pub private_key: String,

    /// Wallet address
    pub address: String,

    /// Optional vault address
    #[serde(default)]
    pub vault_address: Option<String>,

    /// Strategy type: "grid", "mm", "dca", or "arbitrage"
    pub strategy_type: String,

    /// Markets to trade on (V2 format)
    #[serde(default)]
    pub markets: Vec<MarketConfig>,

    /// Polling delay in milliseconds
    #[serde(default = "default_poll_delay_ms")]
    pub poll_delay_ms: u64,

    // -------------------------------------------------------------------------
    // Strategy-specific config objects (only one should be set)
    // -------------------------------------------------------------------------
    /// Grid strategy configuration
    #[serde(default)]
    pub grid: Option<GridConfigJson>,

    /// Market Maker strategy configuration
    #[serde(default)]
    pub mm: Option<MMConfigJson>,

    /// DCA strategy configuration
    #[serde(default)]
    pub dca: Option<DCAConfigJson>,

    /// Arbitrage strategy configuration
    #[serde(default)]
    pub arbitrage: Option<ArbitrageConfigJson>,

    // -------------------------------------------------------------------------
    // Common config
    // -------------------------------------------------------------------------
    /// Builder fee configuration
    #[serde(default)]
    pub builder_fee: Option<BuilderFeeConfig>,

    /// HIP-3 configuration
    #[serde(default)]
    pub hip3: Option<Hip3ConfigJson>,

    /// Trade sync configuration
    #[serde(default)]
    pub sync: Option<SyncConfigJson>,
}

// =============================================================================
// CLI-specific types (not in bot-engine)
// =============================================================================

/// Market configuration (V2 format) — flat representation for JSON Schema.
///
/// The engine uses `bot_core::Market` (tagged enum) instead. This flat struct
/// exists so the generated JSON Schema is clean for external consumers.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MarketConfig {
    /// Exchange name (e.g., "hyperliquid")
    pub exchange: String,

    /// Market type: "perp", "spot", "hip3"
    #[serde(rename = "type")]
    pub market_type: String,

    /// Base asset (e.g., "BTC")
    pub base: String,

    /// Quote asset (e.g., "USDC")
    #[serde(default)]
    pub quote: Option<String>,

    /// Market index
    #[serde(default)]
    pub index: Option<u32>,

    /// DEX name (for HIP-3)
    #[serde(default)]
    pub dex: Option<String>,

    /// Instrument metadata
    #[serde(default)]
    pub instrument_meta: Option<InstrumentMetaConfig>,
}

/// HIP-3 configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Hip3ConfigJson {
    /// The HIP-3 DEX name
    pub dex_name: String,

    /// The index of the DEX
    pub dex_index: u32,

    /// Quote currency: "USDC" or "USDH"
    #[serde(default = "default_quote_currency")]
    pub quote_currency: String,

    /// Asset index within the DEX
    #[serde(default)]
    pub asset_index: u32,
}

/// Instrument metadata configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InstrumentMetaConfig {
    /// Tick size (price precision)
    pub tick_size: String,

    /// Lot size (quantity precision)
    pub lot_size: String,

    /// Minimum order quantity
    #[serde(default)]
    pub min_qty: Option<String>,

    /// Minimum notional value
    #[serde(default)]
    pub min_notional: Option<String>,
}

// =============================================================================
// Default value functions (only for CLI-specific types)
// =============================================================================

fn default_poll_delay_ms() -> u64 {
    500
}

fn default_quote_currency() -> String {
    "USDC".to_string()
}
