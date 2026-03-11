//! Wilder's RSI (Relative Strength Index) — inline implementation.
//!
//! Uses the Wilder smoothing method (exponential moving average with
//! α = 1/period), which is identical to the standard RSI formula.
//!
//! The indicator needs `period + 1` bars before producing its first value.

/// Streaming RSI indicator.
///
/// Feed it close prices one bar at a time via `update()`.
/// Returns `None` until enough bars have been consumed (period + 1).
pub struct Rsi {
    period: usize,
    /// Collect closes during warmup phase.
    warmup_closes: Vec<f64>,
    /// After initialization, track only the previous bar's close for delta.
    prev_close: f64,
    avg_gain: f64,
    avg_loss: f64,
    /// `true` once the initial SMA seed has been computed.
    initialized: bool,
}

impl Rsi {
    /// Create a new RSI indicator with the given period (typically 14).
    pub fn new(period: usize) -> Self {
        assert!(period >= 2, "RSI period must be >= 2");
        Self {
            period,
            warmup_closes: Vec::with_capacity(period + 1),
            prev_close: 0.0,
            avg_gain: 0.0,
            avg_loss: 0.0,
            initialized: false,
        }
    }

    /// Feed the close price of a completed bar.
    ///
    /// Returns `Some(rsi_value)` once the indicator has converged (after
    /// `period + 1` bars), or `None` if still warming up.
    pub fn update(&mut self, close: f64) -> Option<f64> {
        if !self.initialized {
            self.warmup_closes.push(close);

            // Need period + 1 closes to compute the first RSI
            if self.warmup_closes.len() <= self.period {
                return None;
            }

            // Compute initial average gain / loss as simple average
            let (sum_gain, sum_loss) =
                self.warmup_closes.windows(2).fold((0.0, 0.0), |(g, l), w| {
                    let change = w[1] - w[0];
                    if change > 0.0 {
                        (g + change, l)
                    } else {
                        (g, l + change.abs())
                    }
                });

            let n = self.period as f64;
            self.avg_gain = sum_gain / n;
            self.avg_loss = sum_loss / n;
            self.initialized = true;

            // Save last close for next delta, then free warmup storage
            self.prev_close = close;
            self.warmup_closes.clear();
            self.warmup_closes.shrink_to_fit();
        } else {
            // Wilder smoothing: new_avg = (prev_avg * (n-1) + current) / n
            let change = close - self.prev_close;
            let n = self.period as f64;

            let (current_gain, current_loss) = if change > 0.0 {
                (change, 0.0)
            } else {
                (0.0, change.abs())
            };

            self.avg_gain = (self.avg_gain * (n - 1.0) + current_gain) / n;
            self.avg_loss = (self.avg_loss * (n - 1.0) + current_loss) / n;

            // Update prev_close for next iteration
            self.prev_close = close;
        }

        Some(self.compute())
    }

    /// Compute RSI from current averages.
    fn compute(&self) -> f64 {
        if self.avg_loss == 0.0 {
            return 100.0; // No losses → max RSI
        }
        let rs = self.avg_gain / self.avg_loss;
        100.0 - (100.0 / (1.0 + rs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rsi_warmup_period() {
        let mut rsi = Rsi::new(14);
        // First 14 bars should return None (need 15 = period + 1)
        for i in 0..14 {
            assert!(rsi.update(100.0 + i as f64).is_none());
        }
        // 15th bar should return Some
        assert!(rsi.update(114.0).is_some());
    }

    #[test]
    fn test_rsi_all_gains() {
        let mut rsi = Rsi::new(3);
        // prices: 10, 11, 12, 13 — all gains, no losses
        rsi.update(10.0);
        rsi.update(11.0);
        rsi.update(12.0);
        let val = rsi.update(13.0).unwrap();
        assert!((val - 100.0).abs() < 0.01, "All-up RSI should be ~100");
    }

    #[test]
    fn test_rsi_all_losses() {
        let mut rsi = Rsi::new(3);
        // prices: 13, 12, 11, 10 — all losses, no gains
        rsi.update(13.0);
        rsi.update(12.0);
        rsi.update(11.0);
        let val = rsi.update(10.0).unwrap();
        assert!(val < 1.0, "All-down RSI should be ~0, got {}", val);
    }

    #[test]
    fn test_rsi_mixed() {
        let mut rsi = Rsi::new(3);
        // prices: 44, 44.34, 44.09, 43.61 → mixed
        rsi.update(44.0);
        rsi.update(44.34);
        rsi.update(44.09);
        let val = rsi.update(43.61).unwrap();
        // RSI should be between 0 and 100
        assert!(
            val > 0.0 && val < 100.0,
            "RSI should be in (0,100), got {}",
            val
        );
    }

    #[test]
    fn test_rsi_continues_after_warmup() {
        // This test specifically verifies no panic after initialization
        let mut rsi = Rsi::new(3);
        rsi.update(10.0);
        rsi.update(11.0);
        rsi.update(12.0);
        let v1 = rsi.update(13.0).unwrap(); // initial RSI
        let v2 = rsi.update(14.0).unwrap(); // 1st post-warmup
        let v3 = rsi.update(13.5).unwrap(); // 2nd post-warmup (price drop)
        let v4 = rsi.update(12.0).unwrap(); // 3rd post-warmup (bigger drop)

        // All should be valid RSI values
        for (i, v) in [v1, v2, v3, v4].iter().enumerate() {
            assert!(
                *v >= 0.0 && *v <= 100.0,
                "RSI[{}] = {} is out of range",
                i,
                v
            );
        }
        // After price drops, RSI should decrease
        assert!(v4 < v1, "RSI should decrease after drops: {} vs {}", v4, v1);
    }
}
