//! Main engine implementation.

use bot_core::{
    Command, Event, Exchange, ExchangeHealth, ExchangeInstance, InstrumentId, InstrumentMeta,
    OrderFilledEvent, OrderSide, Position, Price, Qty, Quote, StopStrategy, Strategy, StrategyId,
};
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::context::EngineContext;
use crate::inventory::InventoryLedger;
use crate::order_manager::OrderManager;

/// Engine configuration
pub struct EngineConfig {
    /// Minimum delay between userFills polls (ms)
    pub min_poll_delay_ms: u64,
    /// Backoff multiplier for errors
    pub backoff_multiplier: f64,
    /// Max backoff delay (ms)
    pub max_backoff_ms: u64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            min_poll_delay_ms: 500,
            backoff_multiplier: 2.0,
            max_backoff_ms: 30_000,
        }
    }
}

/// The main trading engine.
///
/// Owns:
/// - Order state (OrderManager)
/// - Inventory/balance state (InventoryLedger)
/// - Exchange adapters
/// - Strategies
/// - Exchange health state
pub struct Engine {
    #[allow(dead_code)]
    config: EngineConfig,

    // State
    order_manager: OrderManager,
    inventory: InventoryLedger,
    quotes: HashMap<InstrumentId, Quote>,
    positions: HashMap<InstrumentId, Position>,
    instruments: HashMap<InstrumentId, InstrumentMeta>,
    exchange_health: HashMap<ExchangeInstance, ExchangeHealth>,

    // Strategies that have been stopped (by their own request)
    stopped_strategies: HashSet<StrategyId>,

    // Components (to be wired up)
    exchanges: HashMap<ExchangeInstance, Arc<dyn Exchange>>,
    strategies: Vec<Box<dyn Strategy>>,

    // Fill history (for backtesting/reporting)
    fill_history: Vec<OrderFilledEvent>,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Self {
        Self {
            config,
            order_manager: OrderManager::new(),
            inventory: InventoryLedger::new(),
            quotes: HashMap::new(),
            positions: HashMap::new(),
            instruments: HashMap::new(),
            exchange_health: HashMap::new(),
            stopped_strategies: HashSet::new(),
            exchanges: HashMap::new(),
            strategies: Vec::new(),
            fill_history: Vec::new(),
        }
    }

    /// Register an exchange adapter
    pub fn register_exchange(&mut self, exchange: Arc<dyn Exchange>) {
        let instance = exchange.instance();
        self.exchange_health
            .insert(instance.clone(), ExchangeHealth::Active);
        self.exchanges.insert(instance, exchange);
    }

    /// Register an instrument
    pub fn register_instrument(&mut self, meta: InstrumentMeta) {
        self.instruments.insert(meta.instrument_id.clone(), meta);
    }

    /// Register a strategy
    pub fn register_strategy(&mut self, strategy: Box<dyn Strategy>) {
        self.strategies.push(strategy);
    }

    /// Dispatch an event to all strategies.
    ///
    /// Returns all commands emitted by strategies while handling this event.
    pub fn dispatch_event(&mut self, event: &Event) -> Vec<Command> {
        let now_ms = bot_core::now_ms();
        let num_strategies = self.strategies.len();
        let mut commands: Vec<Command> = Vec::new();
        let mut strategies_to_stop: Vec<(usize, String)> = Vec::new();

        for i in 0..num_strategies {
            let strategy_id = self.strategies[i].id().clone();

            // Skip stopped strategies
            if self.stopped_strategies.contains(&strategy_id) {
                continue;
            }

            // Create context from non-strategy state
            let mut ctx = EngineContext::new(
                &self.quotes,
                &self.instruments,
                &self.order_manager,
                &self.inventory,
                &self.positions,
                &self.exchange_health,
                now_ms,
            );

            // Borrow strategy mutably and dispatch
            self.strategies[i].on_event(&mut ctx, event);

            // ENGINE SAFETY CHECK: Detect funding errors (margin for perps, balance for spot)
            // If an order is rejected due to insufficient funds, stop the bot immediately
            // to prevent further failed orders and notify the user
            if let Event::OrderRejected(ref rejection) = event {
                let is_insufficient_margin = rejection.reason.contains("Insufficient margin");
                let is_insufficient_balance =
                    rejection.reason.contains("Insufficient spot balance");
                let is_min_value_error = rejection.reason.contains("minimum value");

                if is_insufficient_margin || is_insufficient_balance || is_min_value_error {
                    let error_type = if is_insufficient_margin {
                        "MARGIN"
                    } else if is_insufficient_balance {
                        "BALANCE"
                    } else {
                        "MIN_ORDER_VALUE"
                    };
                    tracing::warn!(
                        "🚨 INSUFFICIENT {} detected for strategy {} - stopping bot",
                        error_type,
                        strategy_id
                    );
                    let stop_reason = format!(
                        "Insufficient {} to place order. Last rejected order: {} - {}",
                        error_type.to_lowercase(),
                        rejection.client_id,
                        rejection.reason
                    );
                    strategies_to_stop.push((i, stop_reason.clone()));
                    commands.push(Command::StopStrategy(StopStrategy {
                        strategy_id: strategy_id.clone(),
                        reason: stop_reason,
                    }));
                }
                let is_api_wallet_expired = rejection.reason.contains("User or API Wallet");

                if is_api_wallet_expired {
                    let stop_reason = format!(
                        "API wallet expired for strategy {} - stopping bot",
                        strategy_id
                    );
                    tracing::warn!(stop_reason);
                    strategies_to_stop.push((i, stop_reason.clone()));
                    commands.push(Command::StopStrategy(StopStrategy {
                        strategy_id: strategy_id.clone(),
                        reason: stop_reason,
                    }));
                }
            }

            // Process any commands emitted by the strategy
            let (places, batches, cancels, cancel_alls, stop_requests) = ctx.take_commands();

            for cmd in places {
                tracing::debug!("Strategy {} emitted PlaceOrder: {:?}", strategy_id, cmd);
                commands.push(Command::PlaceOrder(cmd));
            }
            for batch in batches {
                tracing::debug!(
                    "Strategy {} emitted PlaceOrders batch: {} orders",
                    strategy_id,
                    batch.len()
                );
                commands.push(Command::PlaceOrders(batch));
            }
            for cmd in cancels {
                tracing::debug!("Strategy {} emitted CancelOrder: {:?}", strategy_id, cmd);
                commands.push(Command::CancelOrder(cmd));
            }
            for cmd in cancel_alls {
                tracing::debug!("Strategy {} emitted CancelAll: {:?}", strategy_id, cmd);
                commands.push(Command::CancelAll(cmd));
            }

            // Check for stop requests
            for stop_req in stop_requests {
                if stop_req.strategy_id == strategy_id {
                    strategies_to_stop.push((i, stop_req.reason.clone()));
                    commands.push(Command::StopStrategy(stop_req));
                }
            }
        }

        // Process stop requests - call on_stop for strategies that requested it
        for (idx, reason) in strategies_to_stop {
            let strategy_id = self.strategies[idx].id().clone();
            tracing::info!("Stopping strategy {} due to: {}", strategy_id.0, reason);

            let mut ctx = EngineContext::new(
                &self.quotes,
                &self.instruments,
                &self.order_manager,
                &self.inventory,
                &self.positions,
                &self.exchange_health,
                now_ms,
            );

            // Call on_stop
            self.strategies[idx].on_stop(&mut ctx);

            // Collect any commands from on_stop
            let (places, batches, cancels, cancel_alls, _) = ctx.take_commands();
            for cmd in places {
                commands.push(Command::PlaceOrder(cmd));
            }
            for batch in batches {
                commands.push(Command::PlaceOrders(batch));
            }
            for cmd in cancels {
                commands.push(Command::CancelOrder(cmd));
            }
            for cmd in cancel_alls {
                tracing::info!(
                    "on_stop emitted CancelAll for {:?}",
                    cmd.instrument.as_ref().map(|i| i.to_string())
                );
                commands.push(Command::CancelAll(cmd));
            }

            // Mark as stopped
            self.stopped_strategies.insert(strategy_id.clone());
            tracing::info!("Strategy {} marked as stopped", strategy_id.0);
        }

        commands
    }

    /// Update quote for an instrument
    pub fn update_quote(&mut self, quote: Quote) {
        let instrument = quote.instrument.clone();
        self.quotes.insert(instrument, quote);
    }

    /// Update exchange health
    pub fn set_exchange_health(&mut self, instance: &ExchangeInstance, health: ExchangeHealth) {
        self.exchange_health.insert(instance.clone(), health);
    }

    /// Start all strategies
    pub fn start_strategies(&mut self) -> Vec<Command> {
        let now_ms = bot_core::now_ms();
        let num_strategies = self.strategies.len();
        let mut commands: Vec<Command> = Vec::new();

        for i in 0..num_strategies {
            let mut ctx = EngineContext::new(
                &self.quotes,
                &self.instruments,
                &self.order_manager,
                &self.inventory,
                &self.positions,
                &self.exchange_health,
                now_ms,
            );
            self.strategies[i].on_start(&mut ctx);

            let (places, batches, cancels, cancel_alls, _stop_requests) = ctx.take_commands();
            for cmd in places {
                commands.push(Command::PlaceOrder(cmd));
            }
            for batch in batches {
                commands.push(Command::PlaceOrders(batch));
            }
            for cmd in cancels {
                commands.push(Command::CancelOrder(cmd));
            }
            for cmd in cancel_alls {
                commands.push(Command::CancelAll(cmd));
            }
        }

        commands
    }

    /// Stop all strategies
    pub fn stop_strategies(&mut self) -> Vec<Command> {
        let now_ms = bot_core::now_ms();
        let num_strategies = self.strategies.len();
        let mut commands: Vec<Command> = Vec::new();

        for i in 0..num_strategies {
            let strategy_id = self.strategies[i].id().clone();

            // Skip already stopped strategies
            if self.stopped_strategies.contains(&strategy_id) {
                continue;
            }

            let mut ctx = EngineContext::new(
                &self.quotes,
                &self.instruments,
                &self.order_manager,
                &self.inventory,
                &self.positions,
                &self.exchange_health,
                now_ms,
            );
            self.strategies[i].on_stop(&mut ctx);

            let (places, batches, cancels, cancel_alls, _stop_requests) = ctx.take_commands();
            for cmd in places {
                commands.push(Command::PlaceOrder(cmd));
            }
            for batch in batches {
                commands.push(Command::PlaceOrders(batch));
            }
            for cmd in cancels {
                commands.push(Command::CancelOrder(cmd));
            }
            for cmd in cancel_alls {
                commands.push(Command::CancelAll(cmd));
            }

            self.stopped_strategies.insert(strategy_id);
        }

        commands
    }

    /// Check if a strategy has been stopped
    pub fn is_strategy_stopped(&self, strategy_id: &StrategyId) -> bool {
        self.stopped_strategies.contains(strategy_id)
    }

    /// Get all stopped strategies
    pub fn stopped_strategies(&self) -> &HashSet<StrategyId> {
        &self.stopped_strategies
    }

    /// Get reference to order manager
    pub fn order_manager(&self) -> &OrderManager {
        &self.order_manager
    }

    /// Get mutable reference to order manager
    pub fn order_manager_mut(&mut self) -> &mut OrderManager {
        &mut self.order_manager
    }

    /// Get instrument metadata (needed for command execution).
    pub fn instrument_meta(&self, instrument: &InstrumentId) -> Option<&InstrumentMeta> {
        self.instruments.get(instrument)
    }

    /// Get reference to inventory ledger
    pub fn inventory(&self) -> &InventoryLedger {
        &self.inventory
    }

    /// Get mutable reference to inventory ledger
    pub fn inventory_mut(&mut self) -> &mut InventoryLedger {
        &mut self.inventory
    }

    /// Apply a fill to position tracking (for perpetuals).
    ///
    /// - Buy fill: increases position (adds qty)
    /// - Sell fill: decreases position (subtracts qty)
    /// - Tracks realized PnL when reducing/closing position
    pub fn apply_position_fill(
        &mut self,
        instrument: &InstrumentId,
        side: OrderSide,
        qty: Qty,
        price: Price,
    ) {
        let position = self.positions.entry(instrument.clone()).or_default();

        // Calculate signed delta
        let signed_qty = match side {
            OrderSide::Buy => qty.0,
            OrderSide::Sell => -qty.0,
        };

        let old_qty = position.qty;
        let new_qty = old_qty + signed_qty;

        // Detect position reversal (flip) - trade goes beyond current position
        let is_reversal = (old_qty > Decimal::ZERO && new_qty < Decimal::ZERO)
            || (old_qty < Decimal::ZERO && new_qty > Decimal::ZERO);

        // Track realized PnL when reducing position
        if let Some(entry) = position.avg_entry_px {
            // Check if this fill reduces the position (opposite direction)
            let is_reducing = (old_qty > Decimal::ZERO && signed_qty < Decimal::ZERO)
                || (old_qty < Decimal::ZERO && signed_qty > Decimal::ZERO);

            if is_reducing {
                // For reversal: close the FULL old position
                // For partial/full close: close min of trade_qty and old_qty
                let closed_qty = if is_reversal {
                    old_qty.abs() // Close entire old position
                } else {
                    qty.0.min(old_qty.abs())
                };

                // PnL = (exit_price - entry_price) * closed_qty * direction
                // direction: +1 for long, -1 for short
                let direction = if old_qty > Decimal::ZERO {
                    Decimal::ONE
                } else {
                    -Decimal::ONE
                };
                let pnl = (price.0 - entry.0) * closed_qty * direction;
                position.realized_pnl += pnl;
                tracing::debug!(
                    "Realized PnL on close: {} (closed {} @ {} vs entry {})",
                    pnl,
                    closed_qty,
                    price,
                    entry.0
                );
            }
        }

        // Update average entry price
        if is_reversal {
            // REVERSAL: New position starts fresh at trade price
            position.avg_entry_px = Some(price);
            tracing::debug!(
                "Position reversal: {} -> {}, new entry at {}",
                old_qty,
                new_qty,
                price
            );
        } else if let Some(old_avg) = position.avg_entry_px {
            // If adding to position (same direction), compute weighted avg
            if (old_qty > Decimal::ZERO && signed_qty > Decimal::ZERO)
                || (old_qty < Decimal::ZERO && signed_qty < Decimal::ZERO)
            {
                // Same direction: weighted average
                let total_notional = old_avg.0 * old_qty.abs() + price.0 * qty.0;
                let total_qty = old_qty.abs() + qty.0;
                if total_qty > Decimal::ZERO {
                    position.avg_entry_px = Some(Price(total_notional / total_qty));
                }
            }
            // If reducing (not reversal), keep old avg until flat
        } else if new_qty != Decimal::ZERO {
            // First fill, set entry price
            position.avg_entry_px = Some(price);
        }

        position.qty = new_qty;

        // If position is now flat, clear avg entry but KEEP realized_pnl and total_fees
        if new_qty == Decimal::ZERO {
            position.avg_entry_px = None;
            position.unrealized_pnl = None;
        }

        tracing::debug!(
            "Position update: {} {} -> {} (fill: {} {} @ {})",
            instrument,
            old_qty,
            new_qty,
            side,
            qty,
            price
        );
    }

    /// Apply fee from a fill to position tracking
    pub fn apply_fill_fee(&mut self, instrument: &InstrumentId, fee: Decimal) {
        if fee != Decimal::ZERO {
            let position = self.positions.entry(instrument.clone()).or_default();
            position.total_fees += fee;
            tracing::debug!("Fee applied: {} for {}", fee, instrument);
        }
    }

    /// Get current position for an instrument.
    /// Automatically computes unrealized_pnl from current quotes if available.
    pub fn position(&self, instrument: &InstrumentId) -> Position {
        let mut pos = self.positions.get(instrument).cloned().unwrap_or_default();

        // Compute unrealized PnL if we have position and quote
        if let (Some(avg_entry), Some(quote)) = (pos.avg_entry_px, self.quotes.get(instrument)) {
            let mid = (quote.bid.0 + quote.ask.0) / Decimal::TWO;
            let unrealized = (mid - avg_entry.0) * pos.qty;
            pos.unrealized_pnl = Some(unrealized);
        }

        pos
    }

    /// Get all positions (for testing/WASM/UI snapshots).
    /// Computes unrealized_pnl for each position from current quotes.
    pub fn get_positions(&self) -> HashMap<InstrumentId, Position> {
        self.positions
            .keys()
            .map(|inst| (inst.clone(), self.position(inst)))
            .collect()
    }

    /// Get the sync mechanism from the first strategy (assumes all strategies use the same mechanism)
    pub fn sync_mechanism(&self) -> bot_core::SyncMechanism {
        self.strategies
            .first()
            .map(|s| s.sync_mechanism())
            .unwrap_or_default()
    }

    /// Apply a position snapshot (absolute update)
    ///
    /// Used for snapshot-based synchronization (e.g., clearinghouseState)
    pub fn apply_snapshot(
        &mut self,
        instrument: &InstrumentId,
        qty: Decimal,
        avg_entry_px: Option<Price>,
        unrealized_pnl: Option<Decimal>,
    ) {
        let position = self.positions.entry(instrument.clone()).or_default();
        position.qty = qty;
        position.avg_entry_px = avg_entry_px;
        position.unrealized_pnl = unrealized_pnl;

        tracing::debug!(
            "Position snapshot applied: {} qty={} entry={:?} pnl={:?}",
            instrument,
            qty,
            avg_entry_px,
            unrealized_pnl
        );
    }

    /// Record a fill in the engine's fill history.
    /// This is called when processing OrderFilled events (e.g., from exchange or paper_exchange).
    pub fn record_fill(&mut self, fill: OrderFilledEvent) {
        self.fill_history.push(fill);
    }

    /// Get all recorded fills (for backtesting/reporting).
    pub fn get_fills(&self) -> &[OrderFilledEvent] {
        &self.fill_history
    }

    /// Clear fill history (e.g., on reset).
    pub fn clear_fills(&mut self) {
        self.fill_history.clear();
    }
}
