//! Market maker configuration.

use bot_core::{
    Environment, ExchangeInstance, HyperliquidMarket, InstrumentId, Market, MarketIndex, StrategyId,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Skew mode for market making
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkewMode {
    /// No skew adjustment
    None,
    /// Only adjust order sizes based on inventory
    Size,
    /// Only adjust order prices based on inventory
    Price,
    /// Adjust both size and price
    Both,
}

impl Default for SkewMode {
    fn default() -> Self {
        Self::Both
    }
}

/// Configuration for the Market Maker strategy.
///
/// IMPORTANT CONSTRAINTS (validated at runtime):
/// - max_position_size: Must be > 0 (division by zero protection)
/// - size_skew_floor: Must be in (0, 1] - 0.2 means orders never go below 20% size
/// - price_skew_gamma: Must be in [0, 0.5] - higher values risk negative prices
/// - target_position_pct: Must be in [0, 1] - 0.5 = neutral target
/// - base_spread: Must be > 0 - prevents BUY/SELL at same price
/// - base_order_size: Must be > 0 - prevents zero-size orders
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketMakerConfig {
    /// Unique strategy identifier
    pub strategy_id: StrategyId,

    // =========================================================================
    // Unified Market Configuration (replaces exchange, instrument, market_index, is_spot)
    // =========================================================================
    /// Trading environment (mainnet/testnet)
    pub environment: Environment,

    /// Unified market configuration - single source of truth
    pub market: Market,

    /// Base order size
    pub base_order_size: Decimal,

    /// Base spread as a fraction (0.001 = 0.1%)
    pub base_spread: Decimal,

    // -------------------------------------------------------------------------
    // Inventory management
    // -------------------------------------------------------------------------
    /// Target position as fraction of max (0.5 = neutral)
    pub target_position_pct: Decimal,

    /// Minimum position as fraction of max
    pub min_position_pct: Decimal,

    /// Maximum position as fraction of max
    pub max_position_pct: Decimal,

    /// Maximum position size (absolute, used for skew calculations)
    pub max_position_size: Decimal,

    // -------------------------------------------------------------------------
    // Skew configuration
    // -------------------------------------------------------------------------
    /// Skew mode
    pub skew_mode: SkewMode,

    /// Price skew gamma (max recommended: 0.5)
    /// Higher values = more aggressive price adjustment
    pub price_skew_gamma: Decimal,

    /// Size skew floor (0.2 = orders never go below 20% of base size)
    pub size_skew_floor: Decimal,

    // -------------------------------------------------------------------------
    // Refresh triggers
    // -------------------------------------------------------------------------
    /// Minimum price change to trigger order refresh (0.0005 = 0.05%)
    pub min_price_change_pct: Decimal,

    // -------------------------------------------------------------------------
    // PnL-based exit
    // -------------------------------------------------------------------------
    /// Stop loss threshold (optional)
    pub stop_loss: Option<Decimal>,

    /// Take profit threshold (optional)
    pub take_profit: Option<Decimal>,
}

impl MarketMakerConfig {
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

        // Division by zero protection
        if self.max_position_size <= Decimal::ZERO {
            errors.push("max_position_size must be > 0 (division by zero)".into());
        }

        // Spread validation
        if self.base_spread <= Decimal::ZERO {
            errors.push("base_spread must be > 0 (prevents BUY/SELL at same price)".into());
        }

        // Order size validation
        if self.base_order_size <= Decimal::ZERO {
            errors.push("base_order_size must be > 0".into());
        }

        // Size skew floor validation
        if self.size_skew_floor <= Decimal::ZERO || self.size_skew_floor > Decimal::ONE {
            errors.push("size_skew_floor must be in (0, 1] range".into());
        }

        // Price skew gamma validation (prevent negative prices)
        // Max safe gamma: when imbalance=1, price_skew=gamma
        // Buy price = mid * (1 - spread - gamma) > 0
        // Requires: 1 - spread - gamma > 0, so gamma < 1 - spread
        let max_safe_gamma = Decimal::ONE - self.base_spread - Decimal::new(1, 2); // 1% safety margin
        if self.price_skew_gamma < Decimal::ZERO {
            errors.push("price_skew_gamma must be >= 0".into());
        } else if self.price_skew_gamma > max_safe_gamma {
            errors.push(format!(
                "price_skew_gamma={} too high! Max safe value: {} (prevents negative prices)",
                self.price_skew_gamma, max_safe_gamma
            ));
        }

        // Target position validation
        if self.target_position_pct < Decimal::ZERO || self.target_position_pct > Decimal::ONE {
            errors.push("target_position_pct must be in [0, 1] range".into());
        }

        if self.min_position_pct < Decimal::ZERO || self.min_position_pct > Decimal::ONE {
            errors.push("min_position_pct must be in [0, 1] range".into());
        }

        if self.max_position_pct < Decimal::ZERO || self.max_position_pct > Decimal::ONE {
            errors.push("max_position_pct must be in [0, 1] range".into());
        }

        if self.min_position_pct >= self.max_position_pct {
            errors.push("min_position_pct must be < max_position_pct".into());
        }

        errors
    }
}

impl Default for MarketMakerConfig {
    fn default() -> Self {
        Self {
            strategy_id: StrategyId::new("market-maker-default"),
            environment: Environment::Testnet,
            market: Market::Hyperliquid(HyperliquidMarket::Perp {
                base: "BTC".to_string(),
                quote: "USDC".to_string(),
                index: 0,
                instrument_meta: None,
            }),
            base_order_size: Decimal::new(1, 3), // 0.001
            base_spread: Decimal::new(1, 3),     // 0.001 = 0.1%

            target_position_pct: Decimal::new(5, 1), // 0.5
            min_position_pct: Decimal::new(1, 1),    // 0.1
            max_position_pct: Decimal::new(9, 1),    // 0.9
            max_position_size: Decimal::ONE,

            skew_mode: SkewMode::Both,
            price_skew_gamma: Decimal::new(5, 2), // 0.05
            size_skew_floor: Decimal::new(2, 1),  // 0.2

            min_price_change_pct: Decimal::new(5, 4), // 0.0005 = 0.05%

            stop_loss: None,
            take_profit: None,
        }
    }
}
