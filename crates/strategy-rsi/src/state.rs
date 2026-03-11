//! RSI strategy runtime state.

use bot_core::ClientOrderId;

/// Tracks the strategy's position and order lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Warming up — RSI hasn't converged yet (not enough bars)
    WarmingUp,
    /// Monitoring RSI, no position held
    Watching,
    /// Buy order placed, waiting for fill
    Opening,
    /// Position is held, monitoring RSI for exit signal
    InPosition,
    /// Sell order placed, waiting for fill
    Closing,
    /// Strategy has completed (used in one-shot mode)
    Done,
}

pub struct RsiState {
    /// Current lifecycle phase
    pub phase: Phase,
    /// Total bars completed
    pub bars_count: u64,
    /// Total ticks received
    pub tick_count: u64,
    /// Last RSI value (for logging)
    pub last_rsi: Option<f64>,
    /// Active order ID (if any)
    pub active_order: Option<ClientOrderId>,
}

impl RsiState {
    pub fn new() -> Self {
        Self {
            phase: Phase::WarmingUp,
            bars_count: 0,
            tick_count: 0,
            last_rsi: None,
            active_order: None,
        }
    }
}
