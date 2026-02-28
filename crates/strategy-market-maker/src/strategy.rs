//! Market maker strategy implementation.

use crate::config::{MarketMakerConfig, SkewMode};
use crate::state::{InventoryMetrics, MarketMakerState, OrderState, SkewAdjustments};

use bot_core::{
    CancelAll, CancelOrder, ClientOrderId, Event, ExchangeHealth, InstrumentMeta, OrderSide,
    PlaceOrder, Price, Qty, Strategy, StrategyContext, StrategyId, TimerId,
};
use rust_decimal::Decimal;

const TWO: Decimal = Decimal::TWO;

/// Skew-based Market Maker Strategy
///
/// Uses a state machine pattern to prevent duplicate orders:
/// - Only place when state is IDLE
/// - Transition to PLACED on command emit
/// - Transition to ACTIVE on OrderAccepted
/// - Back to IDLE on terminal events (filled/canceled/rejected)
pub struct MarketMaker {
    config: MarketMakerConfig,
    state: MarketMakerState,
    instrument_meta: Option<InstrumentMeta>,
}

impl MarketMaker {
    /// Create a new market maker with the given configuration.
    pub fn new(config: MarketMakerConfig) -> Self {
        Self {
            config,
            state: MarketMakerState::new(),
            instrument_meta: None,
        }
    }

    // -------------------------------------------------------------------------
    // Price Rounding (critical for Hyperliquid)
    // -------------------------------------------------------------------------

    fn round_price(&self, price: Price) -> Price {
        // First trim to 5 significant digits (Hyperliquid requirement)
        let trimmed = price.trim_to_sig_figs(5);

        // Then round to tick size
        if let Some(ref meta) = self.instrument_meta {
            meta.round_price(trimmed)
        } else {
            trimmed
        }
    }

    fn round_qty(&self, qty: Qty) -> Qty {
        if let Some(ref meta) = self.instrument_meta {
            meta.round_qty(qty)
        } else {
            qty
        }
    }

    /// Truncate quantity DOWN to lot size (floor).
    /// Used for sell orders to avoid overselling.
    fn trunc_qty(&self, qty: Qty) -> Qty {
        if let Some(ref meta) = self.instrument_meta {
            meta.trunc_qty(qty)
        } else {
            qty
        }
    }

    fn tick_size(&self) -> Decimal {
        self.instrument_meta
            .as_ref()
            .map(|m| m.tick_size)
            .unwrap_or(Decimal::new(1, 2)) // 0.01 default
    }

    // -------------------------------------------------------------------------
    // Lifecycle Handlers
    // -------------------------------------------------------------------------

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
            let reason = format!("MM configuration validation failed: {}", errors.join("; "));
            self.state.exit_reason = Some(reason.clone());
            ctx.stop_strategy(self.config.strategy_id.clone(), &reason);
            return;
        }

        ctx.log_info(&format!(
            "MarketMaker started: {} spread={} size={} skew_mode={:?}",
            self.config.instrument_id(),
            self.config.base_spread,
            self.config.base_order_size,
            self.config.skew_mode
        ));
    }

    fn handle_stop(&mut self, ctx: &mut dyn StrategyContext) {
        ctx.log_info("MarketMaker stopping - canceling all orders");

        // Cancel all orders for this instrument in a single batch call
        ctx.cancel_all(CancelAll::for_instrument(
            self.config.exchange_instance(),
            self.config.instrument_id(),
        ));

        self.state.reset_all_orders();
    }

    // -------------------------------------------------------------------------
    // Event Handlers
    // -------------------------------------------------------------------------

    fn handle_quote(&mut self, ctx: &mut dyn StrategyContext, bid: Price, ask: Price) {
        let mid = Price((bid.0 + ask.0) / TWO);
        self.state.mid_price = Some(mid);

        // Check for exit condition
        if self.state.exit_reason.is_some() {
            return;
        }

        // Check PnL limits
        self.validate_pnl(ctx);

        // Periodic logging
        let now = ctx.now_ms();
        if now - self.state.last_log_ts > 5000 {
            self.log_status(ctx);
            self.state.last_log_ts = now;
        }

        // Sync orders - place new ones if needed
        self.sync_orders(ctx, mid);
    }

    fn handle_order_accepted(&mut self, ctx: &mut dyn StrategyContext, client_id: &ClientOrderId) {
        ctx.log_info(&format!("Order accepted: {}", client_id));

        if let Some(side) = self.state.order_side(client_id) {
            let side_state = self.state.side_mut(side);
            if side_state.state == OrderState::Placed {
                side_state.set_active();
            }
        }
    }

    fn handle_order_rejected(
        &mut self,
        ctx: &mut dyn StrategyContext,
        client_id: &ClientOrderId,
        reason: &str,
    ) {
        ctx.log_warn(&format!("Order rejected: {} reason={}", client_id, reason));
        self.handle_order_terminal(ctx, client_id);
    }

    fn handle_order_canceled(&mut self, ctx: &mut dyn StrategyContext, client_id: &ClientOrderId) {
        ctx.log_info(&format!("Order canceled: {}", client_id));
        self.handle_order_terminal(ctx, client_id);
    }

    fn handle_order_filled(
        &mut self,
        ctx: &mut dyn StrategyContext,
        client_id: &ClientOrderId,
        side: OrderSide,
        qty: Qty,
        price: Price,
        is_complete: bool,
    ) {
        ctx.log_info(&format!(
            "Order filled: {} side={} qty={} price={}",
            client_id, side, qty, price
        ));

        // Check if fully filled
        if is_complete {
            self.handle_order_terminal(ctx, client_id);
            // NOTE: Do NOT call sync_orders here!
            // Let the next quote tick drive new order placement to avoid duplicates.
        }
    }

    fn handle_order_terminal(&mut self, ctx: &mut dyn StrategyContext, client_id: &ClientOrderId) {
        if let Some(side) = self.state.unregister_order(client_id) {
            let side_state = self.state.side_mut(side);
            side_state.reset();
            ctx.log_info(&format!("{} order terminal -> IDLE", side));
        }
    }

    // -------------------------------------------------------------------------
    // Core Logic: Order Sync
    // -------------------------------------------------------------------------

    fn sync_orders(&mut self, ctx: &mut dyn StrategyContext, mid: Price) {
        // Check exchange health
        if ctx.exchange_health(&self.config.exchange_instance()) == ExchangeHealth::Halted {
            ctx.log_debug("Exchange halted, skipping order sync");
            return;
        }

        // Calculate inventory metrics and skew adjustments
        self.state.inventory_metrics = self.calculate_inventory_metrics(ctx);
        self.state.current_position = self.state.inventory_metrics.current_qty;
        self.state.skew_adjustments = self.calculate_skew_adjustments();

        let price_skew = self.state.skew_adjustments.price_skew;
        let base_spread = self.config.base_spread;
        let base_size = self.config.base_order_size;

        // Calculate raw target prices
        let mut raw_buy_price = mid.0 * (Decimal::ONE - base_spread - price_skew);
        let mut raw_sell_price = mid.0 * (Decimal::ONE + base_spread - price_skew);

        let tick_size = self.tick_size();
        let min_price = tick_size;

        // CRITICAL SAFETY: Ensure prices are positive and properly ordered
        if raw_buy_price <= Decimal::ZERO {
            ctx.log_warn(&format!(
                "Price skew too aggressive! raw_buy_price={} <= 0. Clamping to min_price={}",
                raw_buy_price, min_price
            ));
            raw_buy_price = min_price;
        }

        if raw_sell_price <= raw_buy_price {
            ctx.log_warn(&format!(
                "Spread inverted! sell={} <= buy={}. Forcing minimum spread.",
                raw_sell_price, raw_buy_price
            ));
            raw_sell_price = raw_buy_price + tick_size;
        }

        let target_buy_price = self.round_price(Price(raw_buy_price));
        let mut target_sell_price = self.round_price(Price(raw_sell_price));

        // Final safety: ensure rounding didn't invert the spread
        if target_sell_price.0 <= target_buy_price.0 {
            target_sell_price = Price(target_buy_price.0 + tick_size);
        }

        let mut target_buy_size = base_size * self.state.skew_adjustments.buy_size_mult;
        let mut target_sell_size = base_size * self.state.skew_adjustments.sell_size_mult;

        // SAFETY: Ensure sizes are positive (minimum = floor * base_size)
        let min_size = self.config.size_skew_floor * base_size;
        if target_buy_size < min_size {
            target_buy_size = min_size;
        }
        if target_sell_size < min_size {
            target_sell_size = min_size;
        }

        let target_buy_size = self.round_qty(Qty(target_buy_size));
        let mut target_sell_size = self.round_qty(Qty(target_sell_size));

        // SPOT: Apply sell-side budget constraint
        // Cannot sell more base asset than currently held
        if self.config.is_spot() {
            let current_base = ctx.position(&self.config.instrument_id()).qty;
            if target_sell_size.0 > current_base {
                ctx.log_info(&format!(
                    "Spot sell capped: {} -> {} (base holdings={})",
                    target_sell_size,
                    current_base.max(Decimal::ZERO),
                    current_base
                ));
                target_sell_size = self.trunc_qty(Qty(current_base.max(Decimal::ZERO)));
            }
        }

        // Log skew info when placing new orders
        if self.state.buy_side.can_place() || self.state.sell_side.can_place() {
            let m = &self.state.inventory_metrics;
            let a = &self.state.skew_adjustments;
            ctx.log_info(&format!(
                "Inventory: pos={:.4} pct={:.2}% imbalance={:.4}",
                m.current_qty,
                m.position_pct * Decimal::new(100, 0),
                m.imbalance
            ));
            ctx.log_info(&format!(
                "Skew: price_skew={:.6} buy_mult={:.2} sell_mult={:.2}",
                a.price_skew, a.buy_size_mult, a.sell_size_mult
            ));
        }

        // Check if we need to cancel existing orders (price moved)
        let mut should_cancel_buy = false;
        let mut should_cancel_sell = false;

        if self.should_refresh(mid) {
            // SMART REFRESH: Only cancel if the order is getting WORSE, not better
            //
            // For BUY: Cancel only if new price would be HIGHER (further from fill)
            // For SELL: Cancel only if new price would be LOWER (further from fill)

            if self.state.buy_side.state == OrderState::Active {
                if let Some(current_price) = self.state.buy_side.price {
                    if target_buy_price.0 > current_price.0 {
                        should_cancel_buy = true;
                        ctx.log_info(&format!(
                            "BUY refresh: {} -> {} (moving UP, away from fill)",
                            current_price, target_buy_price
                        ));
                    }
                }
            }

            if self.state.sell_side.state == OrderState::Active {
                if let Some(current_price) = self.state.sell_side.price {
                    if target_sell_price.0 < current_price.0 {
                        should_cancel_sell = true;
                        ctx.log_info(&format!(
                            "SELL refresh: {} -> {} (moving DOWN, away from fill)",
                            current_price, target_sell_price
                        ));
                    }
                }
            }
        }

        // Cancel orders that need refreshing
        if should_cancel_buy {
            if let Some(ref order_id) = self.state.buy_side.order_id {
                ctx.cancel_order(CancelOrder::new(
                    self.config.exchange_instance(),
                    order_id.clone(),
                ));
                self.state.buy_side.set_cancel_pending();
            }
        }

        if should_cancel_sell {
            if let Some(ref order_id) = self.state.sell_side.order_id {
                ctx.cancel_order(CancelOrder::new(
                    self.config.exchange_instance(),
                    order_id.clone(),
                ));
                self.state.sell_side.set_cancel_pending();
            }
        }

        // Place new orders only if state is IDLE
        if self.state.buy_side.can_place() && target_buy_size.0 > Decimal::ZERO {
            let order = PlaceOrder::limit(
                self.config.exchange_instance(),
                self.config.instrument_id(),
                OrderSide::Buy,
                target_buy_price,
                target_buy_size,
            );
            let client_id = order.client_id.clone();

            ctx.log_info(&format!(
                "Placing BUY order: price={} size={}",
                target_buy_price, target_buy_size
            ));

            self.state
                .buy_side
                .set_placed(client_id.clone(), target_buy_price, target_buy_size);
            self.state.register_order(&client_id, OrderSide::Buy);
            ctx.place_order(order);
        }

        if self.state.sell_side.can_place() && target_sell_size.0 > Decimal::ZERO {
            let order = PlaceOrder::limit(
                self.config.exchange_instance(),
                self.config.instrument_id(),
                OrderSide::Sell,
                target_sell_price,
                target_sell_size,
            );
            let client_id = order.client_id.clone();

            ctx.log_info(&format!(
                "Placing SELL order: price={} size={}",
                target_sell_price, target_sell_size
            ));

            self.state
                .sell_side
                .set_placed(client_id.clone(), target_sell_price, target_sell_size);
            self.state.register_order(&client_id, OrderSide::Sell);
            ctx.place_order(order);
        }

        // Update last refresh price
        self.state.last_refresh_price = Some(mid);
    }

    fn should_refresh(&self, mid: Price) -> bool {
        // No refresh if any order has pending cancel
        if self.state.buy_side.is_cancel_pending() || self.state.sell_side.is_cancel_pending() {
            return false;
        }

        // Refresh if price moved significantly from last refresh
        if let Some(last_price) = self.state.last_refresh_price {
            if last_price.0 > Decimal::ZERO {
                let price_change = (mid.0 - last_price.0).abs() / last_price.0;
                if price_change >= self.config.min_price_change_pct {
                    return true;
                }
            }
        }

        false
    }

    // -------------------------------------------------------------------------
    // Inventory & Skew Calculations
    // -------------------------------------------------------------------------

    fn calculate_inventory_metrics(&self, ctx: &dyn StrategyContext) -> InventoryMetrics {
        let position = ctx.position(&self.config.instrument_id());
        let current_qty = position.qty;
        let max_size = self.config.max_position_size;

        // Calculate position as percentage of max
        let raw_position_pct = if max_size > Decimal::ZERO {
            current_qty / max_size
        } else {
            Decimal::ZERO
        };

        // CRITICAL: Clamp position_pct
        // Spot: [0, 1] (cannot go short, min is 0 base holdings)
        // Perp: [-1, +1] (can go short)
        let position_pct = if self.config.is_spot() {
            raw_position_pct.max(Decimal::ZERO).min(Decimal::ONE)
        } else {
            raw_position_pct.max(-Decimal::ONE).min(Decimal::ONE)
        };

        // Normalized to [0, 1] range
        let normalized_pct = (position_pct + Decimal::ONE) / TWO;
        let inventory_ratio = normalized_pct.max(Decimal::ZERO).min(Decimal::ONE);

        // Calculate imbalance from target
        let target_pct = self.config.target_position_pct;
        let target_normalized = (target_pct * TWO) - Decimal::ONE;
        let imbalance = position_pct - target_normalized;

        InventoryMetrics {
            current_qty,
            position_pct,
            inventory_ratio,
            imbalance,
        }
    }

    fn calculate_skew_adjustments(&self) -> SkewAdjustments {
        let metrics = &self.state.inventory_metrics;
        let mode = self.config.skew_mode;

        let mut adjustments = SkewAdjustments::default();

        if mode == SkewMode::None {
            return adjustments;
        }

        let imbalance = metrics.imbalance; // -1 to +1

        // Price skew
        if matches!(mode, SkewMode::Price | SkewMode::Both) {
            let gamma = self.config.price_skew_gamma;
            adjustments.price_skew = gamma * imbalance;
        }

        // Size skew
        if matches!(mode, SkewMode::Size | SkewMode::Both) {
            let floor = self.config.size_skew_floor;

            // Size skew based on imbalance:
            // imbalance > 0 (LONG): reduce buy, increase sell
            // imbalance < 0 (SHORT): increase buy, reduce sell
            // imbalance = 0 (NEUTRAL): both = 1.0

            let scale = Decimal::ONE - floor;
            let mut buy_mult = Decimal::ONE - (imbalance * scale);
            let mut sell_mult = Decimal::ONE + (imbalance * scale);

            // Clamp to [floor, 2-floor] range
            let max_mult = TWO - floor;
            buy_mult = buy_mult.max(floor).min(max_mult);
            sell_mult = sell_mult.max(floor).min(max_mult);

            adjustments.buy_size_mult = buy_mult;
            adjustments.sell_size_mult = sell_mult;
        }

        adjustments
    }

    // -------------------------------------------------------------------------
    // PnL Validation
    // -------------------------------------------------------------------------

    fn validate_pnl(&mut self, ctx: &mut dyn StrategyContext) {
        if let Some(pnl) = self.state.current_pnl {
            if let Some(stop_loss) = self.config.stop_loss {
                if pnl <= stop_loss {
                    let reason = format!("[TerminatePNL]: {} <= stop_loss: {}", pnl, stop_loss);
                    ctx.log_warn(&reason);
                    self.state.exit_reason = Some(reason.clone());
                    ctx.stop_strategy(self.config.strategy_id.clone(), &reason);
                    return;
                }
            }
            if let Some(take_profit) = self.config.take_profit {
                if pnl >= take_profit {
                    let reason = format!("[TerminatePNL]: {} >= take_profit: {}", pnl, take_profit);
                    ctx.log_info(&reason);
                    self.state.exit_reason = Some(reason.clone());
                    ctx.stop_strategy(self.config.strategy_id.clone(), &reason);
                    return;
                }
            }
        }
    }

    // -------------------------------------------------------------------------
    // Logging
    // -------------------------------------------------------------------------

    fn log_status(&self, ctx: &dyn StrategyContext) {
        let buy_info = format!(
            "state={:?} price={:?} size={:?}",
            self.state.buy_side.state, self.state.buy_side.price, self.state.buy_side.size
        );
        let sell_info = format!(
            "state={:?} price={:?} size={:?}",
            self.state.sell_side.state, self.state.sell_side.price, self.state.sell_side.size
        );

        ctx.log_info(&format!(
            "Status: mid={:?} pos={:.4} BUY=[{}] SELL=[{}]",
            self.state.mid_price, self.state.current_position, buy_info, sell_info
        ));
    }
}

// =============================================================================
// Strategy trait implementation
// =============================================================================

impl Strategy for MarketMaker {
    fn id(&self) -> &StrategyId {
        &self.config.strategy_id
    }

    fn on_start(&mut self, ctx: &mut dyn StrategyContext) {
        self.handle_start(ctx);
    }

    fn on_event(&mut self, ctx: &mut dyn StrategyContext, event: &Event) {
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
                // Determine if this fill completes the order
                // For now, we'll treat each fill as potentially complete
                // The engine should emit OrderCompleted for fully filled orders
                self.handle_order_filled(ctx, &e.client_id, e.side, e.qty, e.price, false);
            }
            Event::OrderCompleted(e) => {
                self.handle_order_terminal(ctx, &e.client_id);
            }
            Event::ExchangeStateChanged(e) => {
                ctx.log_info(&format!(
                    "Exchange state changed: {:?} -> {:?} ({})",
                    e.old_state, e.new_state, e.reason
                ));
            }
            Event::FundingRate(_) => {
                // Funding rate events are informational for market makers
            }
        }
    }

    fn on_timer(&mut self, ctx: &mut dyn StrategyContext, _timer_id: TimerId) {
        // We use quote-driven updates, not timers
        // But if you want periodic checks, handle them here
        if let Some(mid) = self.state.mid_price {
            self.sync_orders(ctx, mid);
        }
    }

    fn on_stop(&mut self, ctx: &mut dyn StrategyContext) {
        self.handle_stop(ctx);
    }
}
