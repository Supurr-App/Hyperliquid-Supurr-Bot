//! Tick Trader configuration.

use bot_core::{Environment, Market, StrategyId};
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TickTraderConfig {
    pub strategy_id: StrategyId,
    pub environment: Environment,
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
