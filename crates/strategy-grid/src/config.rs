//! Grid strategy configuration.

use bot_core::{
    Environment, ExchangeInstance, HyperliquidMarket, InstrumentId, Market, MarketIndex, StrategyId,
};
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Grid mode determines the trading direction.
///
/// - **LONG**: All levels are BUY orders. Take profit is SELL above entry.
///   Grid levels go from low (start_price) to high (end_price).
///   Ideal for bullish markets where you expect price to stay within range.
///
/// - **SHORT**: All levels are SELL orders. Take profit is BUY below entry.
///   Grid levels go from high (start_price) to low (end_price).
///   Ideal for bearish markets.
///
/// - **NEUTRAL**: Mixed mode. Levels below current price are BUY (long).
///   Levels above current price are SELL (short). Center level is inactive.
///   Ideal for ranging markets with no clear direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum GridMode {
    /// Long grid: BUY at each level, SELL to take profit
    Long,
    /// Short grid: SELL at each level, BUY to take profit
    Short,
    /// Neutral grid: BUY below mid, SELL above mid
    Neutral,
}

impl Default for GridMode {
    fn default() -> Self {
        Self::Long
    }
}

/// Configuration for the Grid trading strategy.
///
/// The grid creates `grid_levels` evenly spaced price levels between
/// `start_price` and `end_price`. At each level:
/// - An "open" order is placed at the entry price
/// - When filled, a "close" order is placed at the take profit price
/// - When the close order fills, the cycle repeats
///
/// ## Investment Calculation
///
/// Total notional budget = `max_investment_quote` × `leverage`
/// Quote per level = notional budget / (grid_levels - 1)
/// Quantity per level = quote per level / entry price
///
/// ## Safety Checks
///
/// - Validates that liquidation prices are outside the grid range
/// - Validates minimum quote per level (must be > 20 for most exchanges)
/// - Validates that leverage doesn't exceed max_leverage
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GridConfig {
    /// Unique strategy identifier
    pub strategy_id: StrategyId,

    /// Trading environment (mainnet/testnet)
    pub environment: Environment,

    /// Market to trade on — single source of truth for exchange/instrument/market_index/is_spot
    pub market: Market,

    // -------------------------------------------------------------------------
    // Grid Structure
    // -------------------------------------------------------------------------
    /// Grid mode: Long, Short, or Neutral
    pub grid_mode: GridMode,

    /// Number of grid levels (including start and end)
    /// Minimum: 2, Recommended: 10-50
    pub grid_levels: u32,

    /// Starting price of the grid (lower bound for LONG, upper bound for SHORT)
    pub start_price: Decimal,

    /// Ending price of the grid (upper bound for LONG, lower bound for SHORT)
    pub end_price: Decimal,

    // -------------------------------------------------------------------------
    // Position Sizing
    // -------------------------------------------------------------------------
    /// Maximum investment in quote currency (e.g., USDC)
    /// This is the initial margin, not the total notional.
    /// Quantity per level = (max_investment_quote × leverage) / (levels - 1) / price
    pub max_investment_quote: Decimal,

    /// Fallback order size (only used if quote-based calculation fails)
    /// You typically don't need to set this - it's derived from instrument min_qty
    pub base_order_size: Decimal,

    /// Leverage to use (1 = spot-like, 10 = 10x leverage)
    pub leverage: Decimal,

    /// Maximum leverage allowed by the exchange (for liquidation calculations)
    /// Typically 2x the maintenance leverage
    pub max_leverage: Decimal,

    // -------------------------------------------------------------------------
    // Order Settings
    // -------------------------------------------------------------------------
    /// Use post-only orders (maker only, rejected if would take)
    pub post_only: bool,

    // -------------------------------------------------------------------------
    // PnL-based Exit
    // -------------------------------------------------------------------------
    /// Stop loss threshold (optional) - stop when PnL drops below this
    pub stop_loss: Option<Decimal>,

    /// Take profit threshold (optional) - stop when PnL exceeds this
    pub take_profit: Option<Decimal>,

    // -------------------------------------------------------------------------
    // Trailing Grid (Dynamic Window Sliding)
    // -------------------------------------------------------------------------
    /// Hard ceiling for upward trailing.
    ///
    /// When `Some`, the grid slides up whenever price breaks above the window,
    /// stopping once the new top would exceed this value.
    /// `None` disables upward trailing entirely.
    pub trailing_up_limit: Option<Decimal>,

    /// Hard floor for downward trailing.
    ///
    /// When `Some`, the grid slides down whenever price breaks below the window,
    /// stopping once the new bottom would go below this value.
    /// `None` disables downward trailing entirely.
    pub trailing_down_limit: Option<Decimal>,
}

impl GridConfig {
    // -------------------------------------------------------------------------
    // Market Accessors (derived from Market enum)
    // -------------------------------------------------------------------------

    /// Whether this is a spot market (no liquidation, no leverage)
    pub fn is_spot(&self) -> bool {
        self.market.is_spot()
    }

    /// Get the instrument ID from the market
    pub fn instrument_id(&self) -> InstrumentId {
        self.market.instrument_id()
    }

    /// Get the market index from the market
    pub fn market_index(&self) -> MarketIndex {
        self.market.market_index()
    }

    /// Get the exchange instance
    pub fn exchange_instance(&self) -> ExchangeInstance {
        self.market.exchange_instance(self.environment)
    }

    // -------------------------------------------------------------------------
    // Trailing Accessors (inferred from limit presence)
    // -------------------------------------------------------------------------

    /// Whether upward trailing is active (i.e., `trailing_up_limit` is set).
    pub fn trailing_up_enabled(&self) -> bool {
        self.trailing_up_limit.is_some()
    }

    /// Whether downward trailing is active (i.e., `trailing_down_limit` is set).
    pub fn trailing_down_enabled(&self) -> bool {
        self.trailing_down_limit.is_some()
    }

    /// Whether any trailing direction is active.
    pub fn trailing_enabled(&self) -> bool {
        self.trailing_up_enabled() || self.trailing_down_enabled()
    }

    // -------------------------------------------------------------------------
    // Validation
    // -------------------------------------------------------------------------

    /// Validate the configuration and return any errors.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        // Grid levels validation
        if self.grid_levels < 2 {
            errors.push("grid_levels must be >= 2".into());
        }

        // Price range validation
        if self.start_price <= Decimal::ZERO {
            errors.push("start_price must be > 0".into());
        }
        if self.end_price <= Decimal::ZERO {
            errors.push("end_price must be > 0".into());
        }

        // All grid modes require start_price < end_price
        // This ensures step is always positive, and TP direction is handled by mode:
        // - LONG: TP = entry + step (above)
        // - SHORT: TP = entry - step (below)
        // - NEUTRAL: both directions based on position relative to mid
        if self.end_price <= self.start_price {
            errors.push(format!(
                "end_price ({}) must be > start_price ({})",
                self.end_price, self.start_price
            ));
        }

        // Trailing limit ordering: trailing_down_limit < start_price < end_price < trailing_up_limit
        if let Some(floor) = self.trailing_down_limit {
            if floor <= Decimal::ZERO {
                errors.push("trailing_down_limit must be > 0".into());
            }
            if floor >= self.start_price {
                errors.push(format!(
                    "trailing_down_limit ({}) must be < start_price ({})",
                    floor, self.start_price
                ));
            }
        }
        if let Some(ceiling) = self.trailing_up_limit {
            if ceiling <= Decimal::ZERO {
                errors.push("trailing_up_limit must be > 0".into());
            }
            if ceiling <= self.end_price {
                errors.push(format!(
                    "trailing_up_limit ({}) must be > end_price ({})",
                    ceiling, self.end_price
                ));
            }
        }

        // Investment validation
        if self.max_investment_quote <= Decimal::ZERO {
            errors.push("max_investment_quote must be > 0".into());
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

        // Base order size validation
        if self.base_order_size <= Decimal::ZERO {
            errors.push("base_order_size must be > 0".into());
        }

        // Calculate quote per level and validate
        if self.grid_levels > 1 {
            let notional_budget = self.max_investment_quote * self.leverage;
            let quote_per_level = notional_budget / Decimal::from(self.grid_levels - 1);
            if quote_per_level < Decimal::new(20, 0) {
                errors.push(format!(
                    "Quote per level ({}) is too low (must be >= 20). \
                     Increase max_investment_quote or reduce grid_levels.",
                    quote_per_level
                ));
            }
        }

        errors
    }

    /// Calculate the grid step (price difference between adjacent levels).
    pub fn grid_step(&self) -> Decimal {
        if self.grid_levels <= 1 {
            return Decimal::ZERO;
        }
        (self.end_price - self.start_price).abs() / Decimal::from(self.grid_levels - 1)
    }

    /// Calculate the notional budget (max_investment_quote × leverage).
    pub fn notional_budget(&self) -> Decimal {
        if self.is_spot() {
            self.max_investment_quote
        } else {
            self.max_investment_quote * self.leverage
        }
    }

    /// Calculate the quote amount per level.
    pub fn quote_per_level(&self) -> Decimal {
        if self.grid_levels <= 1 {
            return self.notional_budget();
        }
        self.notional_budget() / Decimal::from(self.grid_levels - 1)
    }
}

impl Default for GridConfig {
    fn default() -> Self {
        Self {
            strategy_id: StrategyId::new("grid-default"),
            environment: Environment::Testnet,
            market: Market::Hyperliquid(HyperliquidMarket::Perp {
                base: "BTC".to_string(),
                quote: "USDC".to_string(),
                index: 0,
                instrument_meta: None,
            }),

            grid_mode: GridMode::Long,
            grid_levels: 20,
            start_price: Decimal::new(80000, 0),
            end_price: Decimal::new(90000, 0),

            max_investment_quote: Decimal::new(400, 0), // 400 USDC initial margin
            base_order_size: Decimal::new(1, 3),        // 0.001 fallback (rarely used)
            leverage: Decimal::new(5, 0),
            max_leverage: Decimal::new(50, 0),

            post_only: false,

            stop_loss: None,
            take_profit: None,

            trailing_up_limit: None,
            trailing_down_limit: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_is_valid() {
        let config = GridConfig::default();
        let errors = config.validate();
        assert!(
            errors.is_empty(),
            "Default config should be valid: {:?}",
            errors
        );
    }

    #[test]
    fn test_invalid_grid_levels() {
        let mut config = GridConfig::default();
        config.grid_levels = 1;
        let errors = config.validate();
        assert!(errors.iter().any(|e| e.contains("grid_levels")));
    }

    #[test]
    fn test_invalid_price_range_long() {
        let mut config = GridConfig::default();
        config.grid_mode = GridMode::Long;
        config.start_price = Decimal::new(90000, 0);
        config.end_price = Decimal::new(80000, 0);
        let errors = config.validate();
        assert!(errors.iter().any(|e| e.contains("end_price")));
    }

    #[test]
    fn test_invalid_price_range_short() {
        let mut config = GridConfig::default();
        config.grid_mode = GridMode::Short;
        // SHORT now also requires start < end (same as LONG/NEUTRAL)
        config.start_price = Decimal::new(90000, 0);
        config.end_price = Decimal::new(80000, 0);
        let errors = config.validate();
        assert!(errors.iter().any(|e| e.contains("end_price")));
    }

    #[test]
    fn test_grid_step_calculation() {
        let config = GridConfig::default();
        let step = config.grid_step();
        // 20 levels from 80000 to 90000 = 10000 / 19 ≈ 526.31...
        assert!(step > Decimal::ZERO);
    }

    #[test]
    fn test_quote_per_level_calculation() {
        let config = GridConfig::default();
        // 400 * 5 = 2000 notional, 2000 / 19 ≈ 105.26
        let quote = config.quote_per_level();
        assert!(quote > Decimal::new(100, 0));
    }
}
