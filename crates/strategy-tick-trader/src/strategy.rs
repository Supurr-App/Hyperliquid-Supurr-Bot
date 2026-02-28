//! Tick Trader strategy implementation.
//!
//! Lifecycle:
//!   WaitingToOpen → (tick N) → OpeningPosition → (fill) →
//!   WaitingToClose → (tick M) → ClosingPosition → (fill) → Done

use crate::config::TickTraderConfig;
use crate::state::{Phase, TickTraderState};
use bot_core::*;

pub struct TickTrader {
    config: TickTraderConfig,
    state: TickTraderState,
    instrument_meta: Option<InstrumentMeta>,
}

impl TickTrader {
    pub fn new(config: TickTraderConfig) -> Self {
        Self {
            config,
            state: TickTraderState::new(),
            instrument_meta: None,
        }
    }

    /// Round price to tick_size
    fn round_price(&self, price: Price) -> Price {
        if let Some(meta) = &self.instrument_meta {
            meta.round_price(price)
        } else {
            price
        }
    }

    /// Round qty to lot_size
    fn round_qty(&self, qty: Qty) -> Qty {
        if let Some(meta) = &self.instrument_meta {
            meta.round_qty(qty)
        } else {
            qty
        }
    }

    /// Get the exchange instance for this config
    fn exchange(&self) -> ExchangeInstance {
        self.config
            .market
            .exchange_instance(self.config.environment)
    }

    /// Get the instrument ID
    fn instrument(&self) -> InstrumentId {
        self.config.market.instrument_id()
    }

    /// The side for the opening order
    fn open_side(&self) -> OrderSide {
        match self.config.side.to_lowercase().as_str() {
            "sell" | "short" => OrderSide::Sell,
            _ => OrderSide::Buy,
        }
    }

    /// The side for the closing order (opposite of open)
    fn close_side(&self) -> OrderSide {
        match self.open_side() {
            OrderSide::Buy => OrderSide::Sell,
            OrderSide::Sell => OrderSide::Buy,
        }
    }
}

impl Strategy for TickTrader {
    fn id(&self) -> &StrategyId {
        &self.config.strategy_id
    }

    fn on_start(&mut self, ctx: &mut dyn StrategyContext) {
        // Cache instrument metadata for rounding
        self.instrument_meta = ctx.instrument_meta(&self.instrument()).cloned();

        // Validate config
        let errors = self.config.validate();
        if !errors.is_empty() {
            ctx.log_error(&format!("Config errors: {:?}", errors));
            ctx.stop_strategy(self.config.strategy_id.clone(), "Invalid config");
            return;
        }

        ctx.log_info(&format!(
            "TickTrader started: {} {} on {} — open after {} ticks, close after {} more ticks, size={}",
            self.open_side(),
            self.config.market.base(),
            self.config.environment,
            self.config.open_after_ticks,
            self.config.close_after_ticks,
            self.config.order_size,
        ));
    }

    fn on_event(&mut self, ctx: &mut dyn StrategyContext, event: &Event) {
        match event {
            Event::Quote(q) => {
                if self.state.phase == Phase::Done {
                    return;
                }

                self.state.tick_count += 1;

                match self.state.phase {
                    Phase::WaitingToOpen => {
                        ctx.log_debug(&format!(
                            "Tick {}/{} — waiting to open",
                            self.state.tick_count, self.config.open_after_ticks
                        ));

                        if self.state.tick_count >= self.config.open_after_ticks {
                            // Time to open! Use market-crossing price for immediate fill
                            let price = match self.open_side() {
                                OrderSide::Buy => self.round_price(q.ask),
                                OrderSide::Sell => self.round_price(q.bid),
                            };
                            let qty = self.round_qty(Qty(self.config.order_size));

                            ctx.log_info(&format!(
                                "Opening position: {} {} @ {} (tick {})",
                                self.open_side(),
                                qty,
                                price,
                                self.state.tick_count
                            ));

                            let order = PlaceOrder::limit(
                                self.exchange(),
                                self.instrument(),
                                self.open_side(),
                                price,
                                qty,
                            );

                            self.state.active_order = Some(order.client_id.clone());
                            ctx.place_order(order);
                            self.state.phase = Phase::OpeningPosition;
                        }
                    }
                    Phase::WaitingToClose => {
                        self.state.ticks_since_open += 1;

                        ctx.log_debug(&format!(
                            "Tick {}/{} since open — waiting to close",
                            self.state.ticks_since_open, self.config.close_after_ticks
                        ));

                        if self.state.ticks_since_open >= self.config.close_after_ticks {
                            // Time to close! Use market-crossing price
                            let price = match self.close_side() {
                                OrderSide::Buy => self.round_price(q.ask),
                                OrderSide::Sell => self.round_price(q.bid),
                            };
                            let qty = self.round_qty(Qty(self.config.order_size));

                            ctx.log_info(&format!(
                                "Closing position: {} {} @ {} (tick {} since open)",
                                self.close_side(),
                                qty,
                                price,
                                self.state.ticks_since_open
                            ));

                            let order = PlaceOrder::limit(
                                self.exchange(),
                                self.instrument(),
                                self.close_side(),
                                price,
                                qty,
                            );

                            self.state.active_order = Some(order.client_id.clone());
                            ctx.place_order(order);
                            self.state.phase = Phase::ClosingPosition;
                        }
                    }
                    _ => {}
                }
            }

            Event::OrderFilled(f) => {
                ctx.log_info(&format!(
                    "Order filled: {} {} @ {} (fee: {:?})",
                    f.side, f.qty, f.price, f.fee
                ));

                match self.state.phase {
                    Phase::OpeningPosition => {
                        ctx.log_info("Position opened! Now counting ticks to close...");
                        self.state.phase = Phase::WaitingToClose;
                        self.state.ticks_since_open = 0;
                        self.state.active_order = None;
                    }
                    Phase::ClosingPosition => {
                        ctx.log_info("Position closed! Strategy complete.");
                        self.state.phase = Phase::Done;
                        self.state.active_order = None;
                        ctx.stop_strategy(
                            self.config.strategy_id.clone(),
                            "Tick trader complete — position closed",
                        );
                    }
                    _ => {}
                }
            }

            Event::OrderCompleted(c) => {
                ctx.log_info(&format!(
                    "Order completed: filled_qty={}, avg_px={:?}",
                    c.filled_qty, c.avg_fill_px
                ));
                // OrderFilled already handled the phase transition
            }

            Event::OrderRejected(r) => {
                ctx.log_error(&format!("Order rejected: {}", r.reason));
                // Stop on rejection — something is wrong
                ctx.stop_strategy(
                    self.config.strategy_id.clone(),
                    &format!("Order rejected: {}", r.reason),
                );
            }

            Event::OrderCanceled(c) => {
                ctx.log_warn(&format!("Order canceled: {:?}", c.reason));
                self.state.active_order = None;
                // Go back to waiting phase so we retry on next tick
                match self.state.phase {
                    Phase::OpeningPosition => {
                        self.state.phase = Phase::WaitingToOpen;
                    }
                    Phase::ClosingPosition => {
                        self.state.phase = Phase::WaitingToClose;
                    }
                    _ => {}
                }
            }

            _ => {}
        }
    }

    fn on_timer(&mut self, _ctx: &mut dyn StrategyContext, _timer_id: TimerId) {
        // No timers used
    }

    fn on_stop(&mut self, ctx: &mut dyn StrategyContext) {
        // Cancel any pending orders
        ctx.cancel_all(CancelAll::new(self.exchange()));
        ctx.log_info(&format!(
            "TickTrader stopped. Total ticks: {}, Phase: {:?}",
            self.state.tick_count, self.state.phase
        ));
    }
}
