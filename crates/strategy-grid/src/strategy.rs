//! Grid trading strategy implementation.
//!
//! Supports three modes:
//! - LONG: Buy at each level, sell to take profit
//! - SHORT: Sell at each level, buy to take profit
//! - NEUTRAL: Buy below mid, sell above mid

use crate::config::{GridConfig, GridMode};
use crate::state::{GridLevel, GridLevelState, GridState, LiquidationBoundaries, OrderKind};

use bot_core::{
    CancelAll, CancelOrder, ClientOrderId, Event, ExchangeHealth, InstrumentMeta, OrderSide,
    PlaceOrder, Price, Qty, Strategy, StrategyContext, StrategyId, TimerId,
};
use rust_decimal::Decimal;

const TWO: Decimal = Decimal::TWO;
const ONE: Decimal = Decimal::ONE;

/// Fee buffer for perpetuals (0.08% = 8/10000)
fn fee_buffer() -> Decimal {
    Decimal::new(8, 4)
}

/// Grid Trading Strategy
///
/// Creates a grid of limit orders at predefined price levels.
/// When an order fills, a take-profit order is placed in the opposite direction.
/// When the take-profit fills, the cycle repeats.
pub struct GridStrategy {
    config: GridConfig,
    state: GridState,
    instrument_meta: Option<InstrumentMeta>,
}

impl GridStrategy {
    /// Create a new grid strategy with the given configuration.
    pub fn new(config: GridConfig) -> Self {
        Self {
            config,
            state: GridState::new(),
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

    /// Get tick size from instrument meta.
    fn tick_size(&self) -> Decimal {
        self.instrument_meta
            .as_ref()
            .map(|m| m.tick_size)
            .unwrap_or(Decimal::new(1, 2))
    }

    /// Get lot size from instrument meta.
    fn lot_size(&self) -> Decimal {
        self.instrument_meta
            .as_ref()
            .map(|m| m.lot_size)
            .unwrap_or(Decimal::new(1, 4))
    }

    /// Get minimum quantity from instrument meta.
    fn min_qty(&self) -> Decimal {
        self.instrument_meta
            .as_ref()
            .and_then(|m| m.min_qty)
            .unwrap_or(self.lot_size())
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
            self.stop_strategy(
                ctx,
                &format!(
                    "Grid configuration validation failed: {}",
                    errors.join("; ")
                ),
            );
            return;
        }

        ctx.log_info(&format!(
            "GridStrategy started: {} mode={:?} levels={} range=[{}, {}] leverage={}",
            self.config.instrument_id(),
            self.config.grid_mode,
            self.config.grid_levels,
            self.config.start_price,
            self.config.end_price,
            self.config.leverage
        ));

        // Log trailing config so debuggers can see limits at a glance
        if self.config.trailing_enabled() {
            ctx.log_info(&format!(
                "Trailing grid enabled: up_limit={:?} down_limit={:?}",
                self.config.trailing_up_limit, self.config.trailing_down_limit,
            ));
        }
    }

    fn handle_stop(&mut self, ctx: &mut dyn StrategyContext) {
        ctx.log_info("GridStrategy stopping - canceling all orders");

        ctx.cancel_all(CancelAll::for_instrument(
            self.config.exchange_instance(),
            self.config.instrument_id(),
        ));

        self.state.reset_all_levels();
    }

    // =========================================================================
    // Event Handlers
    // =========================================================================

    fn handle_quote(&mut self, ctx: &mut dyn StrategyContext, bid: Price, ask: Price) {
        let mid = Price((bid.0 + ask.0) / TWO);
        self.state.mid_price = Some(mid);

        // Check for exit condition
        if self.state.exit_reason.is_some() {
            return;
        }

        // Check PnL limits
        self.validate_pnl(ctx);

        // Initialize grid if not done
        if !self.state.is_initialized {
            self.build_grid(ctx, mid);

            // Check if grid initialization triggered a stop (e.g., liquidation safety)
            if self.state.exit_reason.is_some() {
                return;
            }
        }

        // Check liquidation safety
        if !self.config.is_spot() && !self.state.is_price_safe(mid) {
            self.stop_strategy(
                ctx,
                &format!(
                    "Price {} outside safe boundaries: min={:?} max={:?}",
                    mid,
                    self.state.liquidation_boundaries.min_safe_price,
                    self.state.liquidation_boundaries.max_safe_price
                ),
            );
            return;
        }

        // Periodic logging
        let now = ctx.now_ms();
        if now - self.state.last_log_ts > 5000 {
            self.log_status(ctx);
            self.state.last_log_ts = now;
        }

        // Sync orders
        self.sync_orders(ctx);

        // Attempt trailing window slide (no-op when trailing is disabled)
        self.maybe_trailing_slide(ctx, mid);
    }

    fn handle_order_accepted(&mut self, ctx: &mut dyn StrategyContext, client_id: &ClientOrderId) {
        ctx.log_info(&format!("Order accepted: {}", client_id));
        // State transitions happen in place_order, nothing to do here
    }

    fn handle_order_rejected(
        &mut self,
        ctx: &mut dyn StrategyContext,
        client_id: &ClientOrderId,
        reason: &str,
    ) {
        ctx.log_warn(&format!("Order rejected: {} reason={}", client_id, reason));
        self.reset_level_from_order(ctx, client_id, true);
    }

    fn handle_order_canceled(&mut self, ctx: &mut dyn StrategyContext, client_id: &ClientOrderId) {
        ctx.log_info(&format!("Order canceled: {}", client_id));
        self.reset_level_from_order(ctx, client_id, false);
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

        let Some((level_idx, kind)) = self.state.order_info(client_id) else {
            ctx.log_info(&format!(
                "Fill for unmanaged order {} - ignoring",
                client_id
            ));
            return;
        };

        // Validate level index
        if level_idx >= self.state.levels.len() {
            ctx.log_warn(&format!(
                "Fill for invalid level index {} - ignoring",
                level_idx
            ));
            return;
        }

        match kind {
            OrderKind::Open => {
                // Accumulate fill quantity
                self.state.levels[level_idx].filled_open_qty += qty;

                if is_complete {
                    // Transition to OPEN_FILLED
                    self.state.levels[level_idx].set_open_filled();
                    self.state.mark_dirty(level_idx); // Trigger sync for close order
                    let entry_price = self.state.levels[level_idx].entry_price;
                    let filled_qty = self.state.levels[level_idx].filled_open_qty;
                    self.state.unregister_order(client_id);
                    ctx.log_info(&format!(
                        "Level {} open filled @ {} qty={}",
                        level_idx, entry_price, filled_qty
                    ));
                } else {
                    let filled_qty = self.state.levels[level_idx].filled_open_qty;
                    let total_qty = self.state.levels[level_idx].quantity;
                    ctx.log_info(&format!(
                        "Level {} partial open fill: {} / {}",
                        level_idx, filled_qty, total_qty
                    ));
                }
            }
            OrderKind::Close => {
                if is_complete {
                    // Cycle complete - reset to IDLE
                    let tp_price = self.state.levels[level_idx].take_profit_price;
                    self.state.levels[level_idx].reset();
                    self.state.mark_dirty(level_idx); // Trigger sync for new open order
                    self.state.unregister_order(client_id);
                    ctx.log_info(&format!(
                        "Level {} close filled @ {} - cycle complete, resetting to IDLE",
                        level_idx, tp_price
                    ));
                }
            }
        }
    }

    fn handle_order_terminal(&mut self, ctx: &mut dyn StrategyContext, client_id: &ClientOrderId) {
        // OrderCompleted event - order is fully filled
        if let Some((level_idx, kind)) = self.state.order_info(client_id) {
            if let Some(level) = self.state.level_mut(level_idx) {
                match kind {
                    OrderKind::Open => {
                        level.set_open_filled();
                        ctx.log_info(&format!(
                            "Level {} open completed -> OPEN_FILLED",
                            level_idx
                        ));
                    }
                    OrderKind::Close => {
                        level.reset();
                        ctx.log_info(&format!("Level {} close completed -> IDLE", level_idx));
                    }
                }
            }
            // Mark this level as dirty for sync
            self.state.mark_dirty(level_idx);
            self.state.unregister_order(client_id);
        }
    }

    // =========================================================================
    // Grid Construction
    // =========================================================================

    /// Build the grid levels based on current mid price.
    // =========================================================================
    // Trailing Window Slide
    // =========================================================================

    /// Check whether the grid window should slide and, if so, trigger the
    /// appropriate direction. Respects a 30-second cooldown so the grid
    /// does not thrash on every tick.
    fn maybe_trailing_slide(&mut self, ctx: &mut dyn StrategyContext, mid: Price) {
        if !self.state.is_initialized {
            return;
        }
        if !self.config.trailing_enabled() {
            return;
        }

        let step = self.state.grid_step;
        let now_ms = ctx.now_ms();
        let cooldown_ms: i64 = 30_000;

        if now_ms - self.state.last_slide_ts < cooldown_ms {
            // Emit a debug log so debuggers can see the condition was met but
            // the cooldown is suppressing the slide.
            let remaining_ms = cooldown_ms - (now_ms - self.state.last_slide_ts);
            ctx.log_debug(&format!(
                "Trailing slide cooldown active: {}ms remaining (last_slide={})",
                remaining_ms, self.state.last_slide_ts
            ));
            return;
        }

        let end_price = self.config.end_price;
        let start_price = self.config.start_price;

        if self.config.trailing_up_enabled() && mid.0 > end_price + step {
            // Check hard ceiling
            if let Some(limit) = self.config.trailing_up_limit {
                let new_top = end_price + step;
                if new_top > limit {
                    ctx.log_info(&format!(
                        "Trailing up blocked: new top {} would exceed limit {}",
                        new_top, limit
                    ));
                    return;
                }
            }
            ctx.log_info(&format!(
                "Trailing UP: mid={} > end_price+step={}, sliding window up",
                mid,
                end_price + step
            ));
            self.slide_window_up(ctx);
            self.state.last_slide_ts = now_ms;
        } else if self.config.trailing_down_enabled() && mid.0 < start_price - step {
            // Check hard floor
            if let Some(limit) = self.config.trailing_down_limit {
                let new_bottom = start_price - step;
                if new_bottom < limit {
                    ctx.log_info(&format!(
                        "Trailing down blocked: new bottom {} would go below limit {}",
                        new_bottom, limit
                    ));
                    return;
                }
            }
            ctx.log_info(&format!(
                "Trailing DOWN: mid={} < start_price-step={}, sliding window down",
                mid,
                start_price - step
            ));
            self.slide_window_down(ctx);
            self.state.last_slide_ts = now_ms;
        }
    }

    /// Slide the window upwards:
    ///
    /// 1. Evicts the level at `levels[0]` (lowest price) — cancels any live
    ///    orders, unregisters them from the registry.
    /// 2. Removes that level from the Vec.  This shifts every subsequent level
    ///    down by one position in memory.
    /// 3. Calls `reindex_after_shift(-1)` to keep `level.index`, `order_registry`,
    ///    and `dirty_levels` fully consistent with the new positions.
    /// 4. Rebuilds a new level at price `current_max + grid_step`, appends it,
    ///    assigns the correct `index`, and marks it dirty so `sync_orders` places
    ///    a fresh order.
    /// 5. Updates `config.start_price` and `config.end_price`.
    fn slide_window_up(&mut self, ctx: &mut dyn StrategyContext) {
        if self.state.levels.is_empty() {
            return;
        }

        let step = self.state.grid_step;

        // ── Step 1: cancel live orders on the evicted (bottom) level ──────────
        let evicted = self.state.levels[0].clone();
        ctx.log_info(&format!(
            "Slide UP evicting level: price={} open_order={:?} close_order={:?} state={:?}",
            evicted.entry_price, evicted.open_order_id, evicted.close_order_id, evicted.state
        ));
        if let Some(oid) = &evicted.open_order_id {
            ctx.cancel_order(CancelOrder::new(
                self.config.exchange_instance(),
                oid.clone(),
            ));
        }
        if let Some(oid) = &evicted.close_order_id {
            ctx.cancel_order(CancelOrder::new(
                self.config.exchange_instance(),
                oid.clone(),
            ));
        }

        // ── Step 2: remove bottom level and shift all indices by -1 ───────────
        self.state.levels.remove(0);
        self.state.reindex_after_shift(-1);

        // ── Step 3: build new top-level ────────────────────────────────────────
        let new_entry = self.round_price(Price(self.config.end_price + step));
        let new_qty = self.calculate_level_quantity(new_entry, self.state.quote_per_level);
        let new_idx = self.state.levels.len(); // will be appended at this position

        let (new_side, new_tp, new_active) = match self.config.grid_mode {
            GridMode::Long => {
                let tp = self.round_price(Price(new_entry.0 + step));
                (OrderSide::Buy, tp, true)
            }
            GridMode::Short => {
                let tp = self.round_price(Price(new_entry.0 - step));
                (OrderSide::Sell, tp, true)
            }
            GridMode::Neutral => {
                // In neutral mode, new level above current range → SELL (short)
                let tp = self.round_price(Price(new_entry.0 - step));
                (OrderSide::Sell, tp, true)
            }
        };

        let mut new_level = GridLevel::new(new_idx, new_entry, new_tp, new_qty, new_side);
        new_level.is_active = new_active;
        new_level.index = new_idx;
        self.state.levels.push(new_level);
        self.state.mark_dirty(new_idx);

        // ── Step 4: update config bounds ──────────────────────────────────────
        self.config.start_price = self.state.levels[0].entry_price.0;
        self.config.end_price = new_entry.0;

        ctx.log_info(&format!(
            "Slide UP complete: evicted price={} new_top={} window=[{}, {}]",
            evicted.entry_price, new_entry, self.config.start_price, self.config.end_price
        ));
    }

    /// Slide the window downwards (inverse of `slide_window_up`):
    ///
    /// 1. Evicts `levels[last]` (highest price).
    /// 2. Removes it — no index shift needed since the tail is removed.
    /// 3. Prepends a new level at price `current_min - grid_step`, which shifts
    ///    every existing level up by one. Calls `reindex_after_shift(+1)`.
    /// 4. Updates config bounds.
    fn slide_window_down(&mut self, ctx: &mut dyn StrategyContext) {
        if self.state.levels.is_empty() {
            return;
        }

        let step = self.state.grid_step;

        // ── Step 1: cancel live orders on the evicted (top) level ─────────────
        let last_idx = self.state.levels.len() - 1;
        let evicted = self.state.levels[last_idx].clone();
        ctx.log_info(&format!(
            "Slide DOWN evicting level: price={} open_order={:?} close_order={:?} state={:?}",
            evicted.entry_price, evicted.open_order_id, evicted.close_order_id, evicted.state
        ));
        if let Some(oid) = &evicted.open_order_id {
            ctx.cancel_order(CancelOrder::new(
                self.config.exchange_instance(),
                oid.clone(),
            ));
        }
        if let Some(oid) = &evicted.close_order_id {
            ctx.cancel_order(CancelOrder::new(
                self.config.exchange_instance(),
                oid.clone(),
            ));
        }

        // ── Step 2: remove top level (no shift needed for remaining levels) ───
        // NOTE: removing from tail does NOT shift other indices, so no reindex
        // pass is needed here — but we still need to clean up dirty/registry
        // entries pointing to the evicted index.
        self.state.levels.pop();
        // Evicted index was `last_idx`; filter it out from dirty / registry
        self.state.dirty_levels.remove(&last_idx);
        // order_registry entries for the evicted level are already gone since
        // we cancelled orders; filter defensively
        self.state
            .order_registry
            .retain(|_, (idx, _)| *idx != last_idx);

        // ── Step 3: prepend new bottom level ──────────────────────────────────
        // Prepending shifts every element up by 1 → reindex +1
        let new_entry = self.round_price(Price(self.config.start_price - step));
        let new_qty = self.calculate_level_quantity(new_entry, self.state.quote_per_level);

        let (new_side, new_tp, new_active) = match self.config.grid_mode {
            GridMode::Long => {
                let tp = self.round_price(Price(new_entry.0 + step));
                (OrderSide::Buy, tp, true)
            }
            GridMode::Short => {
                let tp = self.round_price(Price(new_entry.0 - step));
                // Deactivate bottommost Short level (no room for TP below)
                (OrderSide::Sell, tp, false)
            }
            GridMode::Neutral => {
                // New level below current range → BUY (long)
                let tp = self.round_price(Price(new_entry.0 + step));
                (OrderSide::Buy, tp, true)
            }
        };

        // Insert at position 0 so it becomes the new lowest level
        let mut new_level = GridLevel::new(0, new_entry, new_tp, new_qty, new_side);
        new_level.is_active = new_active;
        self.state.levels.insert(0, new_level);

        // Every previously existing level is now at position +1
        self.state.reindex_after_shift(1);

        // The new bottom level is at index 0 after the reindex pass set everyone
        // else to idx+1, but level.index for the new element was set to 0 before
        // the shift and was incremented as well.  Fix index 0 explicitly:
        self.state.levels[0].index = 0;
        self.state.mark_dirty(0);

        // ── Step 4: update config bounds ──────────────────────────────────────
        self.config.start_price = new_entry.0;
        let new_last = self.state.levels.len() - 1;
        self.config.end_price = self.state.levels[new_last].entry_price.0;

        ctx.log_info(&format!(
            "Slide DOWN complete: evicted price={} new_bottom={} window=[{}, {}]",
            evicted.entry_price, new_entry, self.config.start_price, self.config.end_price
        ));
    }

    fn build_grid(&mut self, ctx: &mut dyn StrategyContext, mid: Price) {
        ctx.log_info(&format!("Building grid at mid price {}", mid));

        let levels = self.config.grid_levels as usize;
        if levels < 2 {
            self.stop_strategy(ctx, "grid_levels must be >= 2");
            return;
        }

        // Calculate effective investment (with fee buffer for perps)
        let effective_investment = if self.config.is_spot() {
            self.config.max_investment_quote
        } else {
            self.config.max_investment_quote * (ONE - fee_buffer())
        };

        // Calculate notional budget and quote per level
        let notional_budget = effective_investment * self.config.leverage;
        let quote_per_level = notional_budget / Decimal::from(levels - 1);

        if quote_per_level < Decimal::new(20, 0) {
            self.stop_strategy(
                ctx,
                &format!(
                    "Quote per level ({}) is too low (must be >= 20)",
                    quote_per_level
                ),
            );
            return;
        }

        self.state.quote_per_level = quote_per_level;
        ctx.log_info(&format!(
            "Notional budget: {}, Quote per level: {}",
            notional_budget, quote_per_level
        ));

        // Calculate raw step
        let raw_step =
            (self.config.end_price - self.config.start_price) / Decimal::from(levels - 1);

        // Round step to tick size
        let tick_size = self.tick_size();
        let step = if raw_step.abs() >= tick_size {
            self.round_price(Price(raw_step.abs())).0
        } else {
            tick_size
        };
        self.state.grid_step = step;

        ctx.log_info(&format!("Grid step: {}", step));

        // Build level metadata first (prices and quantities)
        let mut level_data: Vec<(usize, Price, Qty)> = Vec::with_capacity(levels);
        for idx in 0..levels {
            let base_price = self.config.start_price + raw_step * Decimal::from(idx);
            let entry_price = self.round_price(Price(base_price));
            let quantity = self.calculate_level_quantity(entry_price, quote_per_level);
            level_data.push((idx, entry_price, quantity));
        }

        // Determine idle index for NEUTRAL mode
        let idle_index: Option<usize> = if self.config.grid_mode == GridMode::Neutral {
            Some(
                level_data
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, (_, price, _))| (price.0 - mid.0).abs())
                    .map(|(i, _)| i)
                    .unwrap_or(levels / 2),
            )
        } else {
            None
        };

        if let Some(idx) = idle_index {
            ctx.log_info(&format!("NEUTRAL mode: idle level at index {}", idx));
        }

        // Build actual grid levels
        let mut grid_levels: Vec<GridLevel> = Vec::with_capacity(levels);
        let mut first_buy_price: Option<Price> = None;
        let mut first_sell_price: Option<Price> = None;

        for (idx, entry_price, quantity) in level_data {
            // Determine side and take profit based on mode
            let (side, tp_price, is_active) = match self.config.grid_mode {
                GridMode::Long => {
                    let tp = self.round_price(Price(entry_price.0 + step));
                    // Deactivate the topmost level (no room for TP above)
                    let active = idx < levels - 1;
                    if first_buy_price.is_none() && active {
                        first_buy_price = Some(entry_price);
                    }
                    (OrderSide::Buy, tp, active)
                }
                GridMode::Short => {
                    let tp = self.round_price(Price(entry_price.0 - step));
                    // Deactivate the bottommost level (no room for TP below)
                    let active = idx > 0;
                    if first_sell_price.is_none() && active {
                        first_sell_price = Some(entry_price);
                    }
                    (OrderSide::Sell, tp, active)
                }
                GridMode::Neutral => {
                    if let Some(idle_idx) = idle_index {
                        if idx < idle_idx {
                            // Below current price -> BUY side (long)
                            let tp = self.round_price(Price(entry_price.0 + step));
                            first_buy_price = Some(entry_price);
                            (OrderSide::Buy, tp, true)
                        } else if idx > idle_idx {
                            // Above current price -> SELL side (short)
                            let tp = self.round_price(Price(entry_price.0 - step));
                            if first_sell_price.is_none() {
                                first_sell_price = Some(entry_price);
                            }
                            (OrderSide::Sell, tp, true)
                        } else {
                            // Center level - inactive
                            (OrderSide::Buy, entry_price, false)
                        }
                    } else {
                        // Fallback
                        let tp = self.round_price(Price(entry_price.0 + step));
                        (OrderSide::Buy, tp, true)
                    }
                }
            };

            let mut level = GridLevel::new(idx, entry_price, tp_price, quantity, side);
            level.is_active = is_active;
            grid_levels.push(level);
        }

        self.state.levels = grid_levels;

        // Build liquidation boundaries (skip for spot)
        if !self.config.is_spot() {
            self.build_liquidation_boundaries(
                ctx,
                mid,
                first_buy_price,
                first_sell_price,
                quote_per_level,
            );

            // Check if stop was requested during boundary check
            if self.state.exit_reason.is_some() {
                ctx.log_warn("Grid initialization aborted due to safety check");
                return;
            }
        }

        self.state.is_initialized = true;

        // Mark all levels as dirty for initial order placement
        self.state.mark_all_dirty();

        // Log all levels
        self.log_grid_levels(ctx);
    }

    /// Calculate quantity for a level based on price and quote budget.
    fn calculate_level_quantity(&self, price: Price, quote_budget: Decimal) -> Qty {
        if price.0 <= Decimal::ZERO || quote_budget <= Decimal::ZERO {
            return self.round_qty(Qty(self.config.base_order_size));
        }

        let raw_qty = quote_budget / price.0;
        let min_qty = self.min_qty();

        // Ensure quantity meets minimum
        let qty = if raw_qty < min_qty { min_qty } else { raw_qty };

        let rounded = self.round_qty(Qty(qty));

        // If rounded to zero, use minimum
        if rounded.0 <= Decimal::ZERO {
            self.round_qty(Qty(min_qty))
        } else {
            rounded
        }
    }

    /// Build liquidation safety boundaries.
    fn build_liquidation_boundaries(
        &mut self,
        ctx: &mut dyn StrategyContext,
        mid: Price,
        first_buy_price: Option<Price>,
        first_sell_price: Option<Price>,
        notional_per_level: Decimal,
    ) {
        // Maintenance leverage is typically 2x the max leverage
        let maintenance_leverage = self.config.max_leverage * TWO;
        let leverage = self.config.leverage;

        let mut boundaries = LiquidationBoundaries::default();

        match self.config.grid_mode {
            GridMode::Long => {
                // For LONG: liquidation price of highest entry should be below grid start
                if let Some(first_buy) = first_buy_price {
                    let liq_price = self.calculate_liquidation_price(
                        self.config.end_price,
                        maintenance_leverage,
                        leverage,
                        notional_per_level,
                        OrderSide::Buy,
                    );
                    boundaries.min_safe_price = Some(Price(liq_price));

                    ctx.log_info(&format!(
                        "LONG liquidation boundary: min_safe={} (entry={}, grid_start={})",
                        liq_price, first_buy, self.config.start_price
                    ));

                    // If trailing is enabled, the per-slide limiter prevents the grid from
                    // crossing trailing_down_limit, and the per-tick liq check stops the bot
                    // if price actually reaches the liq zone. No need to block at startup.
                    let effective_lower = self
                        .config
                        .trailing_down_limit
                        .unwrap_or(self.config.start_price);

                    if liq_price > effective_lower {
                        ctx.log_warn(&format!(
                            "Liq warning: liquidation price {} > effective lower bound {} \
                            (trailing_down_limit or grid_start). Per-tick liq check will guard.",
                            liq_price, effective_lower
                        ));
                    }
                }
            }
            GridMode::Short => {
                // For SHORT: liquidation price of lowest entry should be above grid end
                if let Some(first_sell) = first_sell_price {
                    let liq_price = self.calculate_liquidation_price(
                        self.config.start_price,
                        maintenance_leverage,
                        leverage,
                        notional_per_level,
                        OrderSide::Sell,
                    );
                    boundaries.max_safe_price = Some(Price(liq_price));

                    ctx.log_info(&format!(
                        "SHORT liquidation boundary: max_safe={} (entry={}, grid_end={})",
                        liq_price, first_sell, self.config.end_price
                    ));

                    // Per-slide limiter prevents grid from crossing trailing_up_limit.
                    // Per-tick liq check guards if price reaches the liq zone.
                    let effective_upper = self
                        .config
                        .trailing_up_limit
                        .unwrap_or(self.config.end_price);

                    if liq_price < effective_upper {
                        ctx.log_warn(&format!(
                            "Liq warning: liquidation price {} < effective upper bound {} \
                            (trailing_up_limit or grid_end). Per-tick liq check will guard.",
                            liq_price, effective_upper
                        ));
                    }
                }
            }
            GridMode::Neutral => {
                // Both boundaries needed
                if let Some(first_buy) = first_buy_price {
                    let liq_price = self.calculate_liquidation_price(
                        first_buy.0,
                        maintenance_leverage,
                        leverage,
                        notional_per_level,
                        OrderSide::Buy,
                    );
                    boundaries.min_safe_price = Some(Price(liq_price));
                    ctx.log_info(&format!(
                        "NEUTRAL long liquidation boundary: min_safe={} (entry={}, grid_start={})",
                        liq_price, first_buy, self.config.start_price
                    ));

                    // Per-slide limiter + per-tick liq check provide the real protection.
                    let effective_lower = self
                        .config
                        .trailing_down_limit
                        .unwrap_or(self.config.start_price);
                    if liq_price > effective_lower {
                        ctx.log_warn(&format!(
                            "Liq warning: long liquidation price {} > effective lower bound {} \
                            (trailing_down_limit or grid_start). Per-tick liq check will guard.",
                            liq_price, effective_lower
                        ));
                    }
                }

                if let Some(first_sell) = first_sell_price {
                    let liq_price = self.calculate_liquidation_price(
                        first_sell.0,
                        maintenance_leverage,
                        leverage,
                        notional_per_level,
                        OrderSide::Sell,
                    );
                    boundaries.max_safe_price = Some(Price(liq_price));
                    ctx.log_info(&format!(
                        "NEUTRAL short liquidation boundary: max_safe={} (entry={}, grid_end={})",
                        liq_price, first_sell, self.config.end_price
                    ));

                    // Per-slide limiter + per-tick liq check provide the real protection.
                    let effective_upper = self
                        .config
                        .trailing_up_limit
                        .unwrap_or(self.config.end_price);
                    if liq_price < effective_upper {
                        ctx.log_warn(&format!(
                            "Liq warning: short liquidation price {} < effective upper bound {} \
                            (trailing_up_limit or grid_end). Per-tick liq check will guard.",
                            liq_price, effective_upper
                        ));
                    }
                }
            }
        }

        ctx.log_info(&format!(
            "Liquidation boundaries: min={:?} < {} < max={:?}",
            boundaries.min_safe_price, mid, boundaries.max_safe_price
        ));

        self.state.liquidation_boundaries = boundaries;
    }

    /// Calculate liquidation price using Hyperliquid formula.
    ///
    /// liq_price = price - side_val * margin_available / position_size / (1 - l * side_val)
    ///
    /// where:
    ///   l = 1 / maintenance_leverage (MMR)
    ///   side_val = 1 for long, -1 for short
    fn calculate_liquidation_price(
        &self,
        entry_price: Decimal,
        maintenance_leverage: Decimal,
        leverage: Decimal,
        notional: Decimal,
        side: OrderSide,
    ) -> Decimal {
        if leverage <= Decimal::ZERO
            || maintenance_leverage <= Decimal::ZERO
            || entry_price <= Decimal::ZERO
            || notional <= Decimal::ZERO
        {
            return entry_price;
        }

        let side_val = match side {
            OrderSide::Buy => ONE,
            OrderSide::Sell => -ONE,
        };

        // l = maintenance margin rate = 1 / maintenance_leverage
        let l = ONE / maintenance_leverage;

        // Initial margin rate = 1 / leverage
        let imr = ONE / leverage;

        // Position size in asset units
        let position_size = notional / entry_price;

        // Isolated margin = initial margin = notional / leverage
        let isolated_margin = notional * imr;

        // Maintenance margin required = notional * l
        let maintenance_margin = notional * l;

        // margin_available = isolated_margin - maintenance_margin
        let margin_available = isolated_margin - maintenance_margin;

        // Denominator = 1 - l * side_val
        let denominator = ONE - l * side_val;

        // liq_price = entry - side_val * margin_available / position_size / denominator
        entry_price - side_val * margin_available / position_size / denominator
    }

    // =========================================================================
    // Order Management
    // =========================================================================

    /// Sync orders - place open/close orders for levels that need them.
    /// Orders are batched together and sent in a single API call.
    ///
    /// Optimization: Only checks dirty levels (O(dirty) instead of O(all_levels)).
    /// This is crucial for performance with large grid counts.
    fn sync_orders(&mut self, ctx: &mut dyn StrategyContext) {
        // Fast path: skip if no dirty levels
        if !self.state.has_dirty_levels() {
            return;
        }

        // Check exchange health
        if ctx.exchange_health(&self.config.exchange_instance()) == ExchangeHealth::Halted {
            ctx.log_debug("Exchange halted, skipping order sync");
            return;
        }

        // First, reconcile any stale orders
        self.reconcile_orders(ctx);

        // Collect orders to place - only check dirty levels
        let mut orders_to_place: Vec<(usize, OrderKind)> = Vec::new();

        // Clone dirty_levels to avoid borrow issues
        let dirty_indices: Vec<usize> = self.state.dirty_levels.iter().copied().collect();

        for level_idx in dirty_indices {
            if let Some(level) = self.state.level(level_idx) {
                if !level.is_active {
                    continue;
                }

                if level.can_place_open() {
                    orders_to_place.push((level_idx, OrderKind::Open));
                } else if level.can_place_close() {
                    orders_to_place.push((level_idx, OrderKind::Close));
                }
            }
        }

        // Clear dirty tracking
        self.state.clear_dirty();

        if orders_to_place.is_empty() {
            return;
        }

        // Build batch of orders
        let mut batch_orders: Vec<PlaceOrder> = Vec::with_capacity(orders_to_place.len());

        for (level_idx, kind) in &orders_to_place {
            if let Some(order) = self.build_order_for_level(ctx, *level_idx, *kind) {
                batch_orders.push(order);
            }
        }

        if batch_orders.is_empty() {
            return;
        }

        ctx.log_info(&format!("Placing {} orders in batch", batch_orders.len()));

        // Send all orders in a single batch
        ctx.place_orders(batch_orders);
    }

    /// Build a PlaceOrder for a level without sending it.
    /// Returns None if the order cannot be built (e.g., no filled qty for close order).
    fn build_order_for_level(
        &mut self,
        ctx: &dyn StrategyContext,
        level_idx: usize,
        kind: OrderKind,
    ) -> Option<PlaceOrder> {
        match kind {
            OrderKind::Open => self.build_open_order(ctx, level_idx),
            OrderKind::Close => self.build_close_order(ctx, level_idx),
        }
    }

    /// Build an open order for a level.
    fn build_open_order(
        &mut self,
        ctx: &dyn StrategyContext,
        level_idx: usize,
    ) -> Option<PlaceOrder> {
        let level = self.state.level(level_idx)?;

        let client_id = ClientOrderId::generate();
        let price = level.entry_price;
        let qty = level.quantity;
        let side = level.side;

        let order = PlaceOrder::limit(
            self.config.exchange_instance(),
            self.config.instrument_id(),
            side,
            price,
            qty,
        )
        .with_client_id(client_id.clone());

        ctx.log_info(&format!(
            "Placing OPEN order: level={} side={} price={} qty={} cloid={}",
            level_idx, side, price, qty, client_id
        ));

        // Update level state
        if let Some(level) = self.state.level_mut(level_idx) {
            level.set_open_placed(client_id.clone());
        }
        self.state
            .register_order(&client_id, level_idx, OrderKind::Open);

        Some(order)
    }

    /// Build a close (take profit) order for a level.
    fn build_close_order(
        &mut self,
        ctx: &dyn StrategyContext,
        level_idx: usize,
    ) -> Option<PlaceOrder> {
        let level = self.state.level(level_idx)?;

        let price = level.take_profit_price;
        let raw_qty = level.filled_open_qty; // Net qty after fee deduction
        let side = level.close_side();

        // Safety: ensure we have quantity to close
        if raw_qty.0 <= Decimal::ZERO {
            ctx.log_warn(&format!(
                "Level {} has no filled qty to close - skipping",
                level_idx
            ));
            return None;
        }

        // For close/sell orders, truncate qty DOWN to lot size to avoid overselling.
        // Standard rounding could round up (e.g., 1.339 → 1.34) which would try
        // to sell more than the net qty received after fee deduction.
        let qty = self.trunc_qty(raw_qty);

        // Safety: ensure truncated qty is > 0
        if qty.0 <= Decimal::ZERO {
            ctx.log_warn(&format!(
                "Level {} close qty rounds to zero (raw={}) - skipping",
                level_idx, raw_qty
            ));
            return None;
        }

        let client_id = ClientOrderId::generate();

        let order = PlaceOrder::limit(
            self.config.exchange_instance(),
            self.config.instrument_id(),
            side,
            price,
            qty,
        )
        .with_client_id(client_id.clone());

        ctx.log_info(&format!(
            "Placing CLOSE order: level={} side={} price={} qty={} cloid={}",
            level_idx, side, price, qty, client_id
        ));

        // Update level state
        if let Some(level) = self.state.level_mut(level_idx) {
            level.set_close_placed(client_id.clone());
        }
        self.state
            .register_order(&client_id, level_idx, OrderKind::Close);

        Some(order)
    }

    /// Reset a level based on order event (rejection, cancellation).
    fn reset_level_from_order(
        &mut self,
        ctx: &mut dyn StrategyContext,
        client_id: &ClientOrderId,
        is_rejection: bool,
    ) {
        let Some((level_idx, kind)) = self.state.unregister_order(client_id) else {
            return;
        };

        let Some(level) = self.state.level_mut(level_idx) else {
            return;
        };

        match kind {
            OrderKind::Open => {
                level.open_order_id = None;
                if is_rejection {
                    // On rejection, clear fill qty too
                    level.filled_open_qty = Qty::new(Decimal::ZERO);
                }
                level.state = GridLevelState::Idle;
                ctx.log_info(&format!(
                    "Level {} open order {} -> IDLE",
                    level_idx,
                    if is_rejection { "rejected" } else { "canceled" }
                ));
            }
            OrderKind::Close => {
                level.close_order_id = None;
                level.state = GridLevelState::OpenFilled;
                ctx.log_info(&format!(
                    "Level {} close order {} -> OPEN_FILLED (will retry)",
                    level_idx,
                    if is_rejection { "rejected" } else { "canceled" }
                ));
            }
        }
        // Trigger sync after state change
        self.state.mark_dirty(level_idx);
    }

    /// Reconcile order state with cached orders.
    fn reconcile_orders(&mut self, ctx: &mut dyn StrategyContext) {
        // Check for orphaned orders in our registry that might have been
        // canceled externally or filled without us knowing
        let registered_ids: Vec<ClientOrderId> = self
            .state
            .order_registry
            .keys()
            .map(|s| ClientOrderId::new(s.clone()))
            .collect();

        for client_id in registered_ids {
            // Check if order exists in engine's cache
            if ctx.order(&client_id).is_none() {
                // Order no longer tracked by engine - likely external cancellation
                // or fill that we missed
                ctx.log_warn(&format!(
                    "Reconciliation: order {} not in cache - resetting level",
                    client_id
                ));
                self.reset_level_from_order(ctx, &client_id, false);
            }
        }
    }

    // =========================================================================
    // PnL Validation
    // =========================================================================

    fn validate_pnl(&mut self, ctx: &mut dyn StrategyContext) {
        // Skip if neither SL nor TP is configured
        if self.config.stop_loss.is_none() && self.config.take_profit.is_none() {
            return;
        }

        // Read net PnL directly from engine position — same pattern as ctx.position() elsewhere.
        // realized_pnl = cumulative closed trade PnL tracked by the engine fill accounting.
        // unrealized_pnl = mark-to-market on current open position (computed from mid price in ctx).
        let pos = ctx.position(&self.config.instrument_id());
        let net_pnl = pos.realized_pnl + pos.unrealized_pnl.unwrap_or_default();

        ctx.log_debug(&format!(
            "PnL check: net={} realized={} unrealized={:?} | sl={:?} tp={:?}",
            net_pnl,
            pos.realized_pnl,
            pos.unrealized_pnl,
            self.config.stop_loss,
            self.config.take_profit,
        ));

        if let Some(stop_loss) = self.config.stop_loss {
            if net_pnl <= stop_loss {
                self.stop_strategy(
                    ctx,
                    &format!(
                        "Stop loss triggered: net_pnl={} (realized={} unrealized={:?}) <= threshold={}",
                        net_pnl, pos.realized_pnl, pos.unrealized_pnl, stop_loss
                    ),
                );
                return;
            }
        }

        if let Some(take_profit) = self.config.take_profit {
            if net_pnl >= take_profit {
                self.stop_strategy(
                    ctx,
                    &format!(
                        "Take profit triggered: net_pnl={} (realized={} unrealized={:?}) >= threshold={}",
                        net_pnl, pos.realized_pnl, pos.unrealized_pnl, take_profit
                    ),
                );
            }
        }
    }

    // =========================================================================
    // Utility
    // =========================================================================

    fn stop_strategy(&mut self, ctx: &mut dyn StrategyContext, reason: &str) {
        ctx.log_warn(&format!("Stopping strategy: {}", reason));
        self.state.exit_reason = Some(reason.to_string());
        // Request engine to stop this strategy, which will call on_stop handler
        ctx.stop_strategy(self.config.strategy_id.clone(), reason);
    }

    fn log_status(&self, ctx: &dyn StrategyContext) {
        let (idle, open_placed, open_filled, close_placed) = self.state.level_state_counts();
        let position = ctx.position(&self.config.instrument_id());

        ctx.log_info(&format!(
            "Grid status: mid={:?} levels=[IDLE:{} OPEN_PLACED:{} OPEN_FILLED:{} CLOSE_PLACED:{}] position=[qty:{} avg_entry:{:?}]",
            self.state.mid_price, idle, open_placed, open_filled, close_placed,
            position.qty, position.avg_entry_px
        ));
        ctx.log_info(&format!(
            "Grid window: start={} end={}",
            self.config.start_price, self.config.end_price
        ));
    }

    fn log_grid_levels(&self, ctx: &dyn StrategyContext) {
        ctx.log_info("=== Grid Levels ===");
        for level in &self.state.levels {
            ctx.log_info(&format!(
                "  Level {}: entry={} tp={} qty={} side={} active={} state={:?}",
                level.index,
                level.entry_price,
                level.take_profit_price,
                level.quantity,
                level.side,
                level.is_active,
                level.state
            ));
        }
        ctx.log_info("===================");
    }
}

// =============================================================================
// Strategy trait implementation
// =============================================================================

impl Strategy for GridStrategy {
    fn id(&self) -> &StrategyId {
        &self.config.strategy_id
    }

    fn on_start(&mut self, ctx: &mut dyn StrategyContext) {
        self.handle_start(ctx);
    }

    fn on_event(&mut self, ctx: &mut dyn StrategyContext, event: &Event) {
        // Strategy is stopped - ignore all events
        if self.state.exit_reason.is_some() {
            ctx.log_debug(&format!("Strategy is stopped, ignoring event: {:?}", event));
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
                // Determine if this is a complete fill
                // For now, we'll check the order in cache
                let is_complete = ctx
                    .order(&e.client_id)
                    .map(|o| o.is_complete())
                    .unwrap_or(false);
                // Use net_qty to account for fees deducted from spot BUY fills
                self.handle_order_filled(ctx, &e.client_id, e.net_qty, e.price, is_complete);
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
                // Funding rate events are informational for grid bots
            }
        }
    }

    fn on_timer(&mut self, ctx: &mut dyn StrategyContext, _timer_id: TimerId) {
        // Strategy is stopped - ignore timers
        if self.state.exit_reason.is_some() {
            return;
        }

        // Timer-driven sync (if needed)
        if let Some(mid) = self.state.mid_price {
            self.handle_quote(ctx, mid, mid);
        }
    }

    fn on_stop(&mut self, ctx: &mut dyn StrategyContext) {
        self.handle_stop(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_liquidation_price_calculation_long() {
        let strategy = GridStrategy::new(GridConfig::default());

        // Example: Entry at 100k, 50x max leverage (25x maintenance), 10x position leverage
        let liq_price = strategy.calculate_liquidation_price(
            Decimal::new(100000, 0), // entry
            Decimal::new(50, 0),     // maintenance leverage
            Decimal::new(10, 0),     // position leverage
            Decimal::new(1000, 0),   // notional
            OrderSide::Buy,
        );

        // For long, liquidation should be below entry
        assert!(liq_price < Decimal::new(100000, 0));
        assert!(liq_price > Decimal::ZERO);
    }

    #[test]
    fn test_liquidation_price_calculation_short() {
        let strategy = GridStrategy::new(GridConfig::default());

        let liq_price = strategy.calculate_liquidation_price(
            Decimal::new(100000, 0),
            Decimal::new(50, 0),
            Decimal::new(10, 0),
            Decimal::new(1000, 0),
            OrderSide::Sell,
        );

        // For short, liquidation should be above entry
        assert!(liq_price > Decimal::new(100000, 0));
    }
}
