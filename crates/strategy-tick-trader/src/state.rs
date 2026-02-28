//! Tick Trader runtime state.

use bot_core::ClientOrderId;

/// Tracks the strategy's lifecycle phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Counting ticks before opening
    WaitingToOpen,
    /// Open order placed, waiting for fill
    OpeningPosition,
    /// Position is open, counting ticks before closing
    WaitingToClose,
    /// Close order placed, waiting for fill
    ClosingPosition,
    /// Strategy complete
    Done,
}

pub struct TickTraderState {
    /// Current phase
    pub phase: Phase,
    /// Total quote ticks received
    pub tick_count: u32,
    /// Ticks received since position was opened
    pub ticks_since_open: u32,
    /// Active order ID (if any)
    pub active_order: Option<ClientOrderId>,
}

impl TickTraderState {
    pub fn new() -> Self {
        Self {
            phase: Phase::WaitingToOpen,
            tick_count: 0,
            ticks_since_open: 0,
            active_order: None,
        }
    }
}
