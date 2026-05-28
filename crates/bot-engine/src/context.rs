//! Strategy context implementation.

use bot_core::{
    AssetId, Balance, CancelAll, CancelOrder, ClientOrderId, ExchangeHealth, ExchangeInstance,
    InstrumentId, InstrumentMeta, LiveOrder, PlaceOrder, Position, Price, Quote, StopStrategy,
    StrategyContext, StrategyId, TimerId,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::inventory::InventoryLedger;
use crate::order_manager::OrderManager;

static TIMER_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Pending timer registration
pub struct PendingTimer {
    /// Engine-assigned timer ID.
    pub id: TimerId,
    /// Delay before the timer fires.
    pub delay: Duration,
    /// Whether the timer repeats after firing.
    pub repeating: bool,
}

/// Concrete implementation of StrategyContext
pub struct EngineContext<'a> {
    // Command buffers (collected during event handling, executed after)
    /// Single-order commands emitted during the current callback.
    pub place_orders: Vec<PlaceOrder>,
    /// Batch order commands emitted during the current callback.
    pub batch_orders: Vec<Vec<PlaceOrder>>,
    /// Cancel-one commands emitted during the current callback.
    pub cancel_orders: Vec<CancelOrder>,
    /// Cancel-all commands emitted during the current callback.
    pub cancel_alls: Vec<CancelAll>,
    /// Strategy stop requests emitted during the current callback.
    pub stop_requests: Vec<StopStrategy>,

    // Timer registrations
    /// Timer registrations emitted during the current callback.
    pub timers: Vec<PendingTimer>,
    /// Timer cancellations emitted during the current callback.
    pub canceled_timers: Vec<TimerId>,

    // Read-only references to engine state
    /// Latest quotes by instrument.
    pub quotes: &'a HashMap<InstrumentId, Quote>,
    /// Instrument metadata by instrument.
    pub instruments: &'a HashMap<InstrumentId, InstrumentMeta>,
    /// Order manager snapshot.
    pub orders: &'a OrderManager,
    /// Inventory ledger snapshot.
    pub inventory: &'a InventoryLedger,
    /// Current positions by instrument.
    pub positions: &'a HashMap<InstrumentId, Position>,
    /// Exchange health by exchange instance.
    pub exchange_health: &'a HashMap<ExchangeInstance, ExchangeHealth>,

    // Current time
    /// Engine clock in milliseconds since the Unix epoch.
    pub now_ms: i64,
}

impl<'a> EngineContext<'a> {
    /// Create an engine context for one strategy callback.
    pub fn new(
        quotes: &'a HashMap<InstrumentId, Quote>,
        instruments: &'a HashMap<InstrumentId, InstrumentMeta>,
        orders: &'a OrderManager,
        inventory: &'a InventoryLedger,
        positions: &'a HashMap<InstrumentId, Position>,
        exchange_health: &'a HashMap<ExchangeInstance, ExchangeHealth>,
        now_ms: i64,
    ) -> Self {
        Self {
            place_orders: Vec::new(),
            batch_orders: Vec::new(),
            cancel_orders: Vec::new(),
            cancel_alls: Vec::new(),
            stop_requests: Vec::new(),
            timers: Vec::new(),
            canceled_timers: Vec::new(),
            quotes,
            instruments,
            orders,
            inventory,
            positions,
            exchange_health,
            now_ms,
        }
    }

    /// Drain all collected commands
    pub fn take_commands(
        &mut self,
    ) -> (
        Vec<PlaceOrder>,
        Vec<Vec<PlaceOrder>>,
        Vec<CancelOrder>,
        Vec<CancelAll>,
        Vec<StopStrategy>,
    ) {
        (
            std::mem::take(&mut self.place_orders),
            std::mem::take(&mut self.batch_orders),
            std::mem::take(&mut self.cancel_orders),
            std::mem::take(&mut self.cancel_alls),
            std::mem::take(&mut self.stop_requests),
        )
    }

    /// Drain all timer registrations
    pub fn take_timers(&mut self) -> (Vec<PendingTimer>, Vec<TimerId>) {
        (
            std::mem::take(&mut self.timers),
            std::mem::take(&mut self.canceled_timers),
        )
    }
}

impl<'a> StrategyContext for EngineContext<'a> {
    fn place_order(&mut self, cmd: PlaceOrder) {
        self.place_orders.push(cmd);
    }

    fn place_orders(&mut self, cmds: Vec<PlaceOrder>) {
        if !cmds.is_empty() {
            self.batch_orders.push(cmds);
        }
    }

    fn cancel_order(&mut self, cmd: CancelOrder) {
        self.cancel_orders.push(cmd);
    }

    fn cancel_all(&mut self, cmd: CancelAll) {
        self.cancel_alls.push(cmd);
    }

    fn stop_strategy(&mut self, strategy_id: StrategyId, reason: &str) {
        tracing::warn!("Strategy {} requested stop: {}", strategy_id.0, reason);
        self.stop_requests
            .push(StopStrategy::new(strategy_id, reason));
    }

    fn set_timer(&mut self, delay: Duration) -> TimerId {
        let id = TimerId::new(TIMER_COUNTER.fetch_add(1, Ordering::SeqCst));
        self.timers.push(PendingTimer {
            id,
            delay,
            repeating: false,
        });
        id
    }

    fn set_interval(&mut self, interval: Duration) -> TimerId {
        let id = TimerId::new(TIMER_COUNTER.fetch_add(1, Ordering::SeqCst));
        self.timers.push(PendingTimer {
            id,
            delay: interval,
            repeating: true,
        });
        id
    }

    fn cancel_timer(&mut self, timer_id: TimerId) {
        self.canceled_timers.push(timer_id);
    }

    fn mid_price(&self, instrument: &InstrumentId) -> Option<Price> {
        self.quotes.get(instrument).map(|q| q.mid())
    }

    fn quote(&self, instrument: &InstrumentId) -> Option<Quote> {
        self.quotes.get(instrument).cloned()
    }

    fn instrument_meta(&self, instrument: &InstrumentId) -> Option<&InstrumentMeta> {
        self.instruments.get(instrument)
    }

    fn balance(&self, asset: &AssetId) -> Balance {
        self.inventory.balance(asset)
    }

    fn position(&self, instrument: &InstrumentId) -> Position {
        let mut pos = self.positions.get(instrument).cloned().unwrap_or_default();

        // Compute unrealized PnL if we have position and quote
        if let (Some(avg_entry), Some(quote)) = (pos.avg_entry_px, self.quotes.get(instrument)) {
            let mid = (quote.bid.0 + quote.ask.0) / rust_decimal::Decimal::TWO;
            let unrealized = (mid - avg_entry.0) * pos.qty;
            pos.unrealized_pnl = Some(unrealized);
        }

        pos
    }

    fn exchange_health(&self, exchange: &ExchangeInstance) -> ExchangeHealth {
        self.exchange_health
            .get(exchange)
            .copied()
            .unwrap_or_default()
    }

    fn order(&self, client_id: &ClientOrderId) -> Option<&LiveOrder> {
        self.orders.get(client_id)
    }

    fn now_ms(&self) -> i64 {
        self.now_ms
    }

    fn log_info(&self, msg: &str) {
        tracing::info!("{}", msg);
    }

    fn log_warn(&self, msg: &str) {
        tracing::warn!("{}", msg);
    }

    fn log_error(&self, msg: &str) {
        tracing::error!("{}", msg);
    }

    fn log_debug(&self, msg: &str) {
        tracing::debug!("{}", msg);
    }
}
