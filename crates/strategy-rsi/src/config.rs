//! RSI strategy configuration.

use bot_core::StrategyId;
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Configuration for the RSI trading strategy.
///
/// The strategy aggregates quote ticks into OHLCV bars of `bar_interval_secs`
/// duration, computes RSI over `rsi_period` bars, and generates signals when
/// RSI crosses the `oversold` / `overbought` thresholds.
///
/// Note: `market` and `environment` are NOT part of this config — they come
/// from the top-level `BotConfig` and are injected at construction time.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RsiStrategyConfig {
    /// Strategy identifier
    pub strategy_id: StrategyId,

    /// RSI period (number of bars). Default: 14
    #[serde(default = "default_rsi_period")]
    pub rsi_period: u32,

    /// Bar interval in seconds. Default: 60 (1-minute bars)
    #[serde(default = "default_bar_interval_secs")]
    pub bar_interval_secs: u64,

    /// RSI level below which we consider the asset oversold → buy signal.
    /// Default: 30.0
    #[serde(default = "default_oversold")]
    pub oversold: f64,

    /// RSI level above which we consider the asset overbought → sell signal.
    /// Default: 70.0
    #[serde(default = "default_overbought")]
    pub overbought: f64,

    /// Order size in base asset (e.g. "0.01" = 0.01 ETH).
    /// If `order_notional_quote` is set, this is ignored.
    #[serde(default)]
    pub order_size: Decimal,

    /// Order size in quote asset (e.g. "100" = $100 worth).
    /// At each signal, qty = notional / current_price.
    /// Takes precedence over `order_size` if both are set.
    #[serde(default)]
    pub order_notional_quote: Option<Decimal>,

    /// Trading side: "long" (buy low, sell high), "short" (sell high, buy low)
    #[serde(default = "default_side")]
    pub side: String,

    /// Leverage for perpetual markets
    #[serde(default = "default_leverage")]
    pub leverage: Decimal,
}

impl RsiStrategyConfig {
    /// Validate configuration, returning a list of error messages.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        if self.rsi_period < 2 {
            errors.push("rsi_period must be >= 2".into());
        }
        if self.bar_interval_secs == 0 {
            errors.push("bar_interval_secs must be > 0".into());
        }
        if self.oversold <= 0.0 || self.oversold >= 100.0 {
            errors.push("oversold must be between 0 and 100".into());
        }
        if self.overbought <= 0.0 || self.overbought >= 100.0 {
            errors.push("overbought must be between 0 and 100".into());
        }
        if self.oversold >= self.overbought {
            errors.push("oversold must be less than overbought".into());
        }
        let has_base_size = self.order_size > Decimal::ZERO;
        let has_notional = self
            .order_notional_quote
            .map(|n| n > Decimal::ZERO)
            .unwrap_or(false);

        if !has_base_size && !has_notional {
            errors.push("Either order_size or order_notional_quote must be > 0".into());
        }

        errors
    }
}

fn default_rsi_period() -> u32 {
    14
}

fn default_bar_interval_secs() -> u64 {
    60
}

fn default_oversold() -> f64 {
    30.0
}

fn default_overbought() -> f64 {
    70.0
}

fn default_side() -> String {
    "long".to_string()
}

fn default_leverage() -> Decimal {
    Decimal::ONE
}
