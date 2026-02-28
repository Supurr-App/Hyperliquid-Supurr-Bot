//! Tick Trader Strategy
//!
//! Opens a position after N quote ticks, closes after M ticks.

mod config;
mod state;
mod strategy;

pub use config::*;
pub use state::*;
pub use strategy::*;
