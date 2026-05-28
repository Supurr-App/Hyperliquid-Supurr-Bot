//! Tick Trader configuration.

use bot_core::{Environment, Market, StrategyId};
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Configuration for the tick-count demo strategy.
///
/// The strategy opens after `open_after_ticks` quote events, then closes after
/// `close_after_ticks` more quote events.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TickTraderConfig {
    /// Unique strategy identifier.
    pub strategy_id: StrategyId,
    /// Trading environment.
    pub environment: Environment,
    /// Market to trade.
    pub market: Market,

    /// Number of quote ticks before opening a position
    pub open_after_ticks: u32,

    /// Number of quote ticks after opening before closing the position
    pub close_after_ticks: u32,

    /// Order size in base asset (e.g. "0.001" = 0.001 BTC)
    pub order_size: Decimal,

    /// Buy or sell to open. "buy" = long, "sell" = short
    pub side: String,
}

impl TickTraderConfig {
    /// Validate strategy settings and return human-readable errors.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if self.order_size <= Decimal::ZERO {
            errors.push("order_size must be > 0".into());
        }
        if self.open_after_ticks == 0 {
            errors.push("open_after_ticks must be > 0".into());
        }
        if self.close_after_ticks == 0 {
            errors.push("close_after_ticks must be > 0".into());
        }
        errors
    }
}
