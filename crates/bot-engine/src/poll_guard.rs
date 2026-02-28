//! Generic poll handler with backoff and error classification.
//!
//! Encapsulates the retry/backoff/health concerns shared by all polling
//! operations (fills, quotes, account state). Callers get a clean
//! `PollOutcome` enum and only handle the data path.

use crate::compat::sleep;
use crate::runner::RunnerConfig;
use bot_core::ExchangeError;
use std::time::Duration;

// ---------------------------------------------------------------------------
// HasItems trait — lets PollGuard distinguish "got data" from "got empty Ok"
// ---------------------------------------------------------------------------

/// Trait to check if a poll result contains actual data.
///
/// Blanket-implemented for `Vec<T>`, so `Vec<Fill>`, `Vec<Quote>`, etc.
/// all satisfy this automatically.
pub trait HasItems {
    fn has_items(&self) -> bool;
}

impl<T> HasItems for Vec<T> {
    fn has_items(&self) -> bool {
        !self.is_empty()
    }
}

// ---------------------------------------------------------------------------
// PollOutcome — the only thing callers see
// ---------------------------------------------------------------------------

/// Result of a guarded poll operation.
pub enum PollOutcome<T> {
    /// Got data — backoff reset, consecutive empties reset.
    Data(T),
    /// Got Ok([]) or transient error — backoff applied internally.
    Empty,
    /// Transient but health-affecting error (e.g. 502/503).
    /// Caller should set health to Halted; guard still applies backoff internally.
    Degraded(ExchangeError),
    /// Fatal (non-transient) error — caller should degrade health.
    Fatal(ExchangeError),
}

// ---------------------------------------------------------------------------
// PollGuard — per-(exchange, operation) state machine
// ---------------------------------------------------------------------------

/// Per-poll-operation state machine that handles backoff, error classification,
/// and consecutive-empty tracking.
///
/// Create one per `(exchange, poll_type)` — fills and quotes can fail
/// independently and need separate backoff clocks.
pub struct PollGuard {
    label: &'static str,
    backoff_ms: u64,
    initial_backoff_ms: u64,
    max_backoff_ms: u64,
    backoff_multiplier: f64,
    consecutive_empties: u32,
}

impl PollGuard {
    /// Create a new guard with backoff config derived from `RunnerConfig`.
    pub fn new(label: &'static str, config: &RunnerConfig) -> Self {
        Self {
            label,
            backoff_ms: 0,
            initial_backoff_ms: config.initial_backoff_ms,
            max_backoff_ms: config.max_backoff_ms,
            backoff_multiplier: config.backoff_multiplier,
            consecutive_empties: 0,
        }
    }

    /// Execute a poll with built-in backoff + error handling.
    ///
    /// The skeleton:
    /// 1. Apply current backoff delay (if any).
    /// 2. Call `poll_fn`.
    /// 3. Classify the result and update internal state.
    /// 4. Return a clean `PollOutcome` so the caller only handles data.
    pub async fn execute<T, F, Fut>(&mut self, poll_fn: F) -> PollOutcome<T>
    where
        T: HasItems,
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, ExchangeError>>,
    {
        // 1. Apply backoff delay
        if self.backoff_ms > 0 {
            sleep(Duration::from_millis(self.backoff_ms)).await;
        }

        // 2. Call the actual poll
        match poll_fn().await {
            Ok(result) if result.has_items() => {
                // Got data — reset everything
                self.backoff_ms = 0;
                self.consecutive_empties = 0;
                PollOutcome::Data(result)
            }
            Ok(_empty) => {
                // Empty response — API is healthy, reset backoff but track emptiness
                self.backoff_ms = 0;
                self.consecutive_empties += 1;
                tracing::debug!(
                    "[PollGuard:{}] Empty response ({}x consecutive)",
                    self.label,
                    self.consecutive_empties
                );
                PollOutcome::Empty
            }
            Err(e) if e.is_transient() => {
                // Transient error — backoff and retry next iteration
                self.increase_backoff();

                if e.is_502() {
                    // 502/503 is transient (retry) but also health-affecting
                    tracing::warn!(
                        "[PollGuard:{}] Exchange unavailable, backoff={}ms: {}",
                        self.label,
                        self.backoff_ms,
                        e
                    );
                    PollOutcome::Degraded(e)
                } else {
                    tracing::warn!(
                        "[PollGuard:{}] Transient error, backoff={}ms: {}",
                        self.label,
                        self.backoff_ms,
                        e
                    );
                    PollOutcome::Empty
                }
            }
            Err(e) => {
                // Fatal error — caller should degrade health
                tracing::error!("[PollGuard:{}] Fatal error: {}", self.label, e);
                PollOutcome::Fatal(e)
            }
        }
    }

    /// Check if the poll source looks exhausted.
    ///
    /// Used by the runner for backtest exit detection:
    /// `if delay == 0 && guard.looks_exhausted(3) { break; }`
    pub fn looks_exhausted(&self, threshold: u32) -> bool {
        self.consecutive_empties >= threshold
    }

    /// Exponential backoff with cap.
    fn increase_backoff(&mut self) {
        self.backoff_ms = if self.backoff_ms == 0 {
            self.initial_backoff_ms
        } else {
            ((self.backoff_ms as f64) * self.backoff_multiplier) as u64
        };
        self.backoff_ms = self.backoff_ms.min(self.max_backoff_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_items_vec() {
        let empty: Vec<i32> = vec![];
        assert!(!empty.has_items());

        let non_empty = vec![1, 2, 3];
        assert!(non_empty.has_items());
    }

    #[test]
    fn looks_exhausted_threshold() {
        let config = RunnerConfig::default();
        let mut guard = PollGuard::new("test", &config);

        assert!(!guard.looks_exhausted(3));

        guard.consecutive_empties = 2;
        assert!(!guard.looks_exhausted(3));

        guard.consecutive_empties = 3;
        assert!(guard.looks_exhausted(3));
    }

    #[test]
    fn increase_backoff_exponential() {
        let config = RunnerConfig {
            initial_backoff_ms: 100,
            max_backoff_ms: 1000,
            backoff_multiplier: 2.0,
            ..RunnerConfig::default()
        };
        let mut guard = PollGuard::new("test", &config);

        // First backoff: 0 → initial
        guard.increase_backoff();
        assert_eq!(guard.backoff_ms, 100);

        // Second: 100 * 2 = 200
        guard.increase_backoff();
        assert_eq!(guard.backoff_ms, 200);

        // Third: 200 * 2 = 400
        guard.increase_backoff();
        assert_eq!(guard.backoff_ms, 400);

        // Fourth: 400 * 2 = 800
        guard.increase_backoff();
        assert_eq!(guard.backoff_ms, 800);

        // Fifth: 800 * 2 = 1600 → cap at 1000
        guard.increase_backoff();
        assert_eq!(guard.backoff_ms, 1000);
    }
}
