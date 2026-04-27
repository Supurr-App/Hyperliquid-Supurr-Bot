//! DCA trading strategy implementation.
//!
//! Simplified approach using limit orders:
//! 1. Place ALL DCA limit orders upfront at their calculated prices
//! 2. On each fill: update average entry, cancel old TP, place new TP
//! 3. Monitor for stop loss conditions
//!
//! For SPOT: Always buy DCA, sell TP (unidirectional)
//! For PERPS: Directional - Long (buy DCA, sell TP) or Short (sell DCA, buy TP)

use crate::config::{DCAConfig, DCADirection};
use crate::state::{DCAOrder, DCAOrderState, DCAPhase, DCAState, PendingTakeProfit};

use bot_core::{
    CancelAll, CancelOrder, ClientOrderId, Event, ExchangeHealth, InstrumentMeta, PlaceOrder,
    Price, Qty, Strategy, StrategyContext, StrategyId, TimerId,
};
use rust_decimal::Decimal;

const TWO: Decimal = Decimal::TWO;
const ONE: Decimal = Decimal::ONE;
const HUNDRED: Decimal = Decimal::ONE_HUNDRED;

/// DCA Trading Strategy
///
/// Places all DCA limit orders upfront and manages a single TP order
/// that gets updated on each fill.
pub struct DCAStrategy {
    config: DCAConfig,
    state: DCAState,
    instrument_meta: Option<InstrumentMeta>,
}

impl DCAStrategy {
    /// Create a new DCA strategy with the given configuration.
    pub fn new(config: DCAConfig) -> Self {
        Self {
            state: DCAState::new(config.direction, config.is_spot()),
            config,
            instrument_meta: None,
        }
    }

    // =========================================================================
    // Price & Quantity Rounding
    // =========================================================================

    /// Round price to tick size and trim to 5 significant digits.
    fn round_price(&self, price: Price) -> Price {
        let trimmed = price.trim_to_sig_figs(5);
        if let Some(ref meta) = self.instrument_meta {
            meta.round_price(trimmed)
        } else {
            trimmed
        }
    }

    /// Round quantity to lot size.
    fn round_qty(&self, qty: Qty) -> Qty {
        if let Some(ref meta) = self.instrument_meta {
            meta.round_qty(qty)
        } else {
            qty
        }
    }

    /// Truncate quantity DOWN to lot size (floor).
    /// Used for sell/close orders to avoid overselling.
    fn trunc_qty(&self, qty: Qty) -> Qty {
        if let Some(ref meta) = self.instrument_meta {
            meta.trunc_qty(qty)
        } else {
            qty
        }
    }

    // =========================================================================
    // Lifecycle Handlers
    // =========================================================================

    fn handle_start(&mut self, ctx: &mut dyn StrategyContext) {
        // Load instrument metadata
        self.instrument_meta = ctx.instrument_meta(&self.config.instrument_id()).cloned();

        if self.instrument_meta.is_none() {
            ctx.log_error(&format!(
                "Instrument not found: {}",
                self.config.instrument_id()
            ));
            return;
        }

        // Validate config
        let errors = self.config.validate();
        if !errors.is_empty() {
            for err in &errors {
                ctx.log_error(&format!("Config error: {}", err));
            }
            let reason = format!("DCA configuration validation failed: {}", errors.join("; "));
            self.state.exit_reason = Some(reason.clone());
            ctx.stop_strategy(self.config.strategy_id.clone(), &reason);
            return;
        }

        let mode = if self.config.is_spot() {
            "SPOT (buy→sell)"
        } else {
            match self.config.direction {
                DCADirection::Long => "PERP LONG (buy→sell)",
                DCADirection::Short => "PERP SHORT (sell→buy)",
            }
        };

        ctx.log_info(&format!(
            "DCAStrategy started: {} mode={} base_size={} dca_size={} max_orders={}",
            self.config.instrument_id(),
            mode,
            self.config.base_order_size,
            self.config.dca_order_size,
            self.config.max_dca_orders
        ));

        // Build the DCA ladder
        self.build_dca_ladder(ctx);
        self.state.is_initialized = true;

        // Place all DCA orders immediately
        self.place_all_dca_orders(ctx);
    }

    fn handle_stop(&mut self, ctx: &mut dyn StrategyContext) {
        ctx.log_info("DCAStrategy stopping - canceling all orders");

        // Cancel all orders for this instrument
        ctx.cancel_all(CancelAll::for_instrument(
            self.config.exchange_instance(),
            self.config.instrument_id(),
        ));

        self.state.clear_take_profit_tracking();
        self.state.phase = DCAPhase::Completed;
    }

    // =========================================================================
    // Event Handlers
    // =========================================================================

    fn handle_quote(&mut self, ctx: &mut dyn StrategyContext, bid: Price, ask: Price) {
        let mid = Price((bid.0 + ask.0) / TWO);
        self.state.mid_price = Some(mid);

        // Check if cooldown has expired and start new cycle
        if self.state.phase == DCAPhase::Cooldown {
            let now_ms = ctx.now_ms();
            if let Some(cooldown_until) = self.state.cooldown_until {
                if now_ms >= cooldown_until {
                    ctx.log_info("Cooldown complete - starting new DCA cycle");
                    self.state.cooldown_until = None;
                    self.state.reset_for_new_cycle();
                    self.build_dca_ladder(ctx);
                    self.place_all_dca_orders(ctx);
                }
            }
            return; // Don't process further during cooldown
        }

        // Check for exit condition
        if self.state.exit_reason.is_some() {
            return;
        }

        // Check stop loss - compare unrealized PnL from engine against threshold
        if self.state.has_position() {
            if let Some(stop_loss) = self.config.stop_loss {
                let position = ctx.position(&self.config.instrument_id());
                if let Some(unrealized_pnl) = position.unrealized_pnl {
                    if unrealized_pnl < stop_loss {
                        ctx.log_warn(&format!(
                            "Stop loss reached! unrealized_pnl={} < stop_loss={}",
                            unrealized_pnl, stop_loss
                        ));
                        self.trigger_stop_loss(ctx);
                    }
                }
            }
        }

        // Periodic logging
        let now = ctx.now_ms();
        if now - self.state.last_log_ts > 5000 {
            self.log_status(ctx);
            self.state.last_log_ts = now;
        }
    }

    fn handle_order_accepted(&mut self, ctx: &mut dyn StrategyContext, client_id: &ClientOrderId) {
        ctx.log_info(&format!("Order accepted: {}", client_id));
    }

    fn handle_order_rejected(
        &mut self,
        ctx: &mut dyn StrategyContext,
        client_id: &ClientOrderId,
        reason: &str,
    ) {
        ctx.log_warn(&format!("Order rejected: {} reason={}", client_id, reason));

        // Check if it's a TP order
        if self.state.tp_order_id.as_ref() == Some(client_id) {
            if self.state.tp_cancel_in_flight.as_ref() == Some(client_id) {
                self.state.tp_cancel_in_flight = None;
            }
            self.state.tp_order_id = None;
            ctx.log_info("TP order rejected - will retry on next fill");
            return;
        }

        // Check if it's a DCA order
        if let Some(order_idx) = self.state.unregister_order(client_id) {
            if let Some(order) = self.state.order_mut(order_idx) {
                order.reset();
                ctx.log_warn(&format!(
                    "DCA order {} rejected - will retry placement",
                    order_idx
                ));
            }
        }
    }

    fn handle_order_canceled(&mut self, ctx: &mut dyn StrategyContext, client_id: &ClientOrderId) {
        ctx.log_info(&format!("Order canceled: {}", client_id));

        // TP cancellation is expected when we update it.
        // Place the deferred replacement only after the exchange confirms cancel.
        if self.state.tp_cancel_in_flight.as_ref() == Some(client_id) {
            self.state.tp_cancel_in_flight = None;
            self.state.tp_order_id = None;

            if let Some(replacement) = self.state.pending_tp_replacement.clone() {
                self.place_take_profit_order(ctx, replacement);
            }
            return;
        }

        if self.state.tp_order_id.as_ref() == Some(client_id) {
            self.state.tp_order_id = None;
            return;
        }

        // DCA order cancellation is unexpected
        if let Some(order_idx) = self.state.unregister_order(client_id) {
            ctx.log_warn(&format!(
                "DCA order {} was canceled unexpectedly",
                order_idx
            ));
        }
    }

    fn handle_order_filled(
        &mut self,
        ctx: &mut dyn StrategyContext,
        client_id: &ClientOrderId,
        qty: Qty,
        price: Price,
        is_complete: bool,
    ) {
        ctx.log_info(&format!(
            "Order filled: {} qty={} price={} complete={}",
            client_id, qty, price, is_complete
        ));

        // Check if it's the TP order
        if self.state.tp_order_id.as_ref() == Some(client_id) {
            if is_complete {
                ctx.log_info(&format!("Take profit filled @ {} - cycle complete!", price));
                self.handle_cycle_complete(ctx, "Take profit reached");
            }
            return;
        }

        // Note: SL is not an order - it's live PnL tracking that triggers stop_strategy()

        // Check if it's a DCA order
        let Some(order_idx) = self.state.order_index(client_id) else {
            ctx.log_info(&format!(
                "Fill for unmanaged order {} - ignoring",
                client_id
            ));
            return;
        };

        // Update the order state
        if let Some(order) = self.state.order_mut(order_idx) {
            order.filled_qty += qty;
            if is_complete {
                order.set_filled(price, order.filled_qty);
                self.state.unregister_order(client_id);
            }
        }

        // Update average entry price
        self.state.update_average_entry(price, qty);

        // Update TP price (SL is now PnL-based, checked on quotes)
        self.state.update_tp_price(self.config.take_profit_pct);

        ctx.log_info(&format!(
            "Position update: qty={} avg={:?} tp={:?}",
            self.state.total_filled_qty,
            self.state.average_entry_price,
            self.state.take_profit_price,
        ));

        // Update TP order (cancel old, place new)
        if is_complete {
            self.update_take_profit_order(ctx);
        }
    }

    // =========================================================================
    // DCA Ladder Construction
    // =========================================================================

    /// Build the pre-computed DCA ladder with limit order prices and sizes.
    fn build_dca_ladder(&mut self, ctx: &mut dyn StrategyContext) {
        let total_orders = 1 + self.config.max_dca_orders as usize;
        let mut orders = Vec::with_capacity(total_orders);

        // Get current price for base order
        let base_price = self
            .state
            .mid_price
            .unwrap_or(Price::new(self.config.trigger_price));

        // Base order (index 0) - at current price or trigger price
        let base_limit = if self.config.trigger_price > Decimal::ZERO {
            self.round_price(Price::new(self.config.trigger_price))
        } else {
            self.round_price(base_price)
        };
        let base_size = self.round_qty(Qty::new(self.config.base_order_size));
        orders.push(DCAOrder::new(0, base_limit, base_size));

        ctx.log_info(&format!(
            "DCA ladder[0]: limit={} size={} (base order)",
            base_limit, base_size
        ));

        // DCA orders (index 1 to max_dca_orders) - at progressively lower/higher prices
        let mut current_price = base_limit.0;
        let mut current_size = self.config.dca_order_size;
        let mut deviation_factor = ONE;

        for i in 1..total_orders {
            // Calculate deviation for this level
            let deviation_pct = self.config.price_deviation_pct * deviation_factor;

            // Calculate limit price
            // For LONG/SPOT: DCA orders are BELOW base (buy lower)
            // For SHORT: DCA orders are ABOVE base (sell higher)
            let next_price = if self.config.is_spot() {
                current_price * (ONE - deviation_pct / HUNDRED)
            } else {
                match self.config.direction {
                    DCADirection::Long => current_price * (ONE - deviation_pct / HUNDRED),
                    DCADirection::Short => current_price * (ONE + deviation_pct / HUNDRED),
                }
            };

            let limit_price = self.round_price(Price::new(next_price));
            let order_size = self.round_qty(Qty::new(current_size));

            orders.push(DCAOrder::new(i, limit_price, order_size));

            ctx.log_info(&format!(
                "DCA ladder[{}]: limit={} size={} (deviation={}%)",
                i, limit_price, order_size, deviation_pct
            ));

            // Update for next iteration
            current_price = next_price;
            current_size *= self.config.size_multiplier;
            deviation_factor *= self.config.deviation_multiplier;
        }

        self.state.orders = orders;
    }

    // =========================================================================
    // Order Placement
    // =========================================================================

    /// Place all DCA limit orders upfront.
    fn place_all_dca_orders(&mut self, ctx: &mut dyn StrategyContext) {
        // Check exchange health
        if ctx.exchange_health(&self.config.exchange_instance()) == ExchangeHealth::Halted {
            ctx.log_warn("Exchange halted, deferring order placement");
            return;
        }

        let side = self.state.open_side();

        // First, collect the orders that need placement with their generated client IDs
        let orders_to_place: Vec<(usize, ClientOrderId, Price, Qty)> = self
            .state
            .orders
            .iter()
            .filter(|o| o.needs_placement())
            .map(|o| {
                let client_id = ClientOrderId::generate();
                (o.index, client_id, o.limit_price, o.order_size)
            })
            .collect();

        if orders_to_place.is_empty() {
            ctx.log_info("All DCA orders already placed");
            self.state.phase = DCAPhase::Active;
            return;
        }

        // Build batch of orders
        let mut batch: Vec<PlaceOrder> = Vec::with_capacity(orders_to_place.len());

        for (order_idx, client_id, price, qty) in &orders_to_place {
            let place_order = PlaceOrder::limit(
                self.config.exchange_instance(),
                self.config.instrument_id(),
                side,
                *price,
                *qty,
            )
            .with_client_id(client_id.clone());

            batch.push(place_order);

            // Register and update order state
            self.state.register_order(client_id, *order_idx);
            if let Some(order) = self.state.order_mut(*order_idx) {
                order.set_placed(client_id.clone());
            }
        }

        ctx.log_info(&format!(
            "Placing {} DCA limit orders as batch",
            batch.len()
        ));
        ctx.place_orders(batch);
        self.state.phase = DCAPhase::Active;
    }

    /// Update the take profit order (cancel existing, place new).
    fn update_take_profit_order(&mut self, ctx: &mut dyn StrategyContext) {
        // Calculate new TP price
        let Some(tp_price) = self.state.take_profit_price else {
            ctx.log_warn("Cannot place TP: no average entry price yet");
            return;
        };

        let tp_price = self.round_price(tp_price);
        // Truncate DOWN to lot size to avoid overselling (fee-deducted net qty
        // can have fractional remainder that rounds up past actual holdings)
        let qty = self.trunc_qty(self.state.total_filled_qty);

        if qty.0 <= Decimal::ZERO {
            ctx.log_warn("TP qty truncated to zero - skipping");
            self.state.pending_tp_replacement = None;
            return;
        }

        let replacement = PendingTakeProfit {
            price: tp_price,
            qty,
        };

        if let Some(canceling_tp_id) = self.state.tp_cancel_in_flight.clone() {
            ctx.log_info(&format!(
                "TP cancel still in flight for {} - deferring replacement",
                canceling_tp_id
            ));
            self.state.pending_tp_replacement = Some(replacement);
            return;
        }

        // Cancel existing TP order if present, and wait for cancel ack before replacing it.
        if let Some(old_tp_id) = self.state.tp_order_id.clone() {
            ctx.log_info(&format!("Canceling old TP order: {}", old_tp_id));
            self.state.tp_cancel_in_flight = Some(old_tp_id.clone());
            self.state.pending_tp_replacement = Some(replacement);
            ctx.cancel_order(CancelOrder::new(self.config.exchange_instance(), old_tp_id));
            return;
        }

        self.place_take_profit_order(ctx, replacement);
    }

    fn place_take_profit_order(
        &mut self,
        ctx: &mut dyn StrategyContext,
        replacement: PendingTakeProfit,
    ) {
        let place_order = PlaceOrder::limit(
            self.config.exchange_instance(),
            self.config.instrument_id(),
            self.state.close_side(),
            replacement.price,
            replacement.qty,
        )
        .with_client_id(ClientOrderId::generate());

        let client_id = place_order.client_id.clone();
        let side = place_order.side;

        ctx.log_info(&format!(
            "Placing TP order: side={} price={} qty={} cloid={}",
            side, replacement.price, replacement.qty, client_id
        ));

        self.state.tp_cancel_in_flight = None;
        self.state.pending_tp_replacement = None;
        self.state.tp_order_id = Some(client_id);
        ctx.place_order(place_order);
    }

    /// Trigger stop loss - cancel all orders and STOP strategy (no restart, like liquidation).
    /// Stop-loss is live PnL tracking, not an order. When threshold is hit, we just stop.
    fn trigger_stop_loss(&mut self, ctx: &mut dyn StrategyContext) {
        ctx.log_warn(&format!(
            "STOP LOSS TRIGGERED! avg_entry={:?} stop_loss_threshold={:?}",
            self.state.average_entry_price, self.config.stop_loss
        ));

        // Cancel all open orders
        ctx.cancel_all(CancelAll::for_instrument(
            self.config.exchange_instance(),
            self.config.instrument_id(),
        ));

        self.state.clear_take_profit_tracking();
        // Mark strategy as completed with stop loss reason
        self.state.phase = DCAPhase::Completed;
        self.state.exit_reason = Some("Stop loss triggered".to_string());

        // STOP the strategy - no restart, no cooldown (like liquidation)
        ctx.stop_strategy(
            self.config.strategy_id.clone(),
            "Stop loss triggered - closing bot",
        );
    }

    // =========================================================================
    // Cycle Management
    // =========================================================================

    /// Handle cycle completion (TP hit).
    fn handle_cycle_complete(&mut self, ctx: &mut dyn StrategyContext, reason: &str) {
        ctx.log_info(&format!("DCA cycle complete: {}", reason));

        // Cancel any remaining DCA orders
        ctx.cancel_all(CancelAll::for_instrument(
            self.config.exchange_instance(),
            self.config.instrument_id(),
        ));

        // Clear TP order tracking
        self.state.clear_take_profit_tracking();

        if self.config.restart_on_complete {
            if self.config.cooldown_period_secs > 0 {
                // Enter cooldown phase
                let now_ms = ctx.now_ms();
                let cooldown_end_ms = now_ms + (self.config.cooldown_period_secs as i64 * 1000);
                self.state.cooldown_until = Some(cooldown_end_ms);
                self.state.phase = DCAPhase::Cooldown;
                ctx.log_info(&format!(
                    "Entering cooldown for {}s (until {})",
                    self.config.cooldown_period_secs, cooldown_end_ms
                ));
            } else {
                // No cooldown, restart immediately
                ctx.log_info("Restarting DCA cycle...");
                self.state.reset_for_new_cycle();
                self.build_dca_ladder(ctx);
                self.place_all_dca_orders(ctx);
            }
        } else {
            self.state.phase = DCAPhase::Completed;
            self.state.exit_reason = Some(reason.to_string());
            ctx.stop_strategy(self.config.strategy_id.clone(), reason);
        }
    }

    // =========================================================================
    // Logging
    // =========================================================================

    fn log_status(&self, ctx: &dyn StrategyContext) {
        let filled = self.state.filled_orders_count();
        let total = self.state.orders.len();

        ctx.log_info(&format!(
            "DCA status: phase={:?} mid={:?} filled={}/{} qty={} avg={:?} tp={:?}",
            self.state.phase,
            self.state.mid_price,
            filled,
            total,
            self.state.total_filled_qty,
            self.state.average_entry_price,
            self.state.take_profit_price,
        ));
    }
}

// =============================================================================
// Strategy trait implementation
// =============================================================================

impl Strategy for DCAStrategy {
    fn id(&self) -> &StrategyId {
        &self.config.strategy_id
    }

    fn on_start(&mut self, ctx: &mut dyn StrategyContext) {
        self.handle_start(ctx);
    }

    fn on_event(&mut self, ctx: &mut dyn StrategyContext, event: &Event) {
        // Strategy is stopped - ignore all events
        if self.state.exit_reason.is_some() {
            return;
        }

        match event {
            Event::Quote(e) => {
                if e.instrument == self.config.instrument_id() {
                    self.handle_quote(ctx, e.bid, e.ask);
                }
            }
            Event::OrderAccepted(e) => {
                self.handle_order_accepted(ctx, &e.client_id);
            }
            Event::OrderRejected(e) => {
                self.handle_order_rejected(ctx, &e.client_id, &e.reason);
            }
            Event::OrderCanceled(e) => {
                self.handle_order_canceled(ctx, &e.client_id);
            }
            Event::OrderFilled(e) => {
                let is_complete = ctx
                    .order(&e.client_id)
                    .map(|o| o.is_complete())
                    .unwrap_or(false);
                // Use net_qty to account for fees deducted from spot BUY fills
                self.handle_order_filled(ctx, &e.client_id, e.net_qty, e.price, is_complete);
            }
            Event::OrderCompleted(e) => {
                // Handle TP completion
                if self.state.tp_order_id.as_ref() == Some(&e.client_id) {
                    self.handle_order_filled(
                        ctx,
                        &e.client_id,
                        self.state.total_filled_qty,
                        self.state
                            .take_profit_price
                            .unwrap_or(Price::new(Decimal::ZERO)),
                        true,
                    );
                } else if let Some(order_idx) = self.state.order_index(&e.client_id) {
                    if let Some(order) = self.state.order(order_idx) {
                        if order.state != DCAOrderState::Filled {
                            self.handle_order_filled(
                                ctx,
                                &e.client_id,
                                order.order_size,
                                order.limit_price,
                                true,
                            );
                        }
                    }
                }
            }
            Event::ExchangeStateChanged(e) => {
                ctx.log_info(&format!(
                    "Exchange state changed: {:?} -> {:?} ({})",
                    e.old_state, e.new_state, e.reason
                ));
                // Retry placing orders if exchange comes back
                if self.state.pending_orders_count() > 0 {
                    self.place_all_dca_orders(ctx);
                }
            }
            Event::FundingRate(_) => {}
        }
    }

    fn on_timer(&mut self, ctx: &mut dyn StrategyContext, _timer_id: TimerId) {
        if self.state.exit_reason.is_some() {
            return;
        }

        // Retry placing any pending orders
        if self.state.pending_orders_count() > 0 {
            self.place_all_dca_orders(ctx);
        }
    }

    fn on_stop(&mut self, ctx: &mut dyn StrategyContext) {
        self.handle_stop(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bot_core::{
        AssetId, Balance, Environment, ExchangeHealth, ExchangeId, ExchangeInstance,
        HyperliquidMarket, InstrumentId, InstrumentKind, InstrumentMeta, LiveOrder, Market,
        MarketIndex, OrderCanceledEvent, OrderSide, Position, Price, Qty, Quote, Strategy,
        StrategyContext,
    };
    use rust_decimal_macros::dec;
    use std::collections::HashMap;
    use std::time::Duration;

    #[derive(Default)]
    struct TestContext {
        place_orders: Vec<PlaceOrder>,
        batch_orders: Vec<Vec<PlaceOrder>>,
        cancel_orders: Vec<CancelOrder>,
        cancel_alls: Vec<CancelAll>,
        stop_requests: Vec<(StrategyId, String)>,
        instruments: HashMap<InstrumentId, InstrumentMeta>,
        now_ms: i64,
    }

    impl TestContext {
        fn with_instrument(meta: InstrumentMeta) -> Self {
            let mut instruments = HashMap::new();
            instruments.insert(meta.instrument_id.clone(), meta);
            Self {
                instruments,
                ..Self::default()
            }
        }

        fn clear_commands(&mut self) {
            self.place_orders.clear();
            self.batch_orders.clear();
            self.cancel_orders.clear();
            self.cancel_alls.clear();
            self.stop_requests.clear();
        }
    }

    impl StrategyContext for TestContext {
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
            self.stop_requests.push((strategy_id, reason.to_string()));
        }

        fn set_timer(&mut self, _delay: Duration) -> bot_core::TimerId {
            bot_core::TimerId::new(1)
        }

        fn set_interval(&mut self, _interval: Duration) -> bot_core::TimerId {
            bot_core::TimerId::new(1)
        }

        fn cancel_timer(&mut self, _timer_id: bot_core::TimerId) {}

        fn mid_price(&self, _instrument: &InstrumentId) -> Option<Price> {
            None
        }

        fn quote(&self, _instrument: &InstrumentId) -> Option<Quote> {
            None
        }

        fn instrument_meta(&self, instrument: &InstrumentId) -> Option<&InstrumentMeta> {
            self.instruments.get(instrument)
        }

        fn balance(&self, _asset: &AssetId) -> Balance {
            Balance::zero()
        }

        fn position(&self, _instrument: &InstrumentId) -> Position {
            Position::default()
        }

        fn exchange_health(&self, _exchange: &ExchangeInstance) -> ExchangeHealth {
            ExchangeHealth::Active
        }

        fn order(&self, _client_id: &bot_core::ClientOrderId) -> Option<&LiveOrder> {
            None
        }

        fn now_ms(&self) -> i64 {
            self.now_ms
        }

        fn log_info(&self, _msg: &str) {}

        fn log_warn(&self, _msg: &str) {}

        fn log_error(&self, _msg: &str) {}

        fn log_debug(&self, _msg: &str) {}
    }

    fn spot_test_config() -> DCAConfig {
        DCAConfig {
            strategy_id: StrategyId::new("test-spot-dca"),
            environment: Environment::Testnet,
            market: Market::Hyperliquid(HyperliquidMarket::Spot {
                base: "HYPE".to_string(),
                quote: "USDC".to_string(),
                index: 10107,
                instrument_meta: None,
            }),
            direction: DCADirection::Long,
            trigger_price: dec!(44.89),
            base_order_size: dec!(1.04),
            dca_order_size: dec!(0.61),
            max_dca_orders: 7,
            size_multiplier: dec!(1.25),
            price_deviation_pct: dec!(0.03),
            deviation_multiplier: dec!(1.0),
            take_profit_pct: dec!(0.045),
            stop_loss: None,
            leverage: dec!(1),
            max_leverage: dec!(1),
            restart_on_complete: false,
            cooldown_period_secs: 60,
        }
    }

    fn spot_test_meta() -> InstrumentMeta {
        InstrumentMeta {
            instrument_id: InstrumentId::new("HYPE-SPOT"),
            market_index: MarketIndex::new(10107),
            base_asset: AssetId::new("HYPE"),
            quote_asset: AssetId::new("USDC"),
            tick_size: dec!(0.000001),
            lot_size: dec!(0.01),
            min_qty: Some(dec!(0.01)),
            min_notional: Some(dec!(10)),
            fee_asset_default: Some(AssetId::new("HYPE")),
            kind: InstrumentKind::Spot,
        }
    }

    #[test]
    fn test_strategy_creation() {
        let config = DCAConfig::default();
        let strategy = DCAStrategy::new(config);

        assert_eq!(strategy.state.phase, DCAPhase::PlacingOrders);
        assert!(strategy.state.orders.is_empty()); // Not initialized until on_start
    }

    #[test]
    fn test_spot_mode_uses_correct_sides() {
        let mut config = DCAConfig::default();
        config.market = bot_core::Market::Hyperliquid(HyperliquidMarket::Spot {
            base: "HYPE".to_string(),
            quote: "USDC".to_string(),
            index: 1,
            instrument_meta: None,
        });
        let strategy = DCAStrategy::new(config);

        // Spot should always be buy DCA, sell TP
        assert_eq!(strategy.state.open_side(), bot_core::OrderSide::Buy);
        assert_eq!(strategy.state.close_side(), bot_core::OrderSide::Sell);
    }

    #[test]
    fn test_perp_short_uses_correct_sides() {
        let mut config = DCAConfig::default();
        config.direction = DCADirection::Short;
        // Default market is already Perp from DCAConfig::default()
        let strategy = DCAStrategy::new(config);

        // Short perp: sell DCA, buy TP
        assert_eq!(strategy.state.open_side(), bot_core::OrderSide::Sell);
        assert_eq!(strategy.state.close_side(), bot_core::OrderSide::Buy);
    }

    #[test]
    fn test_tp_replacement_waits_for_cancel_ack() {
        let config = spot_test_config();
        let meta = spot_test_meta();
        let mut strategy = DCAStrategy::new(config.clone());
        strategy.instrument_meta = Some(meta.clone());
        strategy.state.take_profit_price = Some(Price::new(dec!(44.872)));
        strategy.state.total_filled_qty = Qty::new(dec!(6.03727202));
        strategy.state.tp_order_id = Some(bot_core::ClientOrderId::new("old-tp"));

        let old_tp_id = strategy.state.tp_order_id.clone().unwrap();
        let mut ctx = TestContext::with_instrument(meta);

        strategy.update_take_profit_order(&mut ctx);

        assert_eq!(ctx.cancel_orders.len(), 1);
        assert_eq!(ctx.place_orders.len(), 0);
        assert_eq!(
            strategy.state.tp_cancel_in_flight.as_ref(),
            Some(&old_tp_id)
        );
        assert!(strategy.state.pending_tp_replacement.is_some());
        assert_eq!(strategy.state.tp_order_id.as_ref(), Some(&old_tp_id));

        ctx.clear_commands();
        strategy.on_event(
            &mut ctx,
            &Event::OrderCanceled(OrderCanceledEvent {
                exchange: ExchangeId::new("hyperliquid"),
                instrument: config.instrument_id(),
                client_id: old_tp_id.clone(),
                reason: None,
                ts: 0,
            }),
        );

        assert_eq!(ctx.cancel_orders.len(), 0);
        assert_eq!(ctx.place_orders.len(), 1);
        assert!(strategy.state.tp_cancel_in_flight.is_none());
        assert!(strategy.state.pending_tp_replacement.is_none());
        assert!(strategy.state.tp_order_id.is_some());
        assert_ne!(strategy.state.tp_order_id.as_ref(), Some(&old_tp_id));

        let replacement = &ctx.place_orders[0];
        assert_eq!(replacement.side, OrderSide::Sell);
        assert_eq!(replacement.qty, Qty::new(dec!(6.03)));
        assert_eq!(replacement.price, Price::new(dec!(44.872)));
    }

    #[test]
    fn test_tp_replacement_updates_latest_spec_while_cancel_in_flight() {
        let config = spot_test_config();
        let meta = spot_test_meta();
        let mut strategy = DCAStrategy::new(config.clone());
        strategy.instrument_meta = Some(meta.clone());
        strategy.state.take_profit_price = Some(Price::new(dec!(44.881)));
        strategy.state.total_filled_qty = Qty::new(dec!(4.54788303));
        strategy.state.tp_order_id = Some(bot_core::ClientOrderId::new("old-tp"));

        let old_tp_id = strategy.state.tp_order_id.clone().unwrap();
        let mut ctx = TestContext::with_instrument(meta);

        strategy.update_take_profit_order(&mut ctx);
        assert_eq!(ctx.cancel_orders.len(), 1);
        assert!(strategy.state.pending_tp_replacement.is_some());

        ctx.clear_commands();
        strategy.state.take_profit_price = Some(Price::new(dec!(44.872)));
        strategy.state.total_filled_qty = Qty::new(dec!(6.03727202));
        strategy.update_take_profit_order(&mut ctx);

        assert_eq!(ctx.cancel_orders.len(), 0);
        assert_eq!(ctx.place_orders.len(), 0);
        let pending = strategy.state.pending_tp_replacement.as_ref().unwrap();
        assert_eq!(pending.price, Price::new(dec!(44.872)));
        assert_eq!(pending.qty, Qty::new(dec!(6.03)));

        strategy.on_event(
            &mut ctx,
            &Event::OrderCanceled(OrderCanceledEvent {
                exchange: ExchangeId::new("hyperliquid"),
                instrument: config.instrument_id(),
                client_id: old_tp_id,
                reason: None,
                ts: 0,
            }),
        );

        assert_eq!(ctx.place_orders.len(), 1);
        let replacement = &ctx.place_orders[0];
        assert_eq!(replacement.price, Price::new(dec!(44.872)));
        assert_eq!(replacement.qty, Qty::new(dec!(6.03)));
    }

    #[test]
    fn test_tp_fill_clears_deferred_replacement_state() {
        let config = spot_test_config();
        let meta = spot_test_meta();
        let mut strategy = DCAStrategy::new(config.clone());
        strategy.instrument_meta = Some(meta.clone());
        strategy.state.take_profit_price = Some(Price::new(dec!(44.872)));
        strategy.state.total_filled_qty = Qty::new(dec!(6.03727202));
        strategy.state.tp_order_id = Some(bot_core::ClientOrderId::new("old-tp"));

        let old_tp_id = strategy.state.tp_order_id.clone().unwrap();
        let mut ctx = TestContext::with_instrument(meta);

        strategy.update_take_profit_order(&mut ctx);
        assert!(strategy.state.tp_cancel_in_flight.is_some());
        assert!(strategy.state.pending_tp_replacement.is_some());

        ctx.clear_commands();
        strategy.handle_order_filled(
            &mut ctx,
            &old_tp_id,
            Qty::new(dec!(6.03)),
            Price::new(dec!(44.872)),
            true,
        );

        assert!(strategy.state.tp_order_id.is_none());
        assert!(strategy.state.tp_cancel_in_flight.is_none());
        assert!(strategy.state.pending_tp_replacement.is_none());
        assert_eq!(ctx.cancel_alls.len(), 1);
        assert_eq!(ctx.stop_requests.len(), 1);

        ctx.clear_commands();
        strategy.on_event(
            &mut ctx,
            &Event::OrderCanceled(OrderCanceledEvent {
                exchange: ExchangeId::new("hyperliquid"),
                instrument: config.instrument_id(),
                client_id: old_tp_id,
                reason: None,
                ts: 0,
            }),
        );

        assert_eq!(ctx.place_orders.len(), 0);
    }
}
