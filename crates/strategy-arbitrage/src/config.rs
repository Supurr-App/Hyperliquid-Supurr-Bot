//! Arbitrage strategy configuration.

use bot_core::{Environment, ExchangeInstance, InstrumentId, Market, MarketIndex, StrategyId};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Configuration for the Spot-Perp Arbitrage strategy.
///
/// The strategy monitors the price spread between a spot market and its
/// corresponding perpetual market. When the spread exceeds a threshold,
/// it opens a hedged position (long spot + short perp or vice versa).
/// When the spread converges, it closes both positions for profit.
///
/// Market information (exchange, instrument, index, metadata) is derived
/// from the V2 `markets[]` array — `spot_market` and `perp_market`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArbitrageConfig {
    /// Unique strategy identifier
    pub strategy_id: StrategyId,

    // -------------------------------------------------------------------------
    // Market Configuration (from V2 markets[] array)
    // -------------------------------------------------------------------------
    /// Spot market (markets[0] in V2 config)
    pub spot_market: Market,

    /// Perp market (markets[1] in V2 config)
    pub perp_market: Market,

    /// Environment for exchange instance resolution
    pub environment: Environment,

    // -------------------------------------------------------------------------
    // Position Sizing
    // -------------------------------------------------------------------------
    /// Order amount in quote asset / USDC (e.g., 100 = $100 worth)
    pub order_amount: Decimal,

    /// Leverage for perp position (1 = no leverage)
    pub perp_leverage: Decimal,

    // -------------------------------------------------------------------------
    // Spread Thresholds
    // -------------------------------------------------------------------------
    /// Minimum spread to open a position (e.g., 0.003 = 0.3%)
    /// spread = (perp_price - spot_price) / spot_price
    pub min_opening_spread_pct: Decimal,

    /// Minimum spread to close a position (e.g., -0.001 = -0.1%)
    /// When spread drops to this level, close for profit
    pub min_closing_spread_pct: Decimal,

    // -------------------------------------------------------------------------
    // Slippage Protection
    // -------------------------------------------------------------------------
    /// Slippage buffer for spot market orders (e.g., 0.001 = 0.1%)
    pub spot_slippage_buffer_pct: Decimal,

    /// Slippage buffer for perp market orders (e.g., 0.001 = 0.1%)
    pub perp_slippage_buffer_pct: Decimal,
}

// =============================================================================
// Derived Accessors — single source of truth from Market objects
// =============================================================================

impl ArbitrageConfig {
    /// Spot instrument ID (e.g., "HYPE-SPOT")
    pub fn spot_instrument(&self) -> InstrumentId {
        self.spot_market.instrument_id()
    }

    /// Perp instrument ID (e.g., "HYPE-PERP")
    pub fn perp_instrument(&self) -> InstrumentId {
        self.perp_market.instrument_id()
    }

    /// Spot market index
    pub fn spot_market_index(&self) -> MarketIndex {
        self.spot_market.market_index()
    }

    /// Perp market index
    pub fn perp_market_index(&self) -> MarketIndex {
        self.perp_market.market_index()
    }

    /// Exchange instance for spot market
    pub fn spot_exchange(&self) -> ExchangeInstance {
        self.spot_market.exchange_instance(self.environment)
    }

    /// Exchange instance for perp market
    pub fn perp_exchange(&self) -> ExchangeInstance {
        self.perp_market.exchange_instance(self.environment)
    }

    /// Validate the configuration and return any errors.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        // Market type validation
        if !self.spot_market.is_spot() {
            errors.push("markets[0] must be a spot market".into());
        }
        if self.perp_market.is_spot() {
            errors.push("markets[1] must be a perp market".into());
        }

        // Order amount validation
        if self.order_amount <= Decimal::ZERO {
            errors.push("order_amount must be > 0".into());
        }

        // Spread thresholds validation
        if self.min_opening_spread_pct <= Decimal::ZERO {
            errors.push("min_opening_spread_pct must be > 0".into());
        }

        if self.min_opening_spread_pct <= self.min_closing_spread_pct {
            errors.push(format!(
                "min_opening_spread_pct ({}) must be > min_closing_spread_pct ({})",
                self.min_opening_spread_pct, self.min_closing_spread_pct
            ));
        }

        // Leverage validation
        if self.perp_leverage < Decimal::ONE {
            errors.push("perp_leverage must be >= 1".into());
        }

        // Slippage validation
        if self.spot_slippage_buffer_pct < Decimal::ZERO {
            errors.push("spot_slippage_buffer_pct must be >= 0".into());
        }
        if self.perp_slippage_buffer_pct < Decimal::ZERO {
            errors.push("perp_slippage_buffer_pct must be >= 0".into());
        }

        errors
    }

    /// Calculate the spread between perp and spot prices.
    /// spread = (perp_price - spot_price) / spot_price
    pub fn calculate_spread(&self, spot_price: Decimal, perp_price: Decimal) -> Decimal {
        if spot_price <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        (perp_price - spot_price) / spot_price
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bot_core::HyperliquidMarket;
    use rust_decimal_macros::dec;

    fn test_spot_market() -> Market {
        Market::Hyperliquid(HyperliquidMarket::Spot {
            base: "ETH".to_string(),
            quote: "USDC".to_string(),
            index: 10002,
            instrument_meta: None,
        })
    }

    fn test_perp_market() -> Market {
        Market::Hyperliquid(HyperliquidMarket::Perp {
            base: "ETH".to_string(),
            quote: "USDC".to_string(),
            index: 1,
            instrument_meta: None,
        })
    }

    fn test_config() -> ArbitrageConfig {
        ArbitrageConfig {
            strategy_id: StrategyId::new("test-arb"),
            spot_market: test_spot_market(),
            perp_market: test_perp_market(),
            environment: Environment::Testnet,
            order_amount: dec!(0.1),
            perp_leverage: Decimal::ONE,
            min_opening_spread_pct: dec!(0.003),
            min_closing_spread_pct: dec!(-0.001),
            spot_slippage_buffer_pct: dec!(0.001),
            perp_slippage_buffer_pct: dec!(0.001),
        }
    }

    #[test]
    fn test_default_config_is_valid() {
        let config = test_config();
        let errors = config.validate();
        assert!(
            errors.is_empty(),
            "Test config should be valid: {:?}",
            errors
        );
    }

    #[test]
    fn test_derived_accessors() {
        let config = test_config();
        assert_eq!(config.spot_instrument().as_str(), "ETH-SPOT");
        assert_eq!(config.perp_instrument().as_str(), "ETH-PERP");
        assert_eq!(config.spot_market_index().value(), 10002);
        assert_eq!(config.perp_market_index().value(), 1);
    }

    #[test]
    fn test_invalid_market_types() {
        // Swap spot and perp — should fail validation
        let config = ArbitrageConfig {
            spot_market: test_perp_market(), // wrong: perp in spot slot
            perp_market: test_spot_market(), // wrong: spot in perp slot
            ..test_config()
        };
        let errors = config.validate();
        assert!(errors
            .iter()
            .any(|e| e.contains("markets[0] must be a spot market")));
        assert!(errors
            .iter()
            .any(|e| e.contains("markets[1] must be a perp market")));
    }

    #[test]
    fn test_invalid_order_amount() {
        let mut config = test_config();
        config.order_amount = Decimal::ZERO;
        let errors = config.validate();
        assert!(errors.iter().any(|e| e.contains("order_amount")));
    }

    #[test]
    fn test_invalid_spread_thresholds() {
        let mut config = test_config();
        config.min_opening_spread_pct = dec!(0.001);
        config.min_closing_spread_pct = dec!(0.002); // higher than opening!
        let errors = config.validate();
        assert!(errors.iter().any(|e| e.contains("min_opening_spread_pct")));
    }

    #[test]
    fn test_spread_calculation() {
        let config = test_config();

        // Perp at 1% premium
        let spread = config.calculate_spread(
            Decimal::new(3000, 0), // spot = 3000
            Decimal::new(3030, 0), // perp = 3030
        );
        // (3030 - 3000) / 3000 = 0.01 = 1%
        assert_eq!(spread, dec!(0.01));
    }
}
