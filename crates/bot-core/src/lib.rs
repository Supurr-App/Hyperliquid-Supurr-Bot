//! bot-core: Canonical types, events, commands, and traits for the trading bot.
//!
//! This crate defines the exchange-agnostic domain model that all strategies and adapters use.

pub mod commands;
pub mod events;
pub mod exchange;
pub mod market;
pub mod strategy;
pub mod types;

pub use commands::*;
pub use events::*;
pub use exchange::*;
pub use market::*;
pub use strategy::*;
pub use types::*;
