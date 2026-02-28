//! Grid Trading Strategy
//!
//! Implements a grid trading bot that places limit orders at predetermined
//! price levels. When an order fills, a take-profit order is placed in the
//! opposite direction. When the take-profit fills, the cycle repeats.
//!
//! # Modes
//!
//! - **LONG**: All levels are BUY orders. Take profit is SELL above entry.
//!   Ideal for bullish or ranging markets where you expect price to stay within range.
//!
//! - **SHORT**: All levels are SELL orders. Take profit is BUY below entry.
//!   Ideal for bearish markets.
//!
//! - **NEUTRAL**: Mixed mode. Levels below current price are BUY (long).
//!   Levels above current price are SELL (short). The center level is inactive.
//!   Ideal for ranging markets with no clear direction.
//!
//! # Example
//!
//! ```ignore
//! use strategy_grid::{GridStrategy, GridConfig, GridMode};
//! use bot_core::{StrategyId, InstrumentId, MarketIndex, ExchangeInstance, ExchangeId, Environment};
//! use rust_decimal::Decimal;
//!
//! let config = GridConfig {
//!     strategy_id: StrategyId::new("btc-grid"),
//!     exchange: ExchangeInstance::new(ExchangeId::new("hyperliquid"), Environment::Mainnet),
//!     instrument: InstrumentId::new("BTC-PERP"),
//!     market_index: MarketIndex::new(0),
//!     grid_mode: GridMode::Long,
//!     grid_levels: 20,
//!     start_price: Decimal::new(80000, 0),
//!     end_price: Decimal::new(90000, 0),
//!     max_investment_quote: Decimal::new(1000, 0),
//!     leverage: Decimal::new(5, 0),
//!     ..Default::default()
//! };
//!
//! let strategy = GridStrategy::new(config);
//! ```

mod config;
mod state;
mod strategy;

pub use config::*;
pub use state::*;
pub use strategy::*;














