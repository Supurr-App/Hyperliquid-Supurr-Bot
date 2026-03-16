//! Simulation Layer: Shared margin-aware accounting for paper trading and backtesting.
//!
//! This module provides accurate perpetual futures simulation including:
//! - Per-instrument isolated margin positions
//! - Margin-aware order admission (instead of full-notional checks)
//! - Proper fill settlement with margin reserve/release
//! - Unrealized PnL tracking at mark prices
//! - Liquidation price computation
//!
//! Used by `PaperExchange` to provide realistic simulation that matches
//! how Hyperliquid actually handles margin and positions.
//!
//! **Note**: This module is ONLY used in the simulation path (paper/backtest).
//! Live trading uses the real exchange which handles all margin math server-side.

pub mod account;

pub use account::{IsolatedPosition, MarginLedger};
