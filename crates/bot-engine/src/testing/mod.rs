//! Testing infrastructure: Mock exchanges, paper trading, and simulation utilities.
//!
//! This module provides reusable components for testing and paper trading:
//! - `QuoteSource`: Trait for quote data providers (mock, live, CSV)
//! - `FillSimulator`: Shared fill matching logic (price crossing detection)
//! - `MockExchange`: Manual quote/fill injection for unit tests
//! - `PaperExchange`: Live quotes with simulated fills for paper trading
//! - `MockAccountSyncer`, `MockTradeSyncer`: Mock syncers for testing (native only)

// Core testing modules (always available)
pub mod fill_simulator;
pub mod mock_exchange;
pub mod paper_exchange;
pub mod quote_source;

// Native-only testing modules (require sync_traits)
#[cfg(feature = "native")]
pub mod mock_syncer;

// Re-export core types (always available)
pub use fill_simulator::{FillSimulator, PendingOrder, SimulatedFill};
pub use mock_exchange::{MockExchange, MockKnobs, OrderFailMode};
pub use paper_exchange::{
    create_standalone_paper_exchange, create_standalone_paper_exchange_with_id, ArcExchange,
    NoOpExchange, PaperExchange, StandalonePaperExchange,
};
pub use quote_source::{MockQuoteSource, QuoteSource};

// Re-export native-only mock syncers
#[cfg(feature = "native")]
pub use mock_syncer::{MockAccountSyncer, MockTradeSyncer};
