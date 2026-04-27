//! Bot Engine: order manager, inventory ledger, event routing, and polling.
//!
//! This crate contains the runtime that:
//! - Manages order state (in-memory source of truth)
//! - Tracks inventory/balances with reservations
//! - Routes events to strategies
//! - Handles rate limiting and backoff
//! - Manages exchange health state machine
//! - Polling loop for fills and quotes (native only)
//! - Trade syncing to upstream API for PnL tracking (native only)

// Core modules (always available)
pub mod config;
pub mod context;
pub mod engine;
pub mod inventory;
pub mod order_manager;
pub mod performance_metrics;
pub mod poll_guard;

// Simulation layer: margin-aware accounting for paper/backtest (always available)
pub mod simulation;

// Testing and paper trading infrastructure (always available)
pub mod testing;

// Platform compatibility layer (WASM + native)
pub mod compat;

// Native-only modules (require tokio/reqwest)
#[cfg(feature = "native")]
pub mod account_syncer;
#[cfg(feature = "native")]
pub mod sync_traits;
#[cfg(feature = "native")]
pub mod trade_syncer;

// Runner module (core is WASM-compatible, spawn functions are native-only)
pub mod runner;

// WASM API module (only available with wasm feature)
#[cfg(feature = "wasm")]
pub mod wasm_api;

// Re-exports (core)
pub use config::*;
pub use context::*;
pub use engine::*;
pub use inventory::*;
pub use order_manager::*;
pub use performance_metrics::*;
pub use poll_guard::*;

// Re-exports (native-only)
#[cfg(feature = "native")]
pub use account_syncer::*;
#[cfg(feature = "native")]
pub use sync_traits::{AccountSync, AccountSyncResult, TradeSync, TradeSyncResult};
#[cfg(feature = "native")]
pub use trade_syncer::*;

// Re-exports (runner - core types always available, spawn functions native-only)
#[cfg(feature = "native")]
pub use runner::{spawn_runner, spawn_runner_with_syncer};
pub use runner::{BacktestResult, EngineRunner, PollResult, RunnerConfig, TradingStats};

// Re-export testing utilities (always available)
pub use testing::{FillSimulator, MockQuoteSource, PendingOrder, QuoteSource, SimulatedFill};
#[cfg(feature = "native")]
pub use testing::{MockAccountSyncer, MockTradeSyncer};
pub use testing::{MockExchange, MockKnobs, OrderFailMode, PaperExchange}; // Re-export mock syncers (native-only, they depend on sync_traits)
