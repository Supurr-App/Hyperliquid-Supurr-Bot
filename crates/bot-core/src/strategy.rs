//! Strategy trait and context.

use crate::commands::*;
use crate::events::Event;
use crate::types::*;
use std::time::Duration;

/// Timer registration result
pub struct TimerHandle {
    pub id: TimerId,
}

/// Strategy context - what strategies use to interact with the engine.
///
/// This is passed to strategy methods and provides:
/// - Command emission (place/cancel orders)
/// - Timer management
/// - Read-only access to market data and state
/// - Logging
pub trait StrategyContext {
    // -------------------------------------------------------------------------
    // Commands
    // -------------------------------------------------------------------------

    /// Place a new order. Returns immediately; acceptance/rejection comes via events.
    fn place_order(&mut self, cmd: PlaceOrder);

    /// Place multiple orders in a single batch. Returns immediately; acceptance/rejection comes via events.
    fn place_orders(&mut self, cmds: Vec<PlaceOrder>);

    /// Cancel an existing order by client_id.
    fn cancel_order(&mut self, cmd: CancelOrder);

    /// Cancel all open orders (optionally for an instrument).
    fn cancel_all(&mut self, cmd: CancelAll);

    /// Request strategy stop. This will trigger on_stop callback after current event processing.
    fn stop_strategy(&mut self, strategy_id: StrategyId, reason: &str);

    // -------------------------------------------------------------------------
    // Timers
    // -------------------------------------------------------------------------

    /// Register a one-shot timer that fires after `delay`.
    fn set_timer(&mut self, delay: Duration) -> TimerId;

    /// Register a repeating timer that fires every `interval`.
    fn set_interval(&mut self, interval: Duration) -> TimerId;

    /// Cancel a previously set timer.
    fn cancel_timer(&mut self, timer_id: TimerId);

    // -------------------------------------------------------------------------
    // Read-only state
    // -------------------------------------------------------------------------

    /// Get the current mid price for an instrument (if available from quote poller)
    fn mid_price(&self, instrument: &InstrumentId) -> Option<Price>;

    /// Get the current best bid/ask quote for an instrument
    fn quote(&self, instrument: &InstrumentId) -> Option<Quote>;

    /// Get instrument metadata
    fn instrument_meta(&self, instrument: &InstrumentId) -> Option<&InstrumentMeta>;

    /// Get this strategy's current balance for an asset
    fn balance(&self, asset: &AssetId) -> Balance;

    /// Get the current position for an instrument
    fn position(&self, instrument: &InstrumentId) -> Position;

    /// Check if exchange is Active or Halted
    fn exchange_health(&self, exchange: &ExchangeInstance) -> ExchangeHealth;

    /// Get an order by client_id (if tracked)
    fn order(&self, client_id: &ClientOrderId) -> Option<&LiveOrder>;

    // -------------------------------------------------------------------------
    // Time
    // -------------------------------------------------------------------------

    /// Current time in milliseconds (deterministic in backtests)
    fn now_ms(&self) -> i64;

    // -------------------------------------------------------------------------
    // Logging
    // -------------------------------------------------------------------------

    /// Log an info message
    fn log_info(&self, msg: &str);

    /// Log a warning message
    fn log_warn(&self, msg: &str);

    /// Log an error message
    fn log_error(&self, msg: &str);

    /// Log a debug message
    fn log_debug(&self, msg: &str);
}

/// The Strategy trait - what every strategy must implement.
///
/// Strategies are deterministic state machines that:
/// - Receive canonical events
/// - Emit commands via the context
/// - Maintain internal state
///
/// Strategies never call HTTP directly.
pub trait Strategy: Send + 'static {
    /// Unique identifier for this strategy instance
    fn id(&self) -> &StrategyId;

    /// Declare the synchronization mechanism for this strategy.
    /// Default is Poll (incremental fills).
    fn sync_mechanism(&self) -> SyncMechanism {
        SyncMechanism::Poll
    }

    /// Called once when the engine starts this strategy.
    /// Use this to initialize state, set timers, etc.
    fn on_start(&mut self, ctx: &mut dyn StrategyContext);

    /// Called for every event routed to this strategy.
    fn on_event(&mut self, ctx: &mut dyn StrategyContext, event: &Event);

    /// Called when a timer fires.
    fn on_timer(&mut self, ctx: &mut dyn StrategyContext, timer_id: TimerId);

    /// Called once when the engine is stopping this strategy.
    /// Use this to cancel orders, log final state, etc.
    fn on_stop(&mut self, ctx: &mut dyn StrategyContext);
}
