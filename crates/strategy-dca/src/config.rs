//! DCA strategy configuration.

use bot_core::{
    Environment, ExchangeInstance, HyperliquidMarket, InstrumentId, Market, MarketIndex, StrategyId,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// DCA direction determines the trading bias.
///
/// - **LONG**: Buy on price drops (average down), take profit above average entry.
///   Each DCA trigger is BELOW the previous trigger.
///
/// - **SHORT**: Sell on price rises (average up), take profit below average entry.
///   Each DCA trigger is ABOVE the previous trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DCADirection {
    /// Long: Buy at each DCA level, sell to take profit
    Long,
    /// Short: Sell at each DCA level, buy to take profit  
    Short,
}

impl Default for DCADirection {
    fn default() -> Self {
        Self::Long
    }
}

/// Configuration for the DCA trading strategy.
///
/// ## DCA Ladder Calculation
///
/// Trigger prices are calculated as:
/// - LONG: `trigger[i] = trigger[i-1] × (1 - deviation_pct × deviation_multiplier^(i-1))`
/// - SHORT: `trigger[i] = trigger[i-1] × (1 + deviation_pct × deviation_multiplier^(i-1))`
///
/// Order sizes are calculated as:
/// - `size[0] = base_order_size`
/// - `size[i] = dca_order_size × size_multiplier^(i-1)` for i > 0
///
/// ## Take Profit Calculation
///
/// - LONG: `tp_price = average_entry × (1 + take_profit_pct / 100)`
/// - SHORT: `tp_price = average_entry × (1 - take_profit_pct / 100)`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DCAConfig {
    /// Unique strategy identifier
    pub strategy_id: StrategyId,

    /// Trading environment
    pub environment: Environment,

    /// Market to trade on (contains exchange, instrument, market index, and spot/perp type)
    pub market: Market,

    // -------------------------------------------------------------------------
    // Direction & Entry
    // -------------------------------------------------------------------------
    /// DCA direction: Long or Short
    pub direction: DCADirection,

    /// Price to trigger the initial base order
    /// For LONG: typically at or below current price
    /// For SHORT: typically at or above current price
    pub trigger_price: Decimal,

    // -------------------------------------------------------------------------
    // Order Sizing
    // -------------------------------------------------------------------------
    /// Size of the initial base order (in base asset units)
    pub base_order_size: Decimal,

    /// Size of each DCA order (before multiplier)
    pub dca_order_size: Decimal,

    /// Maximum number of DCA orders (excluding base order)
    /// Total orders = 1 (base) + max_dca_orders
    pub max_dca_orders: u32,

    /// Multiplier for subsequent DCA order sizes
    /// e.g., 2.0 means each DCA order is 2x the previous
    pub size_multiplier: Decimal,

    // -------------------------------------------------------------------------
    // Price Deviation
    // -------------------------------------------------------------------------
    /// Percentage price change to trigger the first DCA order
    /// e.g., 1.0 = 1% deviation from trigger price
    pub price_deviation_pct: Decimal,

    /// Multiplier for deviation percentage on subsequent orders
    /// e.g., 1.5 means second DCA triggers at 1.5% from first, third at 2.25%, etc.
    pub deviation_multiplier: Decimal,

    // -------------------------------------------------------------------------
    // Take Profit & Stop Loss
    // -------------------------------------------------------------------------
    /// Take profit percentage from average entry price
    /// e.g., 2.0 = close position when price is 2% above/below average entry
    pub take_profit_pct: Decimal,

    /// Optional stop loss as absolute PnL threshold (not percentage)
    /// e.g., -100 = close strategy when unrealized PnL drops below -$100
    /// When current_pnl < stop_loss → Strategy stops (like liquidation)
    pub stop_loss: Option<Decimal>,

    // -------------------------------------------------------------------------
    // Leverage & Position Sizing
    // -------------------------------------------------------------------------
    /// Leverage to use (1 = spot-like, 10 = 10x leverage)
    pub leverage: Decimal,

    /// Maximum leverage allowed by the exchange
    pub max_leverage: Decimal,

    // -------------------------------------------------------------------------
    // Behavior Settings
    // -------------------------------------------------------------------------
    /// Whether to restart the cycle after take profit
    pub restart_on_complete: bool,

    /// Cooldown period in seconds between cycles (when restart_on_complete is true)
    /// Default: 60 seconds (like Binance)
    pub cooldown_period_secs: u64,
}

impl DCAConfig {
    // =========================================================================
    // Market Accessors - derived from the unified Market enum
    // =========================================================================

    /// Returns true if this is a spot market
    pub fn is_spot(&self) -> bool {
        self.market.is_spot()
    }

    /// Returns the instrument ID derived from the market
    pub fn instrument_id(&self) -> InstrumentId {
        self.market.instrument_id()
    }

    /// Returns the exchange instance for this config
    pub fn exchange_instance(&self) -> ExchangeInstance {
        self.market.exchange_instance(self.environment)
    }

    /// Returns the market index
    pub fn market_index(&self) -> MarketIndex {
        self.market.market_index()
    }

    /// Validate the configuration and return any errors.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        // Trigger price validation
        if self.trigger_price <= Decimal::ZERO {
            errors.push("trigger_price must be > 0".into());
        }

        // Order size validation
        if self.base_order_size <= Decimal::ZERO {
            errors.push("base_order_size must be > 0".into());
        }
        if self.dca_order_size <= Decimal::ZERO {
            errors.push("dca_order_size must be > 0".into());
        }

        // DCA count validation
        if self.max_dca_orders == 0 {
            errors.push("max_dca_orders must be >= 1".into());
        }

        // Deviation validation
        if self.price_deviation_pct <= Decimal::ZERO {
            errors.push("price_deviation_pct must be > 0".into());
        }
        if self.price_deviation_pct > Decimal::new(50, 0) {
            errors.push("price_deviation_pct seems too high (> 50%)".into());
        }

        // Multiplier validation
        if self.deviation_multiplier <= Decimal::ZERO {
            errors.push("deviation_multiplier must be > 0".into());
        }
        if self.size_multiplier <= Decimal::ZERO {
            errors.push("size_multiplier must be > 0".into());
        }

        // Take profit validation
        if self.take_profit_pct <= Decimal::ZERO {
            errors.push("take_profit_pct must be > 0".into());
        }

        // Stop loss validation (if set) - must be negative (absolute PnL threshold)
        if let Some(sl) = self.stop_loss {
            if sl >= Decimal::ZERO {
                errors
                    .push("stop_loss must be negative (e.g., -100 for $100 loss threshold)".into());
            }
        }

        // Leverage validation (skip for spot)
        if !self.is_spot() {
            if self.leverage <= Decimal::ZERO {
                errors.push("leverage must be > 0".into());
            }
            if self.leverage > self.max_leverage {
                errors.push(format!(
                    "leverage ({}) exceeds max_leverage ({})",
                    self.leverage, self.max_leverage
                ));
            }
        }

        errors
    }

    /// Calculate total maximum investment (all orders at full size).
    /// This helps validate that the user has sufficient margin.
    pub fn max_total_investment(&self) -> Decimal {
        let mut total = self.base_order_size * self.trigger_price;

        let mut current_size = self.dca_order_size;
        let mut current_price = self.trigger_price;
        let mut deviation_factor = Decimal::ONE;
        let hundred = Decimal::new(100, 0);

        for _ in 0..self.max_dca_orders {
            // Calculate deviation for this level
            let deviation = self.price_deviation_pct * deviation_factor;

            // Calculate trigger price
            current_price = match self.direction {
                DCADirection::Long => current_price * (Decimal::ONE - deviation / hundred),
                DCADirection::Short => current_price * (Decimal::ONE + deviation / hundred),
            };

            // Add to total
            total += current_size * current_price;

            // Multiply factors for next iteration
            current_size *= self.size_multiplier;
            deviation_factor *= self.deviation_multiplier;
        }

        total
    }
}

impl Default for DCAConfig {
    fn default() -> Self {
        Self {
            strategy_id: StrategyId::new("dca-default"),
            environment: Environment::Testnet,
            market: Market::Hyperliquid(HyperliquidMarket::Perp {
                base: "BTC".to_string(),
                quote: "USDC".to_string(),
                index: 0,
                instrument_meta: None,
            }),

            direction: DCADirection::Long,
            trigger_price: Decimal::new(95000, 0),

            base_order_size: Decimal::new(1, 3), // 0.001
            dca_order_size: Decimal::new(2, 3),  // 0.002
            max_dca_orders: 5,
            size_multiplier: Decimal::new(2, 0), // 2x

            price_deviation_pct: Decimal::new(1, 0),   // 1%
            deviation_multiplier: Decimal::new(15, 1), // 1.5x

            take_profit_pct: Decimal::new(2, 0),    // 2%
            stop_loss: Some(Decimal::new(-100, 0)), // -$100 PnL threshold

            leverage: Decimal::new(5, 0),
            max_leverage: Decimal::new(50, 0),

            restart_on_complete: false,
            cooldown_period_secs: 60, // 60 seconds like Binance
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_is_valid() {
        let config = DCAConfig::default();
        let errors = config.validate();
        assert!(
            errors.is_empty(),
            "Default config should be valid: {:?}",
            errors
        );
    }

    #[test]
    fn test_invalid_trigger_price() {
        let mut config = DCAConfig::default();
        config.trigger_price = Decimal::ZERO;
        let errors = config.validate();
        assert!(errors.iter().any(|e| e.contains("trigger_price")));
    }

    #[test]
    fn test_invalid_deviation() {
        let mut config = DCAConfig::default();
        config.price_deviation_pct = Decimal::new(60, 0); // 60% - too high
        let errors = config.validate();
        assert!(errors.iter().any(|e| e.contains("deviation_pct")));
    }

    #[test]
    fn test_invalid_stop_loss() {
        let mut config = DCAConfig::default();
        config.stop_loss = Some(Decimal::new(5, 0)); // Positive - should be negative
        let errors = config.validate();
        assert!(errors.iter().any(|e| e.contains("stop_loss")));
    }

    #[test]
    fn test_max_investment_calculation() {
        let config = DCAConfig::default();
        let max_inv = config.max_total_investment();
        // Should be positive and reasonable
        assert!(max_inv > Decimal::ZERO);
        assert!(max_inv < Decimal::new(1_000_000, 0)); // Less than 1M
    }
}
