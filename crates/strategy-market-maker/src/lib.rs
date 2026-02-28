//! Skew-based Market Maker Strategy
//!
//! Implements both SIZE SKEW and PRICE SKEW based on inventory position:
//! - Size Skew: Adjusts order sizes based on current position
//! - Price Skew: Shifts bid/ask prices based on inventory imbalance
//!
//! Single-level market making with price-based refresh.

mod config;
mod state;
mod strategy;

pub use config::*;
pub use state::*;
pub use strategy::*;



