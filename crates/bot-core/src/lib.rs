//! Canonical types, events, commands, and traits for the trading bot.
//!
//! `bot-core` is the shared contract between strategies, the engine, and
//! exchange adapters. It owns identifiers, money/quantity wrappers, instrument
//! metadata, order state, command/event DTOs, and the strategy/exchange traits.
//!
//! # Example
//!
//! ```
//! use bot_core::{
//!     Environment, ExchangeId, ExchangeInstance, InstrumentId, OrderSide, PlaceOrder, Price, Qty,
//! };
//! use rust_decimal::Decimal;
//!
//! let exchange = ExchangeInstance::new(ExchangeId::new("hyperliquid"), Environment::Testnet);
//! let order = PlaceOrder::limit(
//!     exchange,
//!     InstrumentId::new("BTC-PERP"),
//!     OrderSide::Buy,
//!     Price::new(Decimal::new(65_000, 0)),
//!     Qty::new(Decimal::new(1, 3)),
//! );
//!
//! assert_eq!(order.instrument.as_str(), "BTC-PERP");
//! ```
//!
//! # Error and panic policy
//!
//! Constructors are intentionally lightweight and do not validate exchange
//! semantics. String parsing helpers return `rust_decimal::Error` on invalid
//! decimals. Non-WASM [`now_ms`] panics only if the host clock is before the
//! Unix epoch.

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
