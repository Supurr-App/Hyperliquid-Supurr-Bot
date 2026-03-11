//! Bar aggregation — converts tick-level quotes into OHLCV bars.
//!
//! `BarBuilder` accumulates mid prices from quote events and emits a
//! completed `Bar` when the time interval rolls over.

/// A completed OHLCV bar.
#[derive(Debug, Clone)]
pub struct Bar {
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    /// Bar open timestamp in milliseconds.
    pub open_time_ms: i64,
}

/// Aggregates streaming mid prices into time-based OHLCV bars.
///
/// Feed it `(mid_price, timestamp_ms)` pairs. When the current bar's time
/// window expires, `update()` returns `Some(completed_bar)` and starts a
/// new bar.
pub struct BarBuilder {
    interval_ms: i64,
    current_open: f64,
    current_high: f64,
    current_low: f64,
    current_close: f64,
    current_open_time: i64,
    /// Whether we've received at least one tick for the current bar.
    has_data: bool,
}

impl BarBuilder {
    /// Create a new bar builder with the given interval in seconds.
    pub fn new(interval_secs: u64) -> Self {
        Self {
            interval_ms: (interval_secs * 1000) as i64,
            current_open: 0.0,
            current_high: 0.0,
            current_low: 0.0,
            current_close: 0.0,
            current_open_time: 0,
            has_data: false,
        }
    }

    /// Feed a new mid price at the given timestamp.
    ///
    /// Returns `Some(Bar)` if the previous bar was completed by this tick,
    /// plus starts accumulating for the new bar.
    pub fn update(&mut self, mid: f64, ts_ms: i64) -> Option<Bar> {
        if !self.has_data {
            // First tick ever — initialize the bar
            self.start_bar(mid, ts_ms);
            return None;
        }

        // Check if this tick belongs to a new bar interval
        let bar_end = self.current_open_time + self.interval_ms;
        if ts_ms >= bar_end {
            // Complete the current bar
            let completed = Bar {
                open: self.current_open,
                high: self.current_high,
                low: self.current_low,
                close: self.current_close,
                open_time_ms: self.current_open_time,
            };

            // Start new bar with this tick
            self.start_bar(mid, ts_ms);

            Some(completed)
        } else {
            // Update current bar
            self.current_high = self.current_high.max(mid);
            self.current_low = self.current_low.min(mid);
            self.current_close = mid;
            None
        }
    }

    /// Initialize a new bar with the given price and timestamp.
    fn start_bar(&mut self, mid: f64, ts_ms: i64) {
        // Align open_time to interval boundary
        self.current_open_time = (ts_ms / self.interval_ms) * self.interval_ms;
        self.current_open = mid;
        self.current_high = mid;
        self.current_low = mid;
        self.current_close = mid;
        self.has_data = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bar_builder_emits_on_interval() {
        let mut builder = BarBuilder::new(60); // 60-second bars

        // First tick at t=0 — no bar emitted
        assert!(builder.update(100.0, 0).is_none());

        // Tick within same bar — no bar emitted
        assert!(builder.update(105.0, 30_000).is_none());
        assert!(builder.update(95.0, 45_000).is_none());

        // Tick in next interval — bar emitted
        let bar = builder.update(102.0, 60_000).unwrap();
        assert_eq!(bar.open, 100.0);
        assert_eq!(bar.high, 105.0);
        assert_eq!(bar.low, 95.0);
        assert_eq!(bar.close, 95.0); // last tick before boundary
    }
}
