//! RSI Trading Strategy
//!
//! Uses the RSI (Relative Strength Index) indicator from `quantedge-ta` to
//! generate buy/sell signals. Quotes are aggregated into OHLCV bars via
//! `BarBuilder`, then fed to the RSI indicator.
//!
//! # Signal Logic
//!
//! - **Buy**: RSI crosses below `oversold` threshold (default 30)
//! - **Sell**: RSI crosses above `overbought` threshold (default 70)
//!
//! # Example
//!
//! ```ignore
//! // In your config JSON:
//! {
//!   "strategy_type": "rsi",
//!   "rsi": {
//!     "rsi_period": 14,
//!     "bar_interval_secs": 60,
//!     "oversold": 30.0,
//!     "overbought": 70.0,
//!     "order_size": "0.01",
//!     "side": "long"
//!   }
//! }
//! ```

mod bar;
mod config;
mod indicator;
mod state;
mod strategy;

pub use config::*;
pub use strategy::*;
