//! Spot-Perp Arbitrage strategy implementation.
//!
//! V2 uses a per-leg state machine:
//! - Each leg (spot, perp) independently tracks: Idle → Placed → Filled
//! - The high-level intent (Opening/Closing) determines order directions
//! - One-legged fills retry the failed leg on each quote, stopping after 10s

use crate::config::ArbitrageConfig;

use bot_core::{
    ClientOrderId, Event, ExchangeHealth, InstrumentMeta, OrderSide, PlaceOrder, Price, Qty,
    Strategy, StrategyContext, StrategyId, TimeInForce, TimerId,
};
use rust_decimal::Decimal;

/// Max time (ms) to retry the unfilled leg before giving up
const ONE_LEGGED_RETRY_TIMEOUT_MS: i64 = 20_000;

const TWO: Decimal = Decimal::TWO;

// =============================================================================
// Per-Leg State Machine
// =============================================================================

/// State of a single leg (spot or perp)
#[derive(Debug, Clone)]
pub enum LegState {
    /// No active order, no position from current cycle
    Idle,

    /// Order placed, waiting for fill
    Placed { order_id: ClientOrderId },

    /// Order filled, holding position from current cycle
    Filled {
        order_id: ClientOrderId,
        entry_price: Price,
    },
}

impl LegState {
    fn is_idle(&self) -> bool {
        matches!(self, LegState::Idle)
    }

    fn is_placed(&self) -> bool {
        matches!(self, LegState::Placed { .. })
    }

    fn is_filled(&self) -> bool {
        matches!(self, LegState::Filled { .. })
    }

    fn order_id(&self) -> Option<&ClientOrderId> {
        match self {
            LegState::Placed { order_id } | LegState::Filled { order_id, .. } => Some(order_id),
            LegState::Idle => None,
        }
    }
}

impl Default for LegState {
    fn default() -> Self {
        Self::Idle
    }
}

/// What the strategy is trying to do right now
#[derive(Debug, Clone, PartialEq)]
pub enum ArbIntent {
    /// No active operation
    None,
    /// Opening a hedged position (buy spot, sell perp)
    Opening { entry_spread: Decimal },
    /// Closing a hedged position (sell spot, buy perp)
    Closing,
    /// One leg filled, retrying the failed leg on each quote update
    OneLegged {
        failed_leg: Leg,
        started_at_ms: i64,
        is_closing: bool,
    },
}

impl Default for ArbIntent {
    fn default() -> Self {
        Self::None
    }
}

/// Combined state: per-leg tracking + high-level intent
#[derive(Debug, Clone)]
pub struct ArbState {
    pub spot: LegState,
    pub perp: LegState,
    pub intent: ArbIntent,
}

impl Default for ArbState {
    fn default() -> Self {
        Self {
            spot: LegState::Idle,
            perp: LegState::Idle,
            intent: ArbIntent::None,
        }
    }
}

impl ArbState {
    /// Both legs idle, no intent → ready to trade
    fn is_flat(&self) -> bool {
        self.spot.is_idle() && self.perp.is_idle() && self.intent == ArbIntent::None
    }

    /// Both legs filled → fully hedged position
    fn is_hedged(&self) -> bool {
        self.spot.is_filled() && self.perp.is_filled()
    }

    /// Reset everything to flat
    fn reset(&mut self) {
        self.spot = LegState::Idle;
        self.perp = LegState::Idle;
        self.intent = ArbIntent::None;
    }
}

// =============================================================================
// Strategy
// =============================================================================

/// Spot-Perp Arbitrage Strategy
///
/// Monitors spread between spot and perp markets.
/// Opens hedged position when spread exceeds threshold.
/// Closes when spread converges.
pub struct ArbitrageStrategy {
    config: ArbitrageConfig,
    state: ArbState,
    spot_instrument_meta: Option<InstrumentMeta>,
    perp_instrument_meta: Option<InstrumentMeta>,

    /// Last known prices
    last_spot_mid: Option<Price>,
    last_perp_mid: Option<Price>,

    /// Entry prices for PnL tracking (persisted across state transitions)
    open_spot_entry: Option<Price>,
    open_perp_entry: Option<Price>,
    open_entry_spread: Option<Decimal>,

    /// Logging throttle
    last_log_ts: i64,
}

impl ArbitrageStrategy {
    /// Create a new arbitrage strategy with the given configuration.
    pub fn new(config: ArbitrageConfig) -> Self {
        Self {
            config,
            state: ArbState::default(),
            spot_instrument_meta: None,
            perp_instrument_meta: None,
            last_spot_mid: None,
            last_perp_mid: None,
            open_spot_entry: None,
            open_perp_entry: None,
            open_entry_spread: None,
            last_log_ts: 0,
        }
    }

    // =========================================================================
    // Price & Quantity Rounding
    // =========================================================================

    fn round_spot_price(&self, price: Price) -> Price {
        if let Some(ref meta) = self.spot_instrument_meta {
            meta.round_price(price.trim_to_sig_figs(5))
        } else {
            price
        }
    }

    fn round_perp_price(&self, price: Price) -> Price {
        if let Some(ref meta) = self.perp_instrument_meta {
            meta.round_price(price.trim_to_sig_figs(5))
        } else {
            price
        }
    }

    fn round_spot_qty(&self, qty: Qty) -> Qty {
        if let Some(ref meta) = self.spot_instrument_meta {
            meta.round_qty(qty)
        } else {
            qty
        }
    }

    /// Truncate spot qty DOWN to lot size (floor).
    /// Used for spot sell to avoid overselling after fee deduction.
    fn trunc_spot_qty(&self, qty: Qty) -> Qty {
        if let Some(ref meta) = self.spot_instrument_meta {
            meta.trunc_qty(qty)
        } else {
            qty
        }
    }

    fn round_perp_qty(&self, qty: Qty) -> Qty {
        if let Some(ref meta) = self.perp_instrument_meta {
            meta.round_qty(qty)
        } else {
            qty
        }
    }

    /// Calculate unrealized PnL based on entry and current prices
    fn calculate_unrealized_pnl(
        &self,
        spot_entry: Price,
        perp_entry: Price,
        spot_current: Price,
        perp_current: Price,
    ) -> Decimal {
        // Arb PnL = (current_spread - entry_spread)
        let entry_spread = perp_entry.0 - spot_entry.0;
        let current_spread = perp_current.0 - spot_current.0;
        current_spread - entry_spread
    }

    // =========================================================================
    // Quote Handling
    // =========================================================================

    fn handle_quote(
        &mut self,
        ctx: &mut dyn StrategyContext,
        instrument: &bot_core::InstrumentId,
        bid: Price,
        ask: Price,
    ) {
        let mid = Price((bid.0 + ask.0) / TWO);

        // Update last known prices
        if *instrument == self.config.spot_instrument() {
            self.last_spot_mid = Some(mid);
        } else if *instrument == self.config.perp_instrument() {
            self.last_perp_mid = Some(mid);
        }

        // Need both prices to calculate spread
        let (spot_mid, perp_mid) = match (self.last_spot_mid, self.last_perp_mid) {
            (Some(s), Some(p)) => (s, p),
            _ => return, // Wait for both quotes
        };

        // Calculate spread
        let spread = self.config.calculate_spread(spot_mid.0, perp_mid.0);

        // Periodic logging
        let now = ctx.now_ms();
        if now - self.last_log_ts > 10_000 {
            let pnl_display =
                if let (Some(se), Some(pe)) = (self.open_spot_entry, self.open_perp_entry) {
                    let pnl = self.calculate_unrealized_pnl(se, pe, spot_mid, perp_mid);
                    format!(" pnl={:.2}", pnl)
                } else {
                    String::new()
                };

            ctx.log_info(&format!(
                "Arb status: spot={} perp={} spread={:.4}%{} state={:?}",
                spot_mid,
                perp_mid,
                spread * Decimal::new(100, 0),
                pnl_display,
                self.state.intent
            ));
            self.last_log_ts = now;
        }

        // State machine logic
        match &self.state.intent {
            ArbIntent::None => {
                if self.state.is_flat() && spread >= self.config.min_opening_spread_pct {
                    self.open_position(ctx, spot_mid, perp_mid, spread);
                } else if self.state.is_hedged() && spread <= self.config.min_closing_spread_pct {
                    ctx.log_info(&format!(
                        "Closing position: spread={:.4}% (entry={:.4}%)",
                        spread * Decimal::new(100, 0),
                        self.open_entry_spread.unwrap_or_default() * Decimal::new(100, 0)
                    ));
                    self.close_position(ctx, spot_mid, perp_mid);
                }
            }
            ArbIntent::OneLegged {
                failed_leg,
                started_at_ms,
                is_closing,
            } => {
                let elapsed = now - started_at_ms;
                let is_closing = *is_closing;
                if elapsed > ONE_LEGGED_RETRY_TIMEOUT_MS {
                    let filled = if self.state.spot.is_filled() {
                        "Spot"
                    } else {
                        "Perp"
                    };
                    let reason = format!(
                        "One-legged {} ({} filled) could not fill {:?} after {}s of retries. Stopping — manual intervention required.",
                        if is_closing { "close" } else { "open" },
                        filled,
                        failed_leg,
                        ONE_LEGGED_RETRY_TIMEOUT_MS / 1000
                    );
                    ctx.log_error(&reason);
                    ctx.stop_strategy(self.config.strategy_id.clone(), &reason);
                } else if failed_leg == &Leg::Spot && self.state.spot.is_idle() {
                    self.retry_failed_leg(ctx, Leg::Spot, is_closing, spot_mid, perp_mid);
                } else if failed_leg == &Leg::Perp && self.state.perp.is_idle() {
                    self.retry_failed_leg(ctx, Leg::Perp, is_closing, spot_mid, perp_mid);
                }
                // else: retry order is in-flight (Placed), wait for it to resolve
            }
            // Opening/Closing in progress — wait for order confirmations
            _ => {}
        }
    }

    // =========================================================================
    // Position Management
    // =========================================================================

    /// Open a hedged position: buy spot, sell perp (both simultaneously)
    fn open_position(
        &mut self,
        ctx: &mut dyn StrategyContext,
        spot_mid: Price,
        perp_mid: Price,
        spread: Decimal,
    ) {
        // Check exchange health
        if ctx.exchange_health(&self.config.spot_exchange()) == ExchangeHealth::Halted
            || ctx.exchange_health(&self.config.perp_exchange()) == ExchangeHealth::Halted
        {
            ctx.log_warn("Exchange halted, skipping open");
            return;
        }

        ctx.log_info(&format!(
            "Opening arb position: spread={:.4}% spot={} perp={}",
            spread * Decimal::new(100, 0),
            spot_mid,
            perp_mid
        ));

        // Convert USDC order amount to base qty using average of spot/perp mid
        let avg_mid = (spot_mid.0 + perp_mid.0) / Decimal::TWO;
        let base_amount = self.config.order_amount / avg_mid;
        let spot_qty = self.round_spot_qty(Qty(base_amount));
        let perp_qty = self.round_perp_qty(Qty(base_amount));

        // Apply slippage buffers for limit orders
        let spot_price = self.round_spot_price(Price(
            spot_mid.0 * (Decimal::ONE + self.config.spot_slippage_buffer_pct),
        ));
        let perp_price = self.round_perp_price(Price(
            perp_mid.0 * (Decimal::ONE - self.config.perp_slippage_buffer_pct),
        ));

        let spot_order_id = ClientOrderId::generate();
        let perp_order_id = ClientOrderId::generate();

        let spot_order = PlaceOrder::limit(
            self.config.spot_exchange(),
            self.config.spot_instrument(),
            OrderSide::Buy,
            spot_price,
            spot_qty,
        )
        .with_tif(TimeInForce::Ioc)
        .with_client_id(spot_order_id.clone());

        let perp_order = PlaceOrder::limit(
            self.config.perp_exchange(),
            self.config.perp_instrument(),
            OrderSide::Sell,
            perp_price,
            perp_qty,
        )
        .with_tif(TimeInForce::Ioc)
        .with_client_id(perp_order_id.clone());

        ctx.log_info(&format!(
            "Placing orders: SPOT BUY {} @ {} ({}), PERP SELL {} @ {} ({})",
            spot_qty, spot_price, spot_order_id, perp_qty, perp_price, perp_order_id
        ));

        // Place separately (HL rejects mixed asset classes in one batch)
        ctx.place_orders(vec![spot_order]);
        ctx.place_orders(vec![perp_order]);

        // Transition: both legs to Placed, intent = Opening
        self.state.spot = LegState::Placed {
            order_id: spot_order_id,
        };
        self.state.perp = LegState::Placed {
            order_id: perp_order_id,
        };
        self.state.intent = ArbIntent::Opening {
            entry_spread: spread,
        };
    }

    /// Close the hedged position: sell spot, buy perp
    fn close_position(&mut self, ctx: &mut dyn StrategyContext, spot_mid: Price, perp_mid: Price) {
        // Check exchange health
        if ctx.exchange_health(&self.config.spot_exchange()) == ExchangeHealth::Halted
            || ctx.exchange_health(&self.config.perp_exchange()) == ExchangeHealth::Halted
        {
            ctx.log_warn("Exchange halted, skipping close");
            return;
        }

        // Spot sell: use actual holdings from position tracker (net after fee deduction)
        let spot_position_qty = ctx.position(&self.config.spot_instrument()).qty;
        let spot_qty = self.trunc_spot_qty(Qty(spot_position_qty.max(Decimal::ZERO)));

        // Perp buy: use calculated amount (perp fees are in USDC, not base asset)
        let avg_mid = (spot_mid.0 + perp_mid.0) / Decimal::TWO;
        let perp_base_amount = self.config.order_amount / avg_mid;
        let perp_qty = self.round_perp_qty(Qty(perp_base_amount));

        // Apply slippage buffers
        let spot_price = self.round_spot_price(Price(
            spot_mid.0 * (Decimal::ONE - self.config.spot_slippage_buffer_pct),
        ));
        let perp_price = self.round_perp_price(Price(
            perp_mid.0 * (Decimal::ONE + self.config.perp_slippage_buffer_pct),
        ));

        let spot_order_id = ClientOrderId::generate();
        let perp_order_id = ClientOrderId::generate();

        let spot_order = PlaceOrder::limit(
            self.config.spot_exchange(),
            self.config.spot_instrument(),
            OrderSide::Sell,
            spot_price,
            spot_qty,
        )
        .with_tif(TimeInForce::Ioc)
        .with_client_id(spot_order_id.clone());

        let perp_order = PlaceOrder::limit(
            self.config.perp_exchange(),
            self.config.perp_instrument(),
            OrderSide::Buy,
            perp_price,
            perp_qty,
        )
        .with_tif(TimeInForce::Ioc)
        .with_client_id(perp_order_id.clone());

        ctx.log_info(&format!(
            "Placing close orders: SPOT SELL {} @ {} ({}), PERP BUY {} @ {} ({})",
            spot_qty, spot_price, spot_order_id, perp_qty, perp_price, perp_order_id
        ));

        // Place separately (HL rejects mixed asset classes in one batch)
        ctx.place_orders(vec![spot_order]);
        ctx.place_orders(vec![perp_order]);

        // Transition: both legs to Placed, intent = Closing
        self.state.spot = LegState::Placed {
            order_id: spot_order_id,
        };
        self.state.perp = LegState::Placed {
            order_id: perp_order_id,
        };
        self.state.intent = ArbIntent::Closing;
    }

    // =========================================================================
    // One-legged fill: retry the failed leg
    // =========================================================================

    /// Transition to OneLegged intent — will retry on each quote update.
    fn enter_one_legged(
        &mut self,
        ctx: &mut dyn StrategyContext,
        failed_leg: Leg,
        is_closing: bool,
        now_ms: i64,
    ) {
        ctx.log_warn(&format!(
            "One-legged {}: {:?} rejected/canceled. Will retry on each quote for {}s.",
            if is_closing { "close" } else { "open" },
            failed_leg,
            ONE_LEGGED_RETRY_TIMEOUT_MS / 1000
        ));
        self.state.intent = ArbIntent::OneLegged {
            failed_leg,
            started_at_ms: now_ms,
            is_closing,
        };
    }

    /// Retry placing a failed leg with fresh prices.
    /// Direction depends on whether we're retrying an open or close.
    fn retry_failed_leg(
        &mut self,
        ctx: &mut dyn StrategyContext,
        leg: Leg,
        is_closing: bool,
        spot_mid: Price,
        perp_mid: Price,
    ) {
        match leg {
            Leg::Spot => {
                // Open = BUY spot, Close = SELL spot
                let side = if is_closing {
                    OrderSide::Sell
                } else {
                    OrderSide::Buy
                };
                let spot_qty = if is_closing {
                    // Use actual holdings for close
                    let pos_qty = ctx.position(&self.config.spot_instrument()).qty;
                    self.trunc_spot_qty(Qty(pos_qty.max(Decimal::ZERO)))
                } else {
                    let base_amount = self.config.order_amount / spot_mid.0;
                    self.round_spot_qty(Qty(base_amount))
                };
                let spot_price = if is_closing {
                    self.round_spot_price(Price(
                        spot_mid.0 * (Decimal::ONE - self.config.spot_slippage_buffer_pct),
                    ))
                } else {
                    self.round_spot_price(Price(
                        spot_mid.0 * (Decimal::ONE + self.config.spot_slippage_buffer_pct),
                    ))
                };

                let order_id = ClientOrderId::generate();
                let order = PlaceOrder::limit(
                    self.config.spot_exchange(),
                    self.config.spot_instrument(),
                    side,
                    spot_price,
                    spot_qty,
                )
                .with_tif(TimeInForce::Ioc)
                .with_client_id(order_id.clone());

                ctx.log_info(&format!(
                    "Retrying spot: {:?} {} @ {} ({})",
                    side, spot_qty, spot_price, order_id
                ));

                ctx.place_orders(vec![order]);
                self.state.spot = LegState::Placed { order_id };
            }
            Leg::Perp => {
                // Open = SELL perp, Close = BUY perp
                let side = if is_closing {
                    OrderSide::Buy
                } else {
                    OrderSide::Sell
                };
                let base_amount = self.config.order_amount / perp_mid.0;
                let perp_qty = self.round_perp_qty(Qty(base_amount));
                let perp_price = if is_closing {
                    self.round_perp_price(Price(
                        perp_mid.0 * (Decimal::ONE + self.config.perp_slippage_buffer_pct),
                    ))
                } else {
                    self.round_perp_price(Price(
                        perp_mid.0 * (Decimal::ONE - self.config.perp_slippage_buffer_pct),
                    ))
                };

                let order_id = ClientOrderId::generate();
                let order = PlaceOrder::limit(
                    self.config.perp_exchange(),
                    self.config.perp_instrument(),
                    side,
                    perp_price,
                    perp_qty,
                )
                .with_tif(TimeInForce::Ioc)
                .with_client_id(order_id.clone());

                ctx.log_info(&format!(
                    "Retrying perp: {:?} {} @ {} ({})",
                    side, perp_qty, perp_price, order_id
                ));

                ctx.place_orders(vec![order]);
                self.state.perp = LegState::Placed { order_id };
            }
        }
    }

    // =========================================================================
    // Order Event Helpers
    // =========================================================================

    /// Check if a client_id belongs to the spot or perp leg
    fn identify_leg(&self, client_id: &ClientOrderId) -> Option<Leg> {
        if self.state.spot.order_id() == Some(client_id) {
            Some(Leg::Spot)
        } else if self.state.perp.order_id() == Some(client_id) {
            Some(Leg::Perp)
        } else {
            None
        }
    }

    /// Handle an order being completed (fully filled via IoC)
    fn handle_order_completed(&mut self, ctx: &mut dyn StrategyContext, client_id: &ClientOrderId) {
        let leg = match self.identify_leg(client_id) {
            Some(l) => l,
            None => return,
        };

        match leg {
            Leg::Spot => {
                let order_id = client_id.clone();
                let entry_price = self.last_spot_mid.unwrap_or(Price(Decimal::ZERO));
                ctx.log_info(&format!("Spot leg completed ({})", order_id));
                self.state.spot = LegState::Filled {
                    order_id,
                    entry_price,
                };
            }
            Leg::Perp => {
                let order_id = client_id.clone();
                let entry_price = self.last_perp_mid.unwrap_or(Price(Decimal::ZERO));
                ctx.log_info(&format!("Perp leg completed ({})", order_id));
                self.state.perp = LegState::Filled {
                    order_id,
                    entry_price,
                };
            }
        }

        // Check if both legs are done → transition based on intent
        match &self.state.intent {
            ArbIntent::Opening { entry_spread } => {
                if self.state.is_hedged() {
                    ctx.log_info("Both legs completed - position opened!");
                    self.open_entry_spread = Some(*entry_spread);
                    self.open_spot_entry = self.last_spot_mid;
                    self.open_perp_entry = self.last_perp_mid;
                    self.state.intent = ArbIntent::None;
                    // Keep spot/perp as Filled — is_hedged() returns true
                } else if self.state.spot.is_idle() || self.state.perp.is_idle() {
                    // One leg filled, but the other was already rejected (Idle).
                    // We have a one-legged position → start timeout.
                    let failed = if self.state.spot.is_idle() {
                        Leg::Spot
                    } else {
                        Leg::Perp
                    };
                    ctx.log_warn(&format!(
                        "Only {:?} leg filled, other already rejected — entering one-legged retry",
                        leg
                    ));
                    self.enter_one_legged(ctx, failed, false, self.last_log_ts);
                }
                // else: other leg is still Placed (in-flight), wait for it to resolve
            }
            ArbIntent::Closing => {
                if self.state.spot.is_filled() && self.state.perp.is_filled() {
                    ctx.log_info("Both legs completed - position closed!");
                    self.state.reset();
                    self.open_spot_entry = None;
                    self.open_perp_entry = None;
                    self.open_entry_spread = None;
                } else if self.state.spot.is_idle() || self.state.perp.is_idle() {
                    // One close leg filled, other rejected — retry it
                    let failed = if self.state.spot.is_idle() {
                        Leg::Spot
                    } else {
                        Leg::Perp
                    };
                    ctx.log_warn(&format!(
                        "Only {:?} close leg filled, other rejected — entering one-legged close retry",
                        leg
                    ));
                    self.enter_one_legged(ctx, failed, true, self.last_log_ts);
                }
            }
            ArbIntent::OneLegged { is_closing, .. } => {
                let is_closing = *is_closing;
                if is_closing {
                    // Close retry succeeded — check if position is fully unwound
                    if self.state.spot.is_filled() && self.state.perp.is_filled() {
                        ctx.log_info("Close retry succeeded — position fully closed!");
                        self.state.reset();
                        self.open_spot_entry = None;
                        self.open_perp_entry = None;
                        self.open_entry_spread = None;
                    }
                } else {
                    // Open retry succeeded — check if now hedged
                    if self.state.is_hedged() {
                        ctx.log_info("Open retry succeeded — both legs filled, position opened!");
                        self.open_spot_entry = self.last_spot_mid;
                        self.open_perp_entry = self.last_perp_mid;
                        self.open_entry_spread = None;
                        self.state.intent = ArbIntent::None;
                    }
                }
            }
            ArbIntent::None => {}
        }
    }

    /// Handle an order being rejected
    fn handle_order_rejected(
        &mut self,
        ctx: &mut dyn StrategyContext,
        client_id: &ClientOrderId,
        reason: &str,
    ) {
        let leg = match self.identify_leg(client_id) {
            Some(l) => l,
            None => return,
        };

        ctx.log_error(&format!(
            "Order rejected: {} reason={} leg={:?}",
            client_id, reason, leg
        ));

        match &self.state.intent {
            ArbIntent::Opening { .. } => {
                match leg {
                    Leg::Spot => {
                        self.state.spot = LegState::Idle;
                        if self.state.perp.is_filled() {
                            // Perp filled but spot rejected → one-legged, start retry
                            self.enter_one_legged(ctx, Leg::Spot, false, self.last_log_ts);
                        } else if self.state.perp.is_placed() {
                            // Perp still placed (IoC in-flight) → wait for it to resolve
                            ctx.log_warn("Spot rejected, waiting for perp to resolve...");
                        } else {
                            // Both failed (perp already Idle too)
                            ctx.log_warn("Opening failed: both legs rejected, reverting to flat");
                            self.state.reset();
                        }
                    }
                    Leg::Perp => {
                        self.state.perp = LegState::Idle;
                        if self.state.spot.is_filled() {
                            // Spot filled but perp rejected → one-legged, start retry
                            self.enter_one_legged(ctx, Leg::Perp, false, self.last_log_ts);
                        } else if self.state.spot.is_placed() {
                            // Spot still placed (IoC in-flight) → wait for it to resolve
                            ctx.log_warn("Perp rejected, waiting for spot to resolve...");
                        } else {
                            // Both failed (spot already Idle too)
                            ctx.log_warn("Opening failed: both legs rejected, reverting to flat");
                            self.state.reset();
                        }
                    }
                }
            }
            ArbIntent::Closing => match leg {
                Leg::Spot => {
                    self.state.spot = LegState::Idle;
                    if self.state.perp.is_filled() {
                        self.enter_one_legged(ctx, Leg::Spot, true, self.last_log_ts);
                    }
                }
                Leg::Perp => {
                    self.state.perp = LegState::Idle;
                    if self.state.spot.is_filled() {
                        self.enter_one_legged(ctx, Leg::Perp, true, self.last_log_ts);
                    }
                }
            },
            ArbIntent::OneLegged { .. } => {
                // Retry order rejected — mark leg as Idle so next quote retries it
                match leg {
                    Leg::Spot => self.state.spot = LegState::Idle,
                    Leg::Perp => self.state.perp = LegState::Idle,
                }
            }
            ArbIntent::None => {}
        }
    }

    /// Handle an order being canceled
    fn handle_order_canceled(&mut self, ctx: &mut dyn StrategyContext, client_id: &ClientOrderId) {
        let leg = match self.identify_leg(client_id) {
            Some(l) => l,
            None => return,
        };

        match &self.state.intent {
            ArbIntent::Opening { .. } => match leg {
                Leg::Spot => {
                    self.state.spot = LegState::Idle;
                    if self.state.perp.is_filled() {
                        // Perp filled but spot canceled → one-legged, start retry
                        self.enter_one_legged(ctx, Leg::Spot, false, self.last_log_ts);
                    } else {
                        let reason = format!(
                                "Opening order canceled ({}) on {:?}. Stopping to avoid unhedged position.",
                                client_id, leg
                            );
                        ctx.log_warn(&reason);
                        ctx.stop_strategy(self.config.strategy_id.clone(), &reason);
                    }
                }
                Leg::Perp => {
                    self.state.perp = LegState::Idle;
                    if self.state.spot.is_filled() {
                        // Spot filled but perp canceled → one-legged, start retry
                        self.enter_one_legged(ctx, Leg::Perp, false, self.last_log_ts);
                    } else {
                        let reason = format!(
                                "Opening order canceled ({}) on {:?}. Stopping to avoid unhedged position.",
                                client_id, leg
                            );
                        ctx.log_warn(&reason);
                        ctx.stop_strategy(self.config.strategy_id.clone(), &reason);
                    }
                }
            },
            ArbIntent::Closing => match leg {
                Leg::Spot => {
                    self.state.spot = LegState::Idle;
                    if self.state.perp.is_filled() {
                        self.enter_one_legged(ctx, Leg::Spot, true, self.last_log_ts);
                    } else {
                        let reason = format!(
                            "Close order canceled ({}) on {:?}. Stopping to avoid unhedged position.",
                            client_id, leg
                        );
                        ctx.log_warn(&reason);
                        ctx.stop_strategy(self.config.strategy_id.clone(), &reason);
                    }
                }
                Leg::Perp => {
                    self.state.perp = LegState::Idle;
                    if self.state.spot.is_filled() {
                        self.enter_one_legged(ctx, Leg::Perp, true, self.last_log_ts);
                    } else {
                        let reason = format!(
                            "Close order canceled ({}) on {:?}. Stopping to avoid unhedged position.",
                            client_id, leg
                        );
                        ctx.log_warn(&reason);
                        ctx.stop_strategy(self.config.strategy_id.clone(), &reason);
                    }
                }
            },
            ArbIntent::OneLegged { .. } => {
                // Cancel during retry — mark leg as Idle so next quote retries
                match leg {
                    Leg::Spot => self.state.spot = LegState::Idle,
                    Leg::Perp => self.state.perp = LegState::Idle,
                }
            }
            ArbIntent::None => {}
        }
    }
}

/// Which leg an order belongs to
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Leg {
    Spot,
    Perp,
}

// =============================================================================
// Strategy Trait Implementation
// =============================================================================

impl Strategy for ArbitrageStrategy {
    fn id(&self) -> &StrategyId {
        &self.config.strategy_id
    }

    fn sync_mechanism(&self) -> bot_core::SyncMechanism {
        bot_core::SyncMechanism::Poll
    }

    fn on_start(&mut self, ctx: &mut dyn StrategyContext) {
        let spot_id = self.config.spot_instrument();
        let perp_id = self.config.perp_instrument();
        self.spot_instrument_meta = ctx.instrument_meta(&spot_id).cloned();
        self.perp_instrument_meta = ctx.instrument_meta(&perp_id).cloned();

        if self.spot_instrument_meta.is_none() {
            ctx.log_error(&format!("Spot instrument not found: {}", spot_id));
            return;
        }
        if self.perp_instrument_meta.is_none() {
            ctx.log_error(&format!("Perp instrument not found: {}", perp_id));
            return;
        }

        let errors = self.config.validate();
        if !errors.is_empty() {
            for err in &errors {
                ctx.log_error(&format!("Config error: {}", err));
            }
            return;
        }

        ctx.log_info(&format!(
            "ArbitrageStrategy started: spot={} perp={} amount={} USDC open_spread={} close_spread={}",
            self.config.spot_instrument(),
            self.config.perp_instrument(),
            self.config.order_amount,
            self.config.min_opening_spread_pct,
            self.config.min_closing_spread_pct,
        ));
    }

    fn on_event(&mut self, ctx: &mut dyn StrategyContext, event: &Event) {
        match event {
            Event::Quote(q) => {
                self.handle_quote(ctx, &q.instrument, q.bid, q.ask);
            }
            Event::OrderAccepted(o) => {
                ctx.log_info(&format!("Order accepted: {}", o.client_id));
            }
            Event::OrderRejected(o) => {
                self.handle_order_rejected(ctx, &o.client_id, &o.reason);
            }
            Event::OrderFilled(o) => {
                ctx.log_info(&format!(
                    "Order filled: {} qty={} price={}",
                    o.client_id, o.qty, o.price
                ));
            }
            Event::OrderCompleted(o) => {
                self.handle_order_completed(ctx, &o.client_id);
            }
            Event::OrderCanceled(o) => {
                ctx.log_warn(&format!("Order canceled: {}", o.client_id));
                self.handle_order_canceled(ctx, &o.client_id);
            }
            _ => {}
        }
    }

    fn on_timer(&mut self, _ctx: &mut dyn StrategyContext, _timer_id: TimerId) {
        // No-op: timers not used; retry logic is driven by handle_quote.
    }

    fn on_stop(&mut self, ctx: &mut dyn StrategyContext) {
        ctx.log_info("ArbitrageStrategy stopping");
    }
}
