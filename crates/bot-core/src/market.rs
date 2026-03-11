//! Market types: unified market configuration enum.
//!
//! This module provides a type-safe `Market` enum that replaces scattered fields
//! (instrument, market_index, is_spot, hip3) with a single source of truth.

use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    AssetId, Environment, ExchangeId, ExchangeInstance, InstrumentId, InstrumentKind, MarketIndex,
};

// =============================================================================
// Top-Level Market Enum
// =============================================================================

/// Unified market configuration — replaces scattered config fields.
///
/// This is the canonical representation of "where to trade".
/// All exchange/market-type-specific details are encapsulated here.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "exchange")]
pub enum Market {
    /// Hyperliquid exchange markets
    #[serde(rename = "hyperliquid")]
    Hyperliquid(HyperliquidMarket),
    // Future: Binance, Bybit, etc.
    // #[serde(rename = "binance")]
    // Binance(BinanceMarket),
}

impl Market {
    /// Get the canonical instrument ID for engine use.
    pub fn instrument_id(&self) -> InstrumentId {
        match self {
            Market::Hyperliquid(hl) => hl.instrument_id(),
        }
    }

    /// Get the exchange-specific market index.
    pub fn market_index(&self) -> MarketIndex {
        match self {
            Market::Hyperliquid(hl) => hl.market_index(),
        }
    }

    /// Check if this is a spot market.
    pub fn is_spot(&self) -> bool {
        match self {
            Market::Hyperliquid(hl) => hl.is_spot(),
        }
    }

    /// Get the base asset (e.g., "BTC", "HYPE").
    pub fn base(&self) -> &str {
        match self {
            Market::Hyperliquid(hl) => hl.base(),
        }
    }

    /// Get the quote asset (e.g., "USDC", "USDE").
    pub fn quote(&self) -> &str {
        match self {
            Market::Hyperliquid(hl) => hl.quote(),
        }
    }

    /// Get the base asset ID.
    pub fn base_asset(&self) -> AssetId {
        AssetId::new(self.base())
    }

    /// Get the quote asset ID.
    pub fn quote_asset(&self) -> AssetId {
        AssetId::new(self.quote())
    }

    /// Get the instrument kind (Spot, Perp, or Outcome).
    pub fn instrument_kind(&self) -> InstrumentKind {
        match self {
            Market::Hyperliquid(hl) => match hl {
                HyperliquidMarket::Outcome { .. } => InstrumentKind::Outcome,
                HyperliquidMarket::Spot { .. } => InstrumentKind::Spot,
                _ => InstrumentKind::Perp,
            },
        }
    }

    /// Check if this is a prediction market (outcome).
    pub fn is_outcome(&self) -> bool {
        match self {
            Market::Hyperliquid(hl) => hl.is_outcome(),
        }
    }

    /// Get outcome parameters (outcome_id, side, name) if this is an Outcome market.
    pub fn outcome_params(&self) -> Option<(u32, u8, String)> {
        match self {
            Market::Hyperliquid(HyperliquidMarket::Outcome {
                outcome_id,
                side,
                name,
                ..
            }) => Some((*outcome_id, *side, name.clone())),
            _ => None,
        }
    }

    /// Get the exchange instance for this market.
    pub fn exchange_instance(&self, environment: Environment) -> ExchangeInstance {
        match self {
            Market::Hyperliquid(_) => {
                ExchangeInstance::new(ExchangeId::new("hyperliquid"), environment)
            }
        }
    }

    /// Get the effective asset ID for order placement (Hyperliquid-specific).
    pub fn effective_asset_id(&self) -> u32 {
        match self {
            Market::Hyperliquid(hl) => hl.effective_asset_id(),
        }
    }

    /// Get the spot coin name for alias resolution (e.g., "@107" -> "HYPE").
    pub fn spot_coin(&self) -> Option<String> {
        match self {
            Market::Hyperliquid(hl) => hl.spot_coin(),
        }
    }

    /// Get the spot market index for price lookups (e.g., 10107 for HYPE).
    pub fn spot_market_index(&self) -> Option<u32> {
        match self {
            Market::Hyperliquid(hl) => hl.spot_market_index(),
        }
    }

    /// Get HIP-3 configuration if this is a HIP-3 market.
    pub fn hip3_config(&self) -> Option<Hip3MarketConfig> {
        match self {
            Market::Hyperliquid(hl) => hl.hip3_config(),
        }
    }

    /// Get instrument metadata if configured.
    pub fn instrument_meta(&self) -> Option<&InstrumentMetaConfig> {
        match self {
            Market::Hyperliquid(hl) => hl.instrument_meta(),
        }
    }
}

// =============================================================================
// Hyperliquid Markets
// =============================================================================

/// Hyperliquid market types — Perp, Spot, HIP-3, or Outcome.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum HyperliquidMarket {
    /// Standard perpetual contract on main Hyperliquid.
    #[serde(rename = "perp")]
    Perp {
        /// Base asset (e.g., "BTC", "ETH")
        base: String,
        /// Quote asset (always "USDC" for main perps)
        #[serde(default = "default_usdc")]
        quote: String,
        /// Asset index on Hyperliquid (e.g., 0 for BTC)
        index: u32,
        /// Instrument metadata (tick/lot sizes)
        #[serde(default)]
        instrument_meta: Option<InstrumentMetaConfig>,
    },

    /// Spot market on Hyperliquid.
    #[serde(rename = "spot")]
    Spot {
        /// Base asset (e.g., "HYPE", "PURR")
        base: String,
        /// Quote asset (e.g., "USDC")
        #[serde(default = "default_usdc")]
        quote: String,
        /// Spot market index (e.g., 10107 for HYPE/USDC)
        index: u32,
        /// Instrument metadata (tick/lot sizes)
        #[serde(default)]
        instrument_meta: Option<InstrumentMetaConfig>,
    },

    /// HIP-3 builder-deployed perpetual DEX.
    #[serde(rename = "hip3")]
    Hip3 {
        /// Base asset (e.g., "BTC", "HFUN")
        base: String,
        /// Quote currency (e.g., "USDC", "USDE", "USDH")
        quote: String,
        /// DEX name (e.g., "hyna", "hypurrfun")
        dex: String,
        /// DEX index in perpDexs() array (starts at 1)
        dex_index: u32,
        /// Asset index within this DEX's meta.universe
        asset_index: u32,
        /// Instrument metadata (tick/lot sizes)
        #[serde(default)]
        instrument_meta: Option<InstrumentMetaConfig>,
    },

    /// Prediction market outcome (testnet-only).
    ///
    /// Outcomes trade like spot but use asset ID = 100_000_000 + encoding,
    /// where encoding = 10 * outcome_id + side.
    #[serde(rename = "outcome")]
    Outcome {
        /// Human-readable name (e.g., "BTC > 69070")
        name: String,
        /// Outcome ID from outcomeMeta (e.g., 516)
        outcome_id: u32,
        /// Side: 0 = Yes, 1 = No
        side: u8,
        /// Instrument metadata (tick/lot sizes)
        #[serde(default)]
        instrument_meta: Option<InstrumentMetaConfig>,
    },
}

fn default_usdc() -> String {
    "USDC".to_string()
}

impl HyperliquidMarket {
    /// Calculate the encoding for an outcome: `10 * outcome_id + side`.
    pub fn outcome_encoding(outcome_id: u32, side: u8) -> u32 {
        10 * outcome_id + side as u32
    }

    /// Get the canonical instrument ID.
    pub fn instrument_id(&self) -> InstrumentId {
        match self {
            Self::Perp { base, .. } => InstrumentId::new(format!("{}-PERP", base)),
            Self::Spot { base, .. } => InstrumentId::new(format!("{}-SPOT", base)),
            Self::Hip3 { dex, base, .. } => InstrumentId::new(format!("{}:{}-PERP", dex, base)),
            Self::Outcome {
                outcome_id, side, ..
            } => {
                let encoding = Self::outcome_encoding(*outcome_id, *side);
                InstrumentId::new(format!("#{}-OUTCOME", encoding))
            }
        }
    }

    /// Get the market index.
    pub fn market_index(&self) -> MarketIndex {
        match self {
            Self::Perp { index, .. } => MarketIndex::new(*index),
            Self::Spot { index, .. } => MarketIndex::new(*index),
            Self::Hip3 {
                dex_index,
                asset_index,
                ..
            } => {
                // HIP-3 effective market index for internal routing
                MarketIndex::new(self.calculate_hip3_asset_id(*dex_index, *asset_index))
            }
            Self::Outcome {
                outcome_id, side, ..
            } => MarketIndex::new(100_000_000 + Self::outcome_encoding(*outcome_id, *side)),
        }
    }

    /// Check if this is a spot market.
    pub fn is_spot(&self) -> bool {
        matches!(self, Self::Spot { .. })
    }

    /// Check if this is a prediction market outcome.
    pub fn is_outcome(&self) -> bool {
        matches!(self, Self::Outcome { .. })
    }

    /// Get the base asset.
    pub fn base(&self) -> &str {
        match self {
            Self::Perp { base, .. } => base,
            Self::Spot { base, .. } => base,
            Self::Hip3 { base, .. } => base,
            Self::Outcome { name, .. } => name,
        }
    }

    /// Get the quote asset.
    pub fn quote(&self) -> &str {
        match self {
            Self::Perp { quote, .. } => quote,
            Self::Spot { quote, .. } => quote,
            Self::Hip3 { quote, .. } => quote,
            Self::Outcome { .. } => "USDC",
        }
    }

    /// Get the effective asset ID for order placement.
    ///
    /// - Perp: returns the index directly
    /// - Spot: returns the index directly (e.g., 10107)
    /// - Hip3: calculates 110000 + ((dex_index-1) * 10000) + asset_index
    /// - Outcome: returns 100_000_000 + encoding
    pub fn effective_asset_id(&self) -> u32 {
        match self {
            Self::Perp { index, .. } => *index,
            Self::Spot { index, .. } => *index,
            Self::Hip3 {
                dex_index,
                asset_index,
                ..
            } => self.calculate_hip3_asset_id(*dex_index, *asset_index),
            Self::Outcome {
                outcome_id, side, ..
            } => 100_000_000 + Self::outcome_encoding(*outcome_id, *side),
        }
    }

    /// Get the coin name used in allMids/fills lookups.
    ///
    /// - Outcome: `#<encoding>` (e.g., `#5160`)
    /// - Spot: base coin name (e.g., `HYPE`)
    /// - Others: None (uses standard name)
    pub fn coin_name(&self) -> Option<String> {
        match self {
            Self::Outcome {
                outcome_id, side, ..
            } => {
                let encoding = Self::outcome_encoding(*outcome_id, *side);
                Some(format!("#{}", encoding))
            }
            Self::Spot { base, .. } => Some(base.clone()),
            _ => None,
        }
    }

    /// Get the spot coin name for @tokenId alias resolution.
    pub fn spot_coin(&self) -> Option<String> {
        match self {
            Self::Spot { base, .. } => Some(base.clone()),
            _ => None,
        }
    }

    /// Get the spot market index for price lookups.
    pub fn spot_market_index(&self) -> Option<u32> {
        match self {
            Self::Spot { index, .. } => Some(*index),
            _ => None,
        }
    }

    /// Get HIP-3 configuration if applicable.
    pub fn hip3_config(&self) -> Option<Hip3MarketConfig> {
        match self {
            Self::Hip3 {
                dex,
                dex_index,
                quote,
                asset_index,
                ..
            } => Some(Hip3MarketConfig {
                dex_name: dex.clone(),
                dex_index: *dex_index,
                quote_currency: quote.clone(),
                asset_index: *asset_index,
            }),
            _ => None,
        }
    }

    /// Check if this HIP-3 market uses non-USDC collateral.
    pub fn uses_alternate_collateral(&self) -> bool {
        match self {
            Self::Hip3 { quote, .. } => quote.to_uppercase() != "USDC",
            _ => false,
        }
    }

    /// Get the DEX name for API calls (None for non-HIP3).
    pub fn dex_name(&self) -> Option<&str> {
        match self {
            Self::Hip3 { dex, .. } => Some(dex.as_str()),
            _ => None,
        }
    }

    // Private helper for HIP-3 asset ID calculation
    fn calculate_hip3_asset_id(&self, dex_index: u32, asset_index: u32) -> u32 {
        110_000 + (dex_index.saturating_sub(1) * 10_000) + asset_index
    }

    /// Get instrument metadata if configured.
    pub fn instrument_meta(&self) -> Option<&InstrumentMetaConfig> {
        match self {
            Self::Perp {
                instrument_meta, ..
            } => instrument_meta.as_ref(),
            Self::Spot {
                instrument_meta, ..
            } => instrument_meta.as_ref(),
            Self::Hip3 {
                instrument_meta, ..
            } => instrument_meta.as_ref(),
            Self::Outcome {
                instrument_meta, ..
            } => instrument_meta.as_ref(),
        }
    }
}

// =============================================================================
// HIP-3 Config (for exchange client compatibility)
// =============================================================================

/// HIP-3 configuration extracted from Market enum.
///
/// This struct provides compatibility with existing `HyperliquidClient`
/// which expects `hip3: Option<Hip3Config>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hip3MarketConfig {
    pub dex_name: String,
    pub dex_index: u32,
    pub quote_currency: String,
    pub asset_index: u32,
}

impl Hip3MarketConfig {
    /// Calculate the HIP-3 asset ID for order placement.
    pub fn calculate_asset_id(&self) -> u32 {
        110_000 + (self.dex_index.saturating_sub(1) * 10_000) + self.asset_index
    }

    /// Check if this DEX uses non-USDC collateral.
    pub fn uses_alternate_collateral(&self) -> bool {
        self.quote_currency.to_uppercase() != "USDC"
    }
}

// =============================================================================
// InstrumentMeta Builder
// =============================================================================

/// Configuration for instrument metadata (tick/lot sizes).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InstrumentMetaConfig {
    /// Tick size for price rounding
    pub tick_size: Decimal,
    /// Lot size for quantity rounding
    pub lot_size: Decimal,
    /// Minimum quantity
    #[serde(default)]
    pub min_qty: Option<Decimal>,
    /// Minimum notional value
    #[serde(default)]
    pub min_notional: Option<Decimal>,
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_perp_market_instrument_id() {
        let market = HyperliquidMarket::Perp {
            base: "BTC".to_string(),
            quote: "USDC".to_string(),
            index: 0,
            instrument_meta: None,
        };
        assert_eq!(market.instrument_id().as_str(), "BTC-PERP");
        assert_eq!(market.market_index().value(), 0);
        assert!(!market.is_spot());
        assert!(!market.is_outcome());
        assert_eq!(market.effective_asset_id(), 0);
    }

    #[test]
    fn test_spot_market_instrument_id() {
        let market = HyperliquidMarket::Spot {
            base: "HYPE".to_string(),
            quote: "USDC".to_string(),
            index: 10107,
            instrument_meta: None,
        };
        assert_eq!(market.instrument_id().as_str(), "HYPE-SPOT");
        assert_eq!(market.market_index().value(), 10107);
        assert!(market.is_spot());
        assert!(!market.is_outcome());
        assert_eq!(market.spot_coin(), Some("HYPE".to_string()));
        assert_eq!(market.spot_market_index(), Some(10107));
    }

    #[test]
    fn test_hip3_market_instrument_id() {
        let market = HyperliquidMarket::Hip3 {
            base: "BTC".to_string(),
            quote: "USDE".to_string(),
            dex: "hyna".to_string(),
            dex_index: 4,
            asset_index: 1,
            instrument_meta: None,
        };
        assert_eq!(market.instrument_id().as_str(), "hyna:BTC-PERP");
        // HIP-3 asset ID: 110000 + (4-1)*10000 + 1 = 140001
        assert_eq!(market.effective_asset_id(), 140001);
        assert!(!market.is_spot());
        assert!(!market.is_outcome());
        assert!(market.uses_alternate_collateral());
        assert_eq!(market.dex_name(), Some("hyna"));
    }

    #[test]
    fn test_hip3_asset_id_calculation() {
        // DEX index 1, asset index 0 → 110000
        let market = HyperliquidMarket::Hip3 {
            base: "TEST".to_string(),
            quote: "USDC".to_string(),
            dex: "test".to_string(),
            dex_index: 1,
            asset_index: 0,
            instrument_meta: None,
        };
        assert_eq!(market.effective_asset_id(), 110000);

        // DEX index 4, asset index 1 → 140001
        let market2 = HyperliquidMarket::Hip3 {
            base: "TEST".to_string(),
            quote: "USDC".to_string(),
            dex: "test".to_string(),
            dex_index: 4,
            asset_index: 1,
            instrument_meta: None,
        };
        assert_eq!(market2.effective_asset_id(), 140001);
    }

    #[test]
    fn test_market_enum_serde() {
        let json = r#"{
            "exchange": "hyperliquid",
            "type": "perp",
            "base": "BTC",
            "quote": "USDC",
            "index": 0
        }"#;

        let market: Market = serde_json::from_str(json).unwrap();
        assert_eq!(market.instrument_id().as_str(), "BTC-PERP");
        assert!(!market.is_spot());
        assert!(!market.is_outcome());
    }

    #[test]
    fn test_hip3_market_serde() {
        let json = r#"{
            "exchange": "hyperliquid",
            "type": "hip3",
            "base": "HFUN",
            "quote": "USDC",
            "dex": "hypurrfun",
            "dex_index": 5,
            "asset_index": 0
        }"#;

        let market: Market = serde_json::from_str(json).unwrap();
        assert_eq!(market.instrument_id().as_str(), "hypurrfun:HFUN-PERP");

        let hip3 = market.hip3_config().unwrap();
        assert_eq!(hip3.dex_name, "hypurrfun");
        assert_eq!(hip3.dex_index, 5);
    }

    // =========================================================================
    // Outcome tests
    // =========================================================================

    #[test]
    fn test_outcome_encoding() {
        // outcome 516, side 0 (Yes) → encoding 5160
        assert_eq!(HyperliquidMarket::outcome_encoding(516, 0), 5160);
        // outcome 516, side 1 (No) → encoding 5161
        assert_eq!(HyperliquidMarket::outcome_encoding(516, 1), 5161);
        // outcome 9, side 0 → encoding 90
        assert_eq!(HyperliquidMarket::outcome_encoding(9, 0), 90);
        // outcome 10, side 1 → encoding 101
        assert_eq!(HyperliquidMarket::outcome_encoding(10, 1), 101);
    }

    #[test]
    fn test_outcome_market_instrument_id() {
        let market = HyperliquidMarket::Outcome {
            name: "BTC > 69070".to_string(),
            outcome_id: 516,
            side: 0,
            instrument_meta: None,
        };
        assert_eq!(market.instrument_id().as_str(), "#5160-OUTCOME");
    }

    #[test]
    fn test_outcome_market_effective_asset_id() {
        // outcome 516, side 0 → asset_id = 100_000_000 + 5160 = 100_005_160
        let yes = HyperliquidMarket::Outcome {
            name: "BTC > 69070".to_string(),
            outcome_id: 516,
            side: 0,
            instrument_meta: None,
        };
        assert_eq!(yes.effective_asset_id(), 100_005_160);

        // outcome 516, side 1 → asset_id = 100_000_000 + 5161 = 100_005_161
        let no = HyperliquidMarket::Outcome {
            name: "BTC > 69070".to_string(),
            outcome_id: 516,
            side: 1,
            instrument_meta: None,
        };
        assert_eq!(no.effective_asset_id(), 100_005_161);
    }

    #[test]
    fn test_outcome_market_coin_name() {
        let market = HyperliquidMarket::Outcome {
            name: "BTC > 69070".to_string(),
            outcome_id: 516,
            side: 0,
            instrument_meta: None,
        };
        assert_eq!(market.coin_name(), Some("#5160".to_string()));
    }

    #[test]
    fn test_outcome_market_properties() {
        let market = HyperliquidMarket::Outcome {
            name: "HYPE > 200".to_string(),
            outcome_id: 686,
            side: 1,
            instrument_meta: None,
        };
        assert!(market.is_outcome());
        assert!(!market.is_spot());
        assert_eq!(market.base(), "HYPE > 200");
        assert_eq!(market.quote(), "USDC");
        assert_eq!(market.spot_coin(), None);
        assert_eq!(market.spot_market_index(), None);
        assert_eq!(market.hip3_config(), None);
    }

    #[test]
    fn test_outcome_market_serde() {
        let json = r#"{
            "exchange": "hyperliquid",
            "type": "outcome",
            "name": "BTC > 69070",
            "outcome_id": 516,
            "side": 0
        }"#;

        let market: Market = serde_json::from_str(json).unwrap();
        assert_eq!(market.instrument_id().as_str(), "#5160-OUTCOME");
        assert!(market.is_outcome());
        assert!(!market.is_spot());
        assert_eq!(market.instrument_kind(), InstrumentKind::Outcome);
    }

    #[test]
    fn test_outcome_market_kind() {
        let outcome = Market::Hyperliquid(HyperliquidMarket::Outcome {
            name: "test".to_string(),
            outcome_id: 10,
            side: 0,
            instrument_meta: None,
        });
        assert_eq!(outcome.instrument_kind(), InstrumentKind::Outcome);

        let perp = Market::Hyperliquid(HyperliquidMarket::Perp {
            base: "BTC".to_string(),
            quote: "USDC".to_string(),
            index: 0,
            instrument_meta: None,
        });
        assert_eq!(perp.instrument_kind(), InstrumentKind::Perp);

        let spot = Market::Hyperliquid(HyperliquidMarket::Spot {
            base: "HYPE".to_string(),
            quote: "USDC".to_string(),
            index: 10107,
            instrument_meta: None,
        });
        assert_eq!(spot.instrument_kind(), InstrumentKind::Spot);
    }
}
