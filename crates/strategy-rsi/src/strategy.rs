//! RSI strategy implementation.
//!
//! Supports two sizing modes:
//! - `order_size`: fixed base qty (e.g. 0.01 BTC)
//! - `order_notional_quote`: fixed quote amount (e.g. $100), qty computed at runtime
//!
//! Aggregates quote ticks into OHLCV bars, computes RSI via inline Wilder's
//! method, and places buy/sell orders when RSI crosses configurable thresholds.

use crate::bar::BarBuilder;
use crate::config::RsiStrategyConfig;
use crate::indicator::Rsi;
use crate::state::{Phase, RsiState};
use bot_core::*;
use rust_decimal::prelude::ToPrimitive;

pub struct RsiStrategy {
    config: RsiStrategyConfig,
    state: RsiState,
    bar_builder: BarBuilder,
    rsi: Rsi,
    /// Injected from top-level BotConfig (not duplicated in strategy config)
    market: Market,
    /// Injected from top-level BotConfig
    environment: Environment,
    instrument_meta: Option<InstrumentMeta>,
}

impl RsiStrategy {
    pub fn new(config: RsiStrategyConfig, market: Market, environment: Environment) -> Self {
        let rsi = Rsi::new(config.rsi_period as usize);

        Self {
            bar_builder: BarBuilder::new(config.bar_interval_secs),
            rsi,
            state: RsiState::new(),
            config,
            market,
            environment,
            instrument_meta: None,
        }
    }

    // =========================================================================
    // Helpers
    // =========================================================================

    fn round_price(&self, price: Price) -> Price {
        if let Some(meta) = &self.instrument_meta {
            meta.round_price(price)
        } else {
            price
        }
    }

    fn round_qty(&self, qty: Qty) -> Qty {
        if let Some(meta) = &self.instrument_meta {
            meta.round_qty(qty)
        } else {
            qty
        }
    }

    fn exchange(&self) -> ExchangeInstance {
        self.market.exchange_instance(self.environment)
    }

    fn instrument(&self) -> InstrumentId {
        self.market.instrument_id()
    }

    /// Compute order qty: if `order_notional_quote` is set, derive qty from price;
    /// otherwise use fixed `order_size`.
    fn compute_order_qty(&self, price: Price) -> Qty {
        if let Some(notional) = self.config.order_notional_quote {
            if price.0 > rust_decimal::Decimal::ZERO {
                return self.round_qty(Qty(notional / price.0));
            }
        }
        self.round_qty(Qty(self.config.order_size))
    }

    fn is_long(&self) -> bool {
        self.config.side.to_lowercase() != "short"
    }

    /// The side for opening a position.
    fn open_side(&self) -> OrderSide {
        if self.is_long() {
            OrderSide::Buy
        } else {
            OrderSide::Sell
        }
    }

    /// The side for closing a position (opposite of open).
    fn close_side(&self) -> OrderSide {
        match self.open_side() {
            OrderSide::Buy => OrderSide::Sell,
            OrderSide::Sell => OrderSide::Buy,
        }
    }

    /// Whether RSI signals an entry (oversold for long, overbought for short).
    fn should_enter(&self, rsi: f64) -> bool {
        if self.is_long() {
            rsi < self.config.oversold
        } else {
            rsi > self.config.overbought
        }
    }

    /// Whether RSI signals an exit (overbought for long, oversold for short).
    fn should_exit(&self, rsi: f64) -> bool {
        if self.is_long() {
            rsi > self.config.overbought
        } else {
            rsi < self.config.oversold
        }
    }

    /// Place an order at market-crossing price for immediate fill.
    fn place_market_crossing_order(
        &mut self,
        ctx: &mut dyn StrategyContext,
        side: OrderSide,
        bid: Price,
        ask: Price,
    ) {
        let price = match side {
            OrderSide::Buy => self.round_price(ask),
            OrderSide::Sell => self.round_price(bid),
        };
        let qty = self.compute_order_qty(price);

        let order = PlaceOrder::limit(self.exchange(), self.instrument(), side, price, qty);

        ctx.log_info(&format!(
            "RSI signal → {} {} @ {} (RSI: {:.1})",
            side,
            qty,
            price,
            self.state.last_rsi.unwrap_or(0.0)
        ));

        self.state.active_order = Some(order.client_id.clone());
        ctx.place_order(order);
    }
}

impl Strategy for RsiStrategy {
    fn id(&self) -> &StrategyId {
        &self.config.strategy_id
    }

    fn on_start(&mut self, ctx: &mut dyn StrategyContext) {
        self.instrument_meta = ctx.instrument_meta(&self.instrument()).cloned();

        let errors = self.config.validate();
        if !errors.is_empty() {
            ctx.log_error(&format!("RSI config errors: {:?}", errors));
            ctx.stop_strategy(self.config.strategy_id.clone(), "Invalid config");
            return;
        }

        ctx.log_info(&format!(
            "RSI Strategy started: {} on {} — RSI({}) oversold={} overbought={} bar={}s size={} side={}",
            self.market.base(),
            self.environment,
            self.config.rsi_period,
            self.config.oversold,
            self.config.overbought,
            self.config.bar_interval_secs,
            self.config.order_size,
            self.config.side,
        ));
    }

    fn on_event(&mut self, ctx: &mut dyn StrategyContext, event: &Event) {
        match event {
            Event::Quote(q) => {
                self.state.tick_count += 1;

                // Calculate mid price from bid/ask
                let mid = ((q.bid.0 + q.ask.0) / rust_decimal::Decimal::TWO)
                    .to_f64()
                    .unwrap_or(0.0);

                // Feed mid price to bar builder
                if let Some(bar) = self.bar_builder.update(mid, q.ts) {
                    self.state.bars_count += 1;

                    // Feed completed bar's close to RSI indicator
                    if let Some(rsi_value) = self.rsi.update(bar.close) {
                        self.state.last_rsi = Some(rsi_value);

                        // Log RSI periodically (every 5 bars to avoid spam)
                        if self.state.bars_count % 5 == 0 {
                            ctx.log_info(&format!(
                                "RSI({}) = {:.1} | bar #{} | O={:.2} H={:.2} L={:.2} C={:.2}",
                                self.config.rsi_period,
                                rsi_value,
                                self.state.bars_count,
                                bar.open,
                                bar.high,
                                bar.low,
                                bar.close,
                            ));
                        }

                        // Transition from WarmingUp to Watching once RSI converges
                        if self.state.phase == Phase::WarmingUp {
                            ctx.log_info(&format!(
                                "RSI converged after {} bars — now watching for signals (RSI: {:.1})",
                                self.state.bars_count, rsi_value
                            ));
                            self.state.phase = Phase::Watching;
                        }

                        // Signal logic
                        match self.state.phase {
                            Phase::Watching => {
                                if self.should_enter(rsi_value) {
                                    ctx.log_info(&format!(
                                        "RSI {:.1} crossed {} threshold → opening {}",
                                        rsi_value,
                                        if self.is_long() {
                                            "oversold"
                                        } else {
                                            "overbought"
                                        },
                                        self.open_side()
                                    ));
                                    self.place_market_crossing_order(
                                        ctx,
                                        self.open_side(),
                                        q.bid,
                                        q.ask,
                                    );
                                    self.state.phase = Phase::Opening;
                                }
                            }
                            Phase::InPosition => {
                                if self.should_exit(rsi_value) {
                                    ctx.log_info(&format!(
                                        "RSI {:.1} crossed {} threshold → closing {}",
                                        rsi_value,
                                        if self.is_long() {
                                            "overbought"
                                        } else {
                                            "oversold"
                                        },
                                        self.close_side()
                                    ));
                                    self.place_market_crossing_order(
                                        ctx,
                                        self.close_side(),
                                        q.bid,
                                        q.ask,
                                    );
                                    self.state.phase = Phase::Closing;
                                }
                            }
                            _ => {}
                        }
                    } else if self.state.phase == Phase::WarmingUp {
                        ctx.log_debug(&format!(
                            "Warming up: bar #{} — need {} bars for RSI convergence",
                            self.state.bars_count,
                            self.config.rsi_period + 1
                        ));
                    }
                }
            }

            Event::OrderFilled(f) => {
                ctx.log_info(&format!(
                    "Order filled: {} {} @ {} (fee: {:?})",
                    f.side, f.qty, f.price, f.fee
                ));

                match self.state.phase {
                    Phase::Opening => {
                        ctx.log_info(&format!(
                            "Position opened! Watching for exit signal (RSI > {} for long, RSI < {} for short)",
                            self.config.overbought, self.config.oversold
                        ));
                        self.state.phase = Phase::InPosition;
                        self.state.active_order = None;
                    }
                    Phase::Closing => {
                        ctx.log_info("Position closed! Back to watching for entry signal.");
                        self.state.phase = Phase::Watching;
                        self.state.active_order = None;
                    }
                    _ => {}
                }
            }

            Event::OrderRejected(r) => {
                ctx.log_error(&format!("Order rejected: {}", r.reason));
                self.state.active_order = None;
                // Fall back to previous watching state
                match self.state.phase {
                    Phase::Opening => self.state.phase = Phase::Watching,
                    Phase::Closing => self.state.phase = Phase::InPosition,
                    _ => {}
                }
            }

            Event::OrderCanceled(c) => {
                ctx.log_warn(&format!("Order canceled: {:?}", c.reason));
                self.state.active_order = None;
                match self.state.phase {
                    Phase::Opening => self.state.phase = Phase::Watching,
                    Phase::Closing => self.state.phase = Phase::InPosition,
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
        ctx.cancel_all(CancelAll::new(self.exchange()));
        ctx.log_info(&format!(
            "RSI Strategy stopped. Bars: {}, Ticks: {}, Last RSI: {:?}, Phase: {:?}",
            self.state.bars_count, self.state.tick_count, self.state.last_rsi, self.state.phase
        ));
    }
}
