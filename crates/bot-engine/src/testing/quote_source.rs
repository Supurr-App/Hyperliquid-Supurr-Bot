//! Quote source trait: Abstraction for quote data providers.
//!
//! Implementations:
//! - `MockQuoteSource`: Manual injection for tests
//! - `LiveQuoteSource`: Wraps real exchange (future)
//! - `CsvQuoteSource`: Replay from CSV (future)

use bot_core::{InstrumentId, Quote};
use std::collections::HashMap;

/// Trait for providing quote data.
///
/// This abstraction allows different quote sources to be plugged in:
/// - Mock quotes for testing
/// - Live quotes from real exchange
/// - Historical quotes from CSV
#[async_trait::async_trait]
pub trait QuoteSource: Send + Sync {
    /// Get current quotes for the given instruments
    async fn poll_quotes(&self, instruments: &[InstrumentId]) -> Vec<Quote>;

    /// Get the last known quote for an instrument (if cached)
    fn last_quote(&self, instrument: &InstrumentId) -> Option<Quote>;

    /// Update time (for simulation purposes)
    fn set_time(&mut self, time_ms: i64);

    /// Get current time
    fn current_time_ms(&self) -> i64;
}

/// Mock quote source for testing - allows manual quote injection.
pub struct MockQuoteSource {
    quotes: HashMap<InstrumentId, Quote>,
    time_ms: i64,
}

impl MockQuoteSource {
    pub fn new() -> Self {
        Self {
            quotes: HashMap::new(),
            time_ms: bot_core::now_ms(),
        }
    }

    /// Set a quote for an instrument
    pub fn set_quote(&mut self, quote: Quote) {
        self.quotes.insert(quote.instrument.clone(), quote);
    }

    /// Clear all quotes
    pub fn clear(&mut self) {
        self.quotes.clear();
    }
}

impl Default for MockQuoteSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl QuoteSource for MockQuoteSource {
    async fn poll_quotes(&self, instruments: &[InstrumentId]) -> Vec<Quote> {
        instruments
            .iter()
            .filter_map(|inst| self.quotes.get(inst).cloned())
            .collect()
    }

    fn last_quote(&self, instrument: &InstrumentId) -> Option<Quote> {
        self.quotes.get(instrument).cloned()
    }

    fn set_time(&mut self, time_ms: i64) {
        self.time_ms = time_ms;
    }

    fn current_time_ms(&self) -> i64 {
        self.time_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bot_core::{Price, Qty};

    #[tokio::test]
    async fn test_mock_quote_source() {
        let mut source = MockQuoteSource::new();

        let instrument = InstrumentId::new("BTC-PERP");
        let quote = Quote {
            instrument: instrument.clone(),
            bid: Price::new(rust_decimal::Decimal::new(50000, 0)),
            ask: Price::new(rust_decimal::Decimal::new(50010, 0)),
            bid_size: Qty::new(rust_decimal::Decimal::new(1, 0)),
            ask_size: Qty::new(rust_decimal::Decimal::new(1, 0)),
            ts: 0,
        };

        source.set_quote(quote.clone());

        let quotes = source.poll_quotes(&[instrument.clone()]).await;
        assert_eq!(quotes.len(), 1);
        assert_eq!(quotes[0].bid.0, rust_decimal::Decimal::new(50000, 0));
    }
}
