//! Engine runner - polling loop and event dispatch.
//!
//! This module contains the main event loop that:
//! 1. Polls exchanges for fills and quotes
//! 2. Synthesizes canonical events
//! 3. Dispatches events to strategies
//! 4. Executes strategy commands
//! 5. Syncs fills to upstream API for PnL tracking

use crate::compat::sleep;
use crate::poll_guard::{PollGuard, PollOutcome};
use bot_core::{
    CancelAll, CancelOrder, ClientOrderId, Command, Event, Exchange, ExchangeHealth,
    ExchangeInstance, Fill, InstrumentId, OrderAcceptedEvent, OrderCanceledEvent,
    OrderCompletedEvent, OrderFilledEvent, OrderInput, OrderRejectedEvent, PlaceOrder,
    PlaceOrderResult, Qty, Quote, QuoteEvent,
};
use futures::channel::mpsc;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "native")]
use crate::account_syncer::{AccountSyncer, AccountSyncerConfig};
#[cfg(feature = "native")]
use crate::sync_traits::{AccountSync, TradeSync};
#[cfg(feature = "native")]
use crate::trade_syncer::{TradeSyncer, TradeSyncerConfig};
use crate::Engine;

use serde::Serialize;

/// Individual fill record for backtest results (serialized to frontend)
#[derive(Debug, Clone, Serialize)]
pub struct BacktestFill {
    pub ts_ms: i64,
    pub price: String,
    pub qty: String,
    pub side: String,
    pub fee: String,
}

/// Backtest result summary for JSON output
#[derive(Debug, Clone, Serialize)]
pub struct BacktestResult {
    pub fills: Vec<BacktestFill>,
    pub trade_count: usize,
    pub final_position_qty: String,
    pub avg_entry_price: Option<String>,
    pub realized_pnl: String,
    pub unrealized_pnl: Option<String>,
    pub total_fees: String,
    pub total_volume: String,
    pub net_pnl: String,
    pub exit_reason: Option<String>,
}

/// Message from polling tasks to the main loop
#[derive(Debug)]
pub enum PollResult {
    Fills {
        instance: ExchangeInstance,
        fills: Vec<Fill>,
    },
    Quotes {
        instance: ExchangeInstance,
        quotes: Vec<Quote>,
    },
    ExchangeHealth {
        instance: ExchangeInstance,
        health: ExchangeHealth,
    },
    Error {
        instance: ExchangeInstance,
        error: String,
    },
}

/// Runner configuration
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// Minimum delay between polls (ms)
    pub min_poll_delay_ms: u64,
    /// Initial backoff delay (ms)
    pub initial_backoff_ms: u64,
    /// Max backoff delay (ms)
    pub max_backoff_ms: u64,
    /// Backoff multiplier
    pub backoff_multiplier: f64,
    /// Quote polling interval (ms) - separate from fills
    pub quote_poll_interval_ms: u64,
    /// Cleanup delay after strategy stop (ms) - time to wait for cleanup commands to complete
    pub cleanup_delay_ms: u64,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            min_poll_delay_ms: 500,
            initial_backoff_ms: 1000,
            max_backoff_ms: 30_000,
            backoff_multiplier: 2.0,
            quote_poll_interval_ms: 1000,
            cleanup_delay_ms: 5000, // 5 seconds for cleanup
        }
    }
}

/// Trading statistics for meta logging (Poll mode)
#[derive(Debug, Default, Clone)]
pub struct TradingStats {
    /// Total orders placed
    pub orders_placed: u32,
    /// Total orders filled
    pub orders_filled: u32,
    /// Total volume traded (notional)
    pub volume_traded: Decimal,
    /// Total fees paid
    pub total_fees: Decimal,
    /// Realized PnL (from closed positions)
    pub realized_pnl: Decimal,
    /// Last meta log time
    pub last_meta_log_ms: i64,
}

/// Engine runner - drives the main event loop
pub struct EngineRunner {
    config: RunnerConfig,
    engine: Engine,
    exchanges: HashMap<ExchangeInstance, Arc<dyn Exchange>>,
    instruments: Vec<InstrumentId>,
    shutdown_rx: Option<mpsc::UnboundedReceiver<()>>,
    shutdown_tx: mpsc::UnboundedSender<()>,
    /// Flag to indicate graceful shutdown requested by strategy
    should_shutdown: bool,
    /// Shutdown reason (from strategy stop or external signal)
    shutdown_reason: Option<String>,
    /// Optional trade syncer for upstream API syncing (Poll mechanism)
    #[cfg(feature = "native")]
    trade_syncer: Option<Box<dyn TradeSync>>,
    /// Optional account syncer for upstream API syncing (Snapshot mechanism)
    #[cfg(feature = "native")]
    account_syncer: Option<Box<dyn AccountSync>>,
    /// Current mid price for syncing (updated from quotes)
    current_mid_price: Option<Decimal>,
    /// Trading statistics for meta logging
    stats: TradingStats,
    /// Poll guard for fills (per-exchange backoff + error classification)
    fills_guard: PollGuard,
    /// Poll guard for quotes (per-exchange backoff + error classification)
    quotes_guard: PollGuard,
}

impl EngineRunner {
    pub fn new(engine: Engine, config: RunnerConfig) -> Self {
        let (shutdown_tx, shutdown_rx) = mpsc::unbounded();
        let fills_guard = PollGuard::new("fills", &config);
        let quotes_guard = PollGuard::new("quotes", &config);

        Self {
            config,
            engine,
            exchanges: HashMap::new(),
            instruments: Vec::new(),
            shutdown_rx: Some(shutdown_rx),
            shutdown_tx,
            should_shutdown: false,
            shutdown_reason: None,
            #[cfg(feature = "native")]
            trade_syncer: None,
            #[cfg(feature = "native")]
            account_syncer: None,
            current_mid_price: None,
            stats: TradingStats::default(),
            fills_guard,
            quotes_guard,
        }
    }

    /// Set trade syncer configuration for upstream API syncing
    #[cfg(feature = "native")]
    pub fn with_syncer(mut self, syncer_config: TradeSyncerConfig) -> Self {
        match TradeSyncer::new(syncer_config) {
            Ok(syncer) => {
                tracing::info!("[EngineRunner] Trade syncer enabled");
                self.trade_syncer = Some(Box::new(syncer));
            }
            Err(e) => {
                tracing::warn!("[EngineRunner] Failed to create trade syncer: {}", e);
            }
        }
        self
    }

    /// Set trade syncer directly (for external configuration or testing)
    #[cfg(feature = "native")]
    pub fn set_trade_syncer(&mut self, syncer: Box<dyn TradeSync>) {
        self.trade_syncer = Some(syncer);
    }

    /// Set account syncer configuration for upstream API syncing (Snapshot mechanism)
    #[cfg(feature = "native")]
    pub fn with_account_syncer(mut self, syncer_config: AccountSyncerConfig) -> Self {
        match AccountSyncer::new(syncer_config) {
            Ok(syncer) => {
                tracing::info!("[EngineRunner] Account syncer enabled");
                self.account_syncer = Some(Box::new(syncer));
            }
            Err(e) => {
                tracing::warn!("[EngineRunner] Failed to create account syncer: {}", e);
            }
        }
        self
    }

    /// Set account syncer directly (for external configuration or testing)
    #[cfg(feature = "native")]
    pub fn set_account_syncer(&mut self, syncer: Box<dyn AccountSync>) {
        self.account_syncer = Some(syncer);
    }

    /// Add an exchange to poll
    pub fn add_exchange(&mut self, exchange: Arc<dyn Exchange>) {
        let instance = exchange.instance();
        self.exchanges.insert(instance, exchange);
    }

    /// Add an instrument to poll quotes for
    pub fn add_instrument(&mut self, instrument: InstrumentId) {
        self.instruments.push(instrument);
    }

    /// Get shutdown sender (for external shutdown trigger)
    pub fn shutdown_handle(&self) -> mpsc::UnboundedSender<()> {
        self.shutdown_tx.clone()
    }

    /// Get reference to the underlying Engine.
    /// Use this after run() completes to access positions, PnL, and other state.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Get the shutdown reason if the engine was stopped by the strategy.
    pub fn shutdown_reason(&self) -> Option<&str> {
        self.shutdown_reason.as_deref()
    }

    /// Compute backtest results from engine state.
    /// Call after run() completes to get summary statistics.
    pub fn get_backtest_results(&self, instrument: &InstrumentId) -> BacktestResult {
        let fills = self.engine.get_fills();
        let position = self.engine.position(instrument);

        // Compute volume from fills
        let mut total_volume = Decimal::ZERO;
        for fill in fills {
            let notional = fill.qty.0 * fill.price.0;
            total_volume += notional;
        }

        // Compute net PnL
        let net_pnl = position.realized_pnl + position.unrealized_pnl.unwrap_or(Decimal::ZERO)
            - position.total_fees;

        // Convert fills to serializable format
        let backtest_fills: Vec<BacktestFill> = fills
            .iter()
            .map(|f| BacktestFill {
                ts_ms: f.ts,
                price: f.price.0.to_string(),
                qty: f.qty.0.to_string(),
                side: format!("{:?}", f.side),
                fee: f.fee.asset.0.to_string(),
            })
            .collect();

        BacktestResult {
            trade_count: backtest_fills.len(),
            fills: backtest_fills,
            final_position_qty: position.qty.to_string(),
            avg_entry_price: position.avg_entry_px.map(|p| p.0.to_string()),
            realized_pnl: position.realized_pnl.to_string(),
            unrealized_pnl: position.unrealized_pnl.map(|p| p.to_string()),
            total_fees: position.total_fees.to_string(),
            total_volume: total_volume.to_string(),
            net_pnl: net_pnl.to_string(),
            exit_reason: self.shutdown_reason.clone(),
        }
    }

    /// Run the main event loop
    pub async fn run(&mut self) {
        tracing::info!("Starting engine runner...");

        // Initialize all exchanges (validate connections, vault ownership, etc.)
        for (instance, exchange) in &self.exchanges {
            if let Err(e) = exchange.init().await {
                tracing::error!("Exchange {} init failed: {}", instance, e);
                self.shutdown_reason = Some(format!("exchange_init_failed:{}", e));

                // Use the existing shutdown path: stop strategies → cleanup
                let stop_cmds = self.engine.stop_strategies();
                self.execute_commands(stop_cmds).await;
                return;
            }
        }

        // Start strategies
        let start_cmds = self.engine.start_strategies();
        self.execute_commands(start_cmds).await;

        // Take shutdown receiver
        let mut shutdown_rx = self.shutdown_rx.take().expect("run called twice");

        // Backtest completion detection constant
        const MAX_EMPTY_POLLS: u32 = 3;

        let mut _loop_iteration: u32 = 0;

        loop {
            _loop_iteration += 1;

            // Log every iteration in WASM for debugging
            #[cfg(target_arch = "wasm32")]
            if loop_iteration <= 500 || loop_iteration % 10 == 0 {
                web_sys::console::log_1(
                    &format!("[WASM Runner] Loop iteration {}", loop_iteration).into(),
                );
            }
            // Check for shutdown from strategy stop
            if self.should_shutdown {
                tracing::info!("Strategy requested shutdown");
                break;
            }

            // Check for external shutdown signal
            match shutdown_rx.try_next() {
                Ok(Some(_)) | Ok(None) => {
                    tracing::info!("Shutdown signal received");
                    break;
                }
                Err(_) => {} // Channel empty
            }

            // Snapshot exchanges for thread safety
            let exchanges_snapshot: Vec<(ExchangeInstance, Arc<dyn Exchange>)> = self
                .exchanges
                .iter()
                .map(|(i, e)| (i.clone(), e.clone()))
                .collect();

            // Determine sync mechanism from strategy
            let sync_mechanism = self.engine.sync_mechanism();

            // Poll all exchanges based on sync mechanism
            for (instance, exchange) in &exchanges_snapshot {
                match sync_mechanism {
                    bot_core::SyncMechanism::Poll => {
                        // Incremental: poll fills via PollGuard
                        match self
                            .fills_guard
                            .execute(|| exchange.poll_user_fills(None))
                            .await
                        {
                            PollOutcome::Data(fills) => {
                                // Update health to active
                                self.engine
                                    .set_exchange_health(instance, ExchangeHealth::Active);

                                // Process fills
                                for fill in fills {
                                    // Only dispatch fills we can attribute to a client_id (either directly via cloid
                                    // or via exchange_order_id -> client_id mapping).
                                    let client_id = self.resolve_fill_client_id(&fill);
                                    if client_id.is_none() {
                                        // Still add to syncer even if we can't attribute to client_id
                                        // (might be from a different source)
                                        #[cfg(feature = "native")]
                                        if let Some(ref mut syncer) = self.trade_syncer {
                                            syncer.add_fill(fill.clone());
                                        }
                                        continue;
                                    }

                                    // Apply fill to local order state (dedupe + completion detection)
                                    let client_id = client_id.expect("checked above");
                                    if !self
                                        .apply_fill_and_emit_events(instance, &client_id, &fill)
                                        .await
                                    {
                                        continue;
                                    }

                                    // Add fill to syncer for upstream API sync
                                    #[cfg(feature = "native")]
                                    if let Some(ref mut syncer) = self.trade_syncer {
                                        syncer.add_fill(fill.clone());
                                    }

                                    tracing::info!(
                                        "Fill: {} {} {} @ {} (oid={:?} tid={})",
                                        fill.side,
                                        fill.qty,
                                        fill.instrument,
                                        fill.price,
                                        fill.exchange_order_id,
                                        fill.trade_id
                                    );

                                    // Update trading stats
                                    self.stats.orders_filled += 1;
                                    self.stats.volume_traded += fill.qty.0 * fill.price.0;
                                    self.stats.total_fees += fill.fee.amount;
                                }
                            }
                            PollOutcome::Empty => {} // backoff already applied by guard
                            PollOutcome::Degraded(_) | PollOutcome::Fatal(_) => {
                                self.engine
                                    .set_exchange_health(instance, ExchangeHealth::Halted);
                            }
                        }
                    }
                    bot_core::SyncMechanism::Snapshot => {
                        // Absolute: poll account state via PollGuard
                        // AccountState doesn't impl HasItems (single result, not a collection),
                        // so we wrap it in a Vec for the guard and unwrap on the other side.
                        match self
                            .fills_guard
                            .execute(|| async {
                                exchange.poll_account_state().await.map(|state| vec![state])
                            })
                            .await
                        {
                            PollOutcome::Data(mut states) => {
                                let account_state = states.remove(0);

                                // Update health to active
                                self.engine
                                    .set_exchange_health(instance, ExchangeHealth::Active);

                                // Update engine positions from snapshot
                                for pos_snapshot in &account_state.positions {
                                    self.engine.apply_snapshot(
                                        &pos_snapshot.instrument,
                                        pos_snapshot.qty,
                                        pos_snapshot.avg_entry_px,
                                        pos_snapshot.unrealized_pnl,
                                    );

                                    tracing::debug!(
                                        "Position snapshot: {} qty={} entry={:?} pnl={:?}",
                                        pos_snapshot.instrument,
                                        pos_snapshot.qty,
                                        pos_snapshot.avg_entry_px,
                                        pos_snapshot.unrealized_pnl
                                    );
                                }

                                // Sync to backend via AccountSyncer
                                #[cfg(feature = "native")]
                                if let Some(ref mut syncer) = self.account_syncer {
                                    if syncer.should_sync() {
                                        if let Err(e) = syncer.sync(&account_state, false, "").await
                                        {
                                            tracing::error!(
                                                "[{}] Account sync failed: {}",
                                                instance,
                                                e
                                            );
                                        }
                                    }
                                }

                                tracing::info!(
                                    "Account snapshot: positions={} account_value={:?} pnl={:?}",
                                    account_state.positions.len(),
                                    account_state.account_value,
                                    account_state.unrealized_pnl
                                );
                            }
                            PollOutcome::Empty => {} // backoff already applied by guard
                            PollOutcome::Degraded(_) | PollOutcome::Fatal(_) => {
                                self.engine
                                    .set_exchange_health(instance, ExchangeHealth::Halted);
                            }
                        }
                    }
                }
            }

            // Poll quotes via PollGuard
            if !self.instruments.is_empty() {
                for (instance, exchange) in &exchanges_snapshot {
                    match self
                        .quotes_guard
                        .execute(|| exchange.poll_quotes(&self.instruments))
                        .await
                    {
                        PollOutcome::Data(quotes) => {
                            self.engine
                                .set_exchange_health(instance, ExchangeHealth::Active);

                            for quote in quotes {
                                // Update engine quote state
                                self.engine.update_quote(quote.clone());

                                // Store mid price for syncing
                                let mid = (quote.bid.0 + quote.ask.0) / Decimal::TWO;
                                self.current_mid_price = Some(mid);
                                tracing::debug!(
                                    "[EngineRunner] Updated mid price: {} (bid={}, ask={})",
                                    mid,
                                    quote.bid,
                                    quote.ask
                                );

                                // Create and dispatch quote event
                                let event = Event::Quote(QuoteEvent {
                                    exchange: instance.exchange_id.clone(),
                                    instrument: quote.instrument.clone(),
                                    bid: quote.bid,
                                    ask: quote.ask,
                                    ts: quote.ts,
                                });

                                self.handle_event(event).await;
                            }
                        }
                        PollOutcome::Empty => {} // backoff already applied by guard
                        PollOutcome::Degraded(_) | PollOutcome::Fatal(_) => {
                            self.engine
                                .set_exchange_health(instance, ExchangeHealth::Halted);
                        }
                    }
                }
            }

            // Backtest completion detection: ONLY when min_poll_delay_ms == 0 (backtest mode)
            // Live bots (delay > 0) must never exit from empty quote polls — API hiccups are transient.
            if self.config.min_poll_delay_ms == 0
                && self.quotes_guard.looks_exhausted(MAX_EMPTY_POLLS)
            {
                tracing::info!("[EngineRunner] Backtest complete: no more quotes available");

                #[cfg(target_arch = "wasm32")]
                web_sys::console::log_1(&"[WASM Runner] Backtest complete - exiting".into());

                self.shutdown_reason = Some("Backtest complete - all quotes processed".to_string());
                break;
            }

            // Periodic trade sync to upstream API
            #[cfg(feature = "native")]
            if let Some(ref mut syncer) = self.trade_syncer {
                if syncer.should_sync() {
                    match syncer.sync(self.current_mid_price, false, "").await {
                        Ok(result) => {
                            if let Some(pnl) = result.pnl {
                                tracing::debug!("[EngineRunner] Sync success: pnl={:.4}", pnl);
                            }
                        }
                        Err(e) => {
                            tracing::warn!("[EngineRunner] Sync failed: {}", e);
                        }
                    }
                }
            }

            // Periodic meta log (every 30 seconds)
            let now_ms = bot_core::now_ms();
            if now_ms - self.stats.last_meta_log_ms >= 30_000 {
                self.stats.last_meta_log_ms = now_ms;

                // Build position string for all instruments
                let pos_str: String = self
                    .instruments
                    .iter()
                    .map(|i| {
                        let p = self.engine.position(i);
                        format!("{}:{:.4}", i, p.qty)
                    })
                    .collect::<Vec<_>>()
                    .join("/");

                let total_upnl: rust_decimal::Decimal = self
                    .instruments
                    .iter()
                    .map(|i| self.engine.position(i).unrealized_pnl.unwrap_or_default())
                    .sum();

                let sync_mechanism = self.engine.sync_mechanism();
                if sync_mechanism == bot_core::SyncMechanism::Poll {
                    // Poll mode: full stats
                    tracing::info!(
                        "[META] pos={} orders={}/{} vol={:.2} fees={:.4} u_pnl={:.4}",
                        pos_str,
                        self.stats.orders_placed,
                        self.stats.orders_filled,
                        self.stats.volume_traded,
                        self.stats.total_fees,
                        total_upnl
                    );
                } else {
                    // Snapshot mode: just position
                    tracing::info!("[META] pos={} u_pnl={:.4} (snapshot)", pos_str, total_upnl);
                }
            }

            // Minimum delay between loops (skip entirely for backtesting when delay is 0)
            if self.config.min_poll_delay_ms > 0 {
                sleep(Duration::from_millis(self.config.min_poll_delay_ms)).await;
            } else {
                // Yield to event loop without timer overhead (crucial for WASM backtesting)
                crate::compat::yield_now().await;
            }
        }

        // Stop strategies on shutdown (calls on_stop for any not yet stopped)
        let stop_cmds = self.engine.stop_strategies();
        self.execute_commands(stop_cmds).await;

        // Wait for cleanup commands to complete (e.g., CancelAll orders)
        if self.config.cleanup_delay_ms > 0 {
            tracing::info!(
                "Waiting {}ms for cleanup to complete...",
                self.config.cleanup_delay_ms
            );
            sleep(Duration::from_millis(self.config.cleanup_delay_ms)).await;
        }

        // Final sync to upstream API on shutdown
        #[cfg(feature = "native")]
        if let Some(ref mut syncer) = self.trade_syncer {
            let reason = self
                .shutdown_reason
                .as_deref()
                .unwrap_or("shutdown:graceful");
            tracing::info!(
                "[EngineRunner] Performing final sync before shutdown with reason: {}",
                reason
            );
            match syncer.shutdown_sync(self.current_mid_price, reason).await {
                Ok(result) => {
                    tracing::info!("[EngineRunner] Final sync complete: pnl={:?}", result.pnl);
                }
                Err(e) => {
                    tracing::warn!("[EngineRunner] Final sync failed: {}", e);
                }
            }
        }

        // Output backtest results as JSON to stdout if we have instruments
        // This allows supurr_cli (and other callers) to parse results
        if let Some(first_instrument) = self.instruments.first() {
            let results = self.get_backtest_results(first_instrument);
            // Print JSON to stdout on a single line for easy parsing
            if let Ok(json) = serde_json::to_string(&results) {
                println!("{}", json);
            }
        }

        tracing::info!("Engine runner stopped");
    }

    async fn handle_event(&mut self, event: Event) {
        use std::collections::VecDeque;

        let mut queue: VecDeque<Event> = VecDeque::new();
        queue.push_back(event);

        while let Some(ev) = queue.pop_front() {
            let cmds = self.engine.dispatch_event(&ev);
            let followups = self.execute_commands(cmds).await;
            for f in followups {
                queue.push_back(f);
            }
        }
    }

    fn resolve_fill_client_id(&self, fill: &Fill) -> Option<ClientOrderId> {
        if let Some(cid) = fill.client_id.clone() {
            return Some(cid);
        }

        if let Some(ref eid) = fill.exchange_order_id {
            if let Some(cid) = self.engine.order_manager().client_id_from_exchange_id(eid) {
                return Some(cid.clone());
            }
        }

        None
    }

    async fn apply_fill_and_emit_events(
        &mut self,
        instance: &ExchangeInstance,
        client_id: &ClientOrderId,
        fill: &Fill,
    ) -> bool {
        // Apply fill to order manager (dedupe).
        let is_new = self.engine.order_manager_mut().apply_fill(
            client_id,
            &fill.trade_id,
            fill.qty,
            fill.price,
        );

        if !is_new {
            return false;
        }

        // Calculate net_qty FIRST: for spot BUY, deduct fee if fee is in base asset
        // This must happen before position update so position reflects actual holdings
        let net_qty = if let Some(meta) = self.engine.instrument_meta(&fill.instrument) {
            if meta.kind == bot_core::InstrumentKind::Spot
                && fill.side == bot_core::OrderSide::Buy
                && fill.fee.asset == meta.base_asset
            {
                // Spot BUY: fee is deducted from received base asset
                let nq = Qty::new((fill.qty.0 - fill.fee.amount).max(Decimal::ZERO));
                tracing::debug!(
                    "Spot BUY fee deduction: gross={} fee={} net={}",
                    fill.qty,
                    fill.fee.amount,
                    nq
                );
                nq
            } else {
                fill.qty
            }
        } else {
            fill.qty
        };

        // Update position using NET qty (actual holdings after fee)
        self.engine
            .apply_position_fill(&fill.instrument, fill.side, net_qty, fill.price);

        // Track fee from fill
        self.engine
            .apply_fill_fee(&fill.instrument, fill.fee.amount);

        // Emit OrderFilled
        let filled_event = Event::OrderFilled(OrderFilledEvent {
            exchange: instance.exchange_id.clone(),
            instrument: fill.instrument.clone(),
            client_id: client_id.clone(),
            trade_id: fill.trade_id.clone(),
            side: fill.side,
            price: fill.price,
            qty: fill.qty,
            net_qty,
            fee: fill.fee.clone(),
            ts: fill.ts,
        });

        // Record fill in engine's fill history (for backtesting/reporting)
        if let Event::OrderFilled(ref fill_event) = filled_event {
            self.engine.record_fill(fill_event.clone());
        }

        self.handle_event(filled_event).await;

        // Emit OrderCompleted if order is now complete.
        let is_complete = self.engine.order_manager().is_complete(client_id);
        if is_complete {
            if let Some(order) = self.engine.order_manager().get(client_id).cloned() {
                let completed_event = Event::OrderCompleted(OrderCompletedEvent {
                    exchange: instance.exchange_id.clone(),
                    instrument: order.instrument.clone(),
                    client_id: client_id.clone(),
                    filled_qty: order.filled_qty,
                    avg_fill_px: order.avg_fill_px,
                    ts: bot_core::now_ms(),
                });
                self.handle_event(completed_event).await;
            }
        }

        true
    }

    async fn execute_commands(&mut self, cmds: Vec<Command>) -> Vec<Event> {
        let mut followups: Vec<Event> = Vec::new();

        for cmd in cmds {
            match cmd {
                Command::PlaceOrder(c) => {
                    let mut evs = self.execute_place(c).await;
                    followups.append(&mut evs);
                }
                Command::PlaceOrders(orders) => {
                    let mut evs = self.execute_place_batch(orders).await;
                    followups.append(&mut evs);
                }
                Command::CancelOrder(c) => {
                    if let Some(ev) = self.execute_cancel(c).await {
                        followups.push(ev);
                    }
                }
                Command::CancelAll(c) => {
                    let mut evs = self.execute_cancel_all(c).await;
                    followups.append(&mut evs);
                }
                Command::StopStrategy(stop) => {
                    // Strategy stop is handled internally by the engine (on_stop already called)
                    tracing::info!("Strategy {} stopped: {}", stop.strategy_id.0, stop.reason);
                    // Capture the actual stop reason from strategy
                    self.shutdown_reason = Some(stop.reason.clone());
                    // Signal graceful shutdown
                    self.should_shutdown = true;
                }
            }
        }

        followups
    }

    async fn execute_place(&mut self, cmd: PlaceOrder) -> Vec<Event> {
        #[cfg(feature = "wasm")]
        web_sys::console::log_1(
            &format!(
                "[Runner] execute_place called: instrument={}, exchange={:?}",
                cmd.instrument, cmd.exchange
            )
            .into(),
        );

        let Some(exchange) = self.exchanges.get(&cmd.exchange).cloned() else {
            #[cfg(feature = "wasm")]
            {
                let available: Vec<_> = self.exchanges.keys().collect();
                web_sys::console::log_1(
                    &format!(
                        "[Runner] Exchange NOT FOUND: {:?}, available: {:?}",
                        cmd.exchange, available
                    )
                    .into(),
                );
            }
            return vec![];
        };

        #[cfg(feature = "wasm")]
        web_sys::console::log_1(&format!("[Runner] Exchange found, proceeding with order").into());

        let (market_index, rounded_price, rounded_qty) =
            match self.engine.instrument_meta(&cmd.instrument) {
                Some(meta) => {
                    let rp = meta.round_price(cmd.price);
                    let rq = meta.round_qty(cmd.qty);
                    tracing::debug!(
                        "Rounding: price {} -> {}, qty {} -> {}",
                        cmd.price,
                        rp,
                        cmd.qty,
                        rq
                    );
                    (meta.market_index.clone(), rp, rq)
                }
                None => {
                    tracing::warn!(
                        "PlaceOrder ignored: no InstrumentMeta for {}",
                        cmd.instrument
                    );
                    return vec![Event::OrderRejected(OrderRejectedEvent {
                        exchange: cmd.exchange.exchange_id.clone(),
                        instrument: cmd.instrument.clone(),
                        client_id: cmd.client_id.clone(),
                        reason: "missing instrument meta".to_string(),
                        ts: bot_core::now_ms(),
                    })];
                }
            };

        // Create/track the order locally before submitting (so cancels can resolve instrument).
        self.engine.order_manager_mut().create_order(
            cmd.client_id.clone(),
            cmd.instrument.clone(),
            cmd.side,
            rounded_price,
            rounded_qty,
        );

        // Build OrderInput for the batch API
        let order_input = OrderInput {
            instrument: cmd.instrument.clone(),
            market_index,
            client_id: cmd.client_id.clone(),
            side: cmd.side,
            price: rounded_price,
            qty: rounded_qty,
            tif: cmd.tif,
            post_only: cmd.post_only,
            reduce_only: cmd.reduce_only,
        };

        match exchange.place_orders(&[order_input]).await {
            Ok(results) => {
                // Get the first (and only) result
                match results.into_iter().next() {
                    Some(PlaceOrderResult::Accepted {
                        exchange_order_id,
                        filled_qty,
                        avg_fill_px,
                    }) => {
                        self.engine
                            .order_manager_mut()
                            .accept_order(&cmd.client_id, exchange_order_id.clone());

                        // Update trading stats
                        self.stats.orders_placed += 1;

                        // Always emit OrderAccepted first
                        let accepted_event = Event::OrderAccepted(OrderAcceptedEvent {
                            exchange: cmd.exchange.exchange_id.clone(),
                            instrument: cmd.instrument.clone(),
                            client_id: cmd.client_id.clone(),
                            exchange_order_id,
                            ts: bot_core::now_ms(),
                        });

                        // For Snapshot strategies with IOC fill, also emit OrderCompleted
                        // For Poll strategies, fills come from userFills polling
                        let use_ioc_fill =
                            self.engine.sync_mechanism() == bot_core::SyncMechanism::Snapshot;

                        if use_ioc_fill {
                            if let (Some(qty), Some(px)) = (&filled_qty, &avg_fill_px) {
                                // Emit OrderAccepted first, then OrderCompleted
                                vec![
                                    accepted_event,
                                    Event::OrderCompleted(OrderCompletedEvent {
                                        exchange: cmd.exchange.exchange_id.clone(),
                                        instrument: cmd.instrument.clone(),
                                        client_id: cmd.client_id.clone(),
                                        filled_qty: *qty,
                                        avg_fill_px: Some(*px),
                                        ts: bot_core::now_ms(),
                                    }),
                                ]
                            } else {
                                vec![accepted_event]
                            }
                        } else {
                            vec![accepted_event]
                        }
                    }
                    Some(PlaceOrderResult::Rejected { reason }) => {
                        self.engine.order_manager_mut().reject_order(&cmd.client_id);
                        vec![Event::OrderRejected(OrderRejectedEvent {
                            exchange: cmd.exchange.exchange_id.clone(),
                            instrument: cmd.instrument.clone(),
                            client_id: cmd.client_id.clone(),
                            reason,
                            ts: bot_core::now_ms(),
                        })]
                    }
                    None => {
                        self.engine.order_manager_mut().reject_order(&cmd.client_id);
                        vec![Event::OrderRejected(OrderRejectedEvent {
                            exchange: cmd.exchange.exchange_id.clone(),
                            instrument: cmd.instrument.clone(),
                            client_id: cmd.client_id.clone(),
                            reason: "No result returned from place_orders".to_string(),
                            ts: bot_core::now_ms(),
                        })]
                    }
                }
            }
            Err(e) => {
                self.engine.order_manager_mut().reject_order(&cmd.client_id);
                vec![Event::OrderRejected(OrderRejectedEvent {
                    exchange: cmd.exchange.exchange_id.clone(),
                    instrument: cmd.instrument.clone(),
                    client_id: cmd.client_id.clone(),
                    reason: e.to_string(),
                    ts: bot_core::now_ms(),
                })]
            }
        }
    }

    /// Execute a batch of place orders in a single API call
    async fn execute_place_batch(&mut self, orders: Vec<PlaceOrder>) -> Vec<Event> {
        if orders.is_empty() {
            return Vec::new();
        }

        // All orders should be for the same exchange
        let exchange_instance = &orders[0].exchange;
        let exchange = match self.exchanges.get(exchange_instance) {
            Some(e) => e.clone(),
            None => {
                return orders
                    .iter()
                    .map(|cmd| {
                        Event::OrderRejected(OrderRejectedEvent {
                            exchange: cmd.exchange.exchange_id.clone(),
                            instrument: cmd.instrument.clone(),
                            client_id: cmd.client_id.clone(),
                            reason: "Exchange not found".to_string(),
                            ts: bot_core::now_ms(),
                        })
                    })
                    .collect();
            }
        };

        // Build order inputs and track orders locally
        let mut order_inputs: Vec<OrderInput> = Vec::with_capacity(orders.len());
        let mut cmd_metadata: Vec<(PlaceOrder, bool)> = Vec::with_capacity(orders.len()); // (cmd, valid)

        for cmd in orders {
            let meta_result = self.engine.instrument_meta(&cmd.instrument).map(|meta| {
                let rp = meta.round_price(cmd.price);
                let rq = meta.round_qty(cmd.qty);
                (meta.market_index.clone(), rp, rq)
            });

            match meta_result {
                Some((market_index, rounded_price, rounded_qty)) => {
                    // Create/track the order locally before submitting
                    self.engine.order_manager_mut().create_order(
                        cmd.client_id.clone(),
                        cmd.instrument.clone(),
                        cmd.side,
                        rounded_price,
                        rounded_qty,
                    );

                    order_inputs.push(OrderInput {
                        instrument: cmd.instrument.clone(),
                        market_index,
                        client_id: cmd.client_id.clone(),
                        side: cmd.side,
                        price: rounded_price,
                        qty: rounded_qty,
                        tif: cmd.tif,
                        post_only: cmd.post_only,
                        reduce_only: cmd.reduce_only,
                    });
                    cmd_metadata.push((cmd, true));
                }
                None => {
                    tracing::warn!(
                        "PlaceOrder ignored: no InstrumentMeta for {}",
                        cmd.instrument
                    );
                    cmd_metadata.push((cmd, false));
                }
            }
        }

        // Collect events for invalid orders first
        let mut events: Vec<Event> = cmd_metadata
            .iter()
            .filter(|(_, valid)| !valid)
            .map(|(cmd, _)| {
                Event::OrderRejected(OrderRejectedEvent {
                    exchange: cmd.exchange.exchange_id.clone(),
                    instrument: cmd.instrument.clone(),
                    client_id: cmd.client_id.clone(),
                    reason: "missing instrument meta".to_string(),
                    ts: bot_core::now_ms(),
                })
            })
            .collect();

        if order_inputs.is_empty() {
            return events;
        }

        tracing::info!(
            "Executing batch place_orders with {} orders",
            order_inputs.len()
        );

        // Execute batch place_orders
        match exchange.place_orders(&order_inputs).await {
            Ok(results) => {
                // Match results with valid commands
                let valid_cmds: Vec<&PlaceOrder> = cmd_metadata
                    .iter()
                    .filter(|(_, valid)| *valid)
                    .map(|(cmd, _)| cmd)
                    .collect();

                for (i, result) in results.into_iter().enumerate() {
                    let cmd = valid_cmds.get(i);
                    if let Some(cmd) = cmd {
                        match result {
                            PlaceOrderResult::Accepted {
                                exchange_order_id,
                                filled_qty,
                                avg_fill_px,
                            } => {
                                self.engine
                                    .order_manager_mut()
                                    .accept_order(&cmd.client_id, exchange_order_id.clone());

                                // For Snapshot strategies with IOC orders, emit OrderCompleted from fill info
                                // For Poll strategies, skip this - fills come from userFills polling
                                let use_ioc_fill = self.engine.sync_mechanism()
                                    == bot_core::SyncMechanism::Snapshot;

                                if use_ioc_fill {
                                    if let (Some(qty), Some(px)) = (&filled_qty, &avg_fill_px) {
                                        events.push(Event::OrderCompleted(OrderCompletedEvent {
                                            exchange: cmd.exchange.exchange_id.clone(),
                                            instrument: cmd.instrument.clone(),
                                            client_id: cmd.client_id.clone(),
                                            filled_qty: *qty,
                                            avg_fill_px: Some(*px),
                                            ts: bot_core::now_ms(),
                                        }));
                                    } else {
                                        events.push(Event::OrderAccepted(OrderAcceptedEvent {
                                            exchange: cmd.exchange.exchange_id.clone(),
                                            instrument: cmd.instrument.clone(),
                                            client_id: cmd.client_id.clone(),
                                            exchange_order_id,
                                            ts: bot_core::now_ms(),
                                        }));
                                    }
                                } else {
                                    events.push(Event::OrderAccepted(OrderAcceptedEvent {
                                        exchange: cmd.exchange.exchange_id.clone(),
                                        instrument: cmd.instrument.clone(),
                                        client_id: cmd.client_id.clone(),
                                        exchange_order_id,
                                        ts: bot_core::now_ms(),
                                    }));
                                }
                            }
                            PlaceOrderResult::Rejected { reason } => {
                                self.engine.order_manager_mut().reject_order(&cmd.client_id);
                                events.push(Event::OrderRejected(OrderRejectedEvent {
                                    exchange: cmd.exchange.exchange_id.clone(),
                                    instrument: cmd.instrument.clone(),
                                    client_id: cmd.client_id.clone(),
                                    reason,
                                    ts: bot_core::now_ms(),
                                }));
                            }
                        }
                    }
                }
            }
            Err(e) => {
                // Reject all valid orders on error
                for (cmd, valid) in &cmd_metadata {
                    if *valid {
                        self.engine.order_manager_mut().reject_order(&cmd.client_id);
                        events.push(Event::OrderRejected(OrderRejectedEvent {
                            exchange: cmd.exchange.exchange_id.clone(),
                            instrument: cmd.instrument.clone(),
                            client_id: cmd.client_id.clone(),
                            reason: e.to_string(),
                            ts: bot_core::now_ms(),
                        }));
                    }
                }
            }
        }

        events
    }

    async fn execute_cancel(&mut self, cmd: CancelOrder) -> Option<Event> {
        let exchange = self.exchanges.get(&cmd.exchange)?.clone();

        let (instrument, exchange_order_id) = match self.engine.order_manager().get(&cmd.client_id)
        {
            Some(o) => (o.instrument.clone(), o.exchange_order_id.clone()),
            None => {
                tracing::warn!("CancelOrder ignored: unknown client_id {}", cmd.client_id);
                return None;
            }
        };

        let market_index = match self.engine.instrument_meta(&instrument) {
            Some(m) => m.market_index.clone(),
            None => {
                tracing::warn!("CancelOrder ignored: no InstrumentMeta for {}", instrument);
                return None;
            }
        };

        match exchange
            .cancel_order(
                &instrument,
                &market_index,
                &cmd.client_id,
                exchange_order_id.as_ref(),
            )
            .await
        {
            Ok(_) => {
                self.engine.order_manager_mut().cancel_order(&cmd.client_id);
                Some(Event::OrderCanceled(OrderCanceledEvent {
                    exchange: cmd.exchange.exchange_id.clone(),
                    instrument,
                    client_id: cmd.client_id.clone(),
                    reason: None,
                    ts: bot_core::now_ms(),
                }))
            }
            Err(e) => {
                tracing::warn!("CancelOrder failed for {}: {}", cmd.client_id, e);
                None
            }
        }
    }

    async fn execute_cancel_all(&mut self, cmd: CancelAll) -> Vec<Event> {
        tracing::info!(
            "Executing CancelAll for exchange={:?} instrument={:?}",
            cmd.exchange.exchange_id,
            cmd.instrument.as_ref().map(|i| i.to_string())
        );
        let out = Vec::new();
        let exchange = match self.exchanges.get(&cmd.exchange) {
            Some(e) => e.clone(),
            None => {
                tracing::warn!(
                    "CancelAll: exchange not found {:?}",
                    cmd.exchange.exchange_id
                );
                return out;
            }
        };

        let instruments: Vec<InstrumentId> = if let Some(i) = cmd.instrument.clone() {
            vec![i]
        } else {
            self.instruments.clone()
        };

        for instrument in instruments {
            let market_index = match self.engine.instrument_meta(&instrument) {
                Some(m) => m.market_index.clone(),
                None => continue,
            };

            tracing::info!("CancelAll: calling cancel_all_orders for {}", instrument);
            match exchange.cancel_all_orders(&instrument, &market_index).await {
                Ok(n) => {
                    tracing::info!(
                        "CancelAll: successfully canceled {} orders for {}",
                        n,
                        instrument
                    );
                }
                Err(e) => {
                    tracing::warn!("CancelAll failed for {}: {}", instrument, e);
                }
            }
        }

        out
    }
}

/// Spawn the runner as a background task
#[cfg(feature = "native")]
pub fn spawn_runner(
    engine: Engine,
    exchanges: Vec<Arc<dyn Exchange>>,
    instruments: Vec<InstrumentId>,
    config: RunnerConfig,
) -> (tokio::task::JoinHandle<()>, mpsc::UnboundedSender<()>) {
    spawn_runner_with_syncer(engine, exchanges, instruments, config, None, None)
}

/// Spawn the runner as a background task with optional trade syncer and account syncer
#[cfg(feature = "native")]
pub fn spawn_runner_with_syncer(
    engine: Engine,
    exchanges: Vec<Arc<dyn Exchange>>,
    instruments: Vec<InstrumentId>,
    config: RunnerConfig,
    syncer_config: Option<TradeSyncerConfig>,
    account_syncer_config: Option<AccountSyncerConfig>,
) -> (tokio::task::JoinHandle<()>, mpsc::UnboundedSender<()>) {
    let mut runner = EngineRunner::new(engine, config);

    // Configure trade syncer if provided (for Poll strategies)
    if let Some(syncer_cfg) = syncer_config {
        runner = runner.with_syncer(syncer_cfg);
    }

    // Configure account syncer if provided (for Snapshot strategies)
    if let Some(account_cfg) = account_syncer_config {
        runner = runner.with_account_syncer(account_cfg);
    }

    for exchange in exchanges {
        runner.add_exchange(exchange);
    }
    for instrument in instruments {
        runner.add_instrument(instrument);
    }

    let shutdown_handle = runner.shutdown_handle();

    let handle = tokio::spawn(async move {
        runner.run().await;
    });

    (handle, shutdown_handle)
}
