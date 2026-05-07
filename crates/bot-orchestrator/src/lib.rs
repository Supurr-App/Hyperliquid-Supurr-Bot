//! In-process strategy orchestration.
//!
//! The orchestrator is itself a strategy from the engine's point of view.
//! Child strategies are plain strategy instances owned and driven inside it.

use bot_core::{
    AssetId, Balance, CancelAll, CancelOrder, ClientOrderId, Event, ExchangeHealth,
    ExchangeInstance, InstrumentId, InstrumentMeta, LiveOrder, PlaceOrder, Position, Price, Quote,
    Strategy, StrategyContext, StrategyId, SyncMechanism, TimerId,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Group-level risk settings for the orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupRiskConfig {
    /// Total group allocation in quote currency.
    pub allocated_capital_quote: Decimal,
    /// Take-profit threshold in percentage units. Example: `10` means +10%.
    pub take_profit_pct: Option<Decimal>,
    /// Stop-loss threshold in percentage units. Example: `5` means -5%.
    pub stop_loss_pct: Option<Decimal>,
}

impl GroupRiskConfig {
    pub fn disabled(allocated_capital_quote: Decimal) -> Self {
        Self {
            allocated_capital_quote,
            take_profit_pct: None,
            stop_loss_pct: None,
        }
    }
}

/// One child strategy leg owned by the parent orchestrator.
pub struct OrchestratorLeg {
    pub id: String,
    pub instrument: InstrumentId,
    pub strategy: Box<dyn Strategy>,
    active: bool,
}

impl OrchestratorLeg {
    pub fn new(
        id: impl Into<String>,
        instrument: InstrumentId,
        strategy: Box<dyn Strategy>,
    ) -> Self {
        Self {
            id: id.into(),
            instrument,
            strategy,
            active: true,
        }
    }
}

/// In-process parent strategy that coordinates multiple child strategies.
pub struct BotOrchestrator {
    id: StrategyId,
    legs: Vec<OrchestratorLeg>,
    risk: GroupRiskConfig,
    stopped: bool,
    stop_reason: Option<String>,
}

impl BotOrchestrator {
    pub fn new(id: StrategyId, legs: Vec<OrchestratorLeg>, risk: GroupRiskConfig) -> Self {
        Self {
            id,
            legs,
            risk,
            stopped: false,
            stop_reason: None,
        }
    }

    pub fn legs(&self) -> &[OrchestratorLeg] {
        &self.legs
    }

    fn run_child_start(&mut self, ctx: &mut dyn StrategyContext, leg_index: usize) {
        let leg_id = self.legs[leg_index].id.clone();
        let (captured_commands, captured_stop) = {
            let mut child_ctx = ChildStrategyContext::new(ctx, &leg_id);
            self.legs[leg_index].strategy.on_start(&mut child_ctx);
            (child_ctx.take_commands(), child_ctx.stop_reason.take())
        };
        self.flush_child_context(ctx, leg_index, captured_commands, captured_stop);
    }

    fn run_child_event(&mut self, ctx: &mut dyn StrategyContext, leg_index: usize, event: &Event) {
        if !self.legs[leg_index].active {
            return;
        }

        let leg_id = self.legs[leg_index].id.clone();
        let (captured_commands, captured_stop) = {
            let mut child_ctx = ChildStrategyContext::new(ctx, &leg_id);
            self.legs[leg_index]
                .strategy
                .on_event(&mut child_ctx, event);
            (child_ctx.take_commands(), child_ctx.stop_reason.take())
        };
        self.flush_child_context(ctx, leg_index, captured_commands, captured_stop);
    }

    fn run_child_stop(&mut self, ctx: &mut dyn StrategyContext, leg_index: usize) {
        if !self.legs[leg_index].active {
            return;
        }

        let leg_id = self.legs[leg_index].id.clone();
        let (captured_commands, captured_stop) = {
            let mut child_ctx = ChildStrategyContext::new(ctx, &leg_id);
            self.legs[leg_index].strategy.on_stop(&mut child_ctx);
            (child_ctx.take_commands(), child_ctx.stop_reason.take())
        };
        self.legs[leg_index].active = false;
        self.flush_child_context(ctx, leg_index, captured_commands, captured_stop);
    }

    fn flush_child_context(
        &mut self,
        ctx: &mut dyn StrategyContext,
        leg_index: usize,
        captured_commands: CapturedCommands,
        captured_stop: Option<String>,
    ) {
        for cmd in captured_commands.place_orders {
            ctx.place_order(cmd);
        }
        for batch in captured_commands.batch_orders {
            ctx.place_orders(batch);
        }
        for cmd in captured_commands.cancel_orders {
            ctx.cancel_order(cmd);
        }
        for cmd in captured_commands.cancel_alls {
            ctx.cancel_all(cmd);
        }

        if let Some(reason) = captured_stop {
            let leg_id = self.legs[leg_index].id.clone();
            self.stop_group(ctx, &format!("child leg {} stopped: {}", leg_id, reason));
        }
    }

    fn routed_leg_indexes(&self, event: &Event) -> Vec<usize> {
        let Some(instrument) = event.instrument() else {
            return self
                .legs
                .iter()
                .enumerate()
                .filter_map(|(idx, leg)| leg.active.then_some(idx))
                .collect();
        };

        self.legs
            .iter()
            .enumerate()
            .filter_map(|(idx, leg)| (leg.active && &leg.instrument == instrument).then_some(idx))
            .collect()
    }

    fn current_group_pnl(&self, ctx: &dyn StrategyContext) -> Decimal {
        self.legs
            .iter()
            .map(|leg| ctx.position(&leg.instrument).current_pnl())
            .sum()
    }

    fn evaluate_group_risk(&mut self, ctx: &mut dyn StrategyContext) {
        if self.stopped || self.risk.allocated_capital_quote <= Decimal::ZERO {
            return;
        }

        let group_pnl = self.current_group_pnl(ctx);
        let pnl_pct = (group_pnl / self.risk.allocated_capital_quote) * Decimal::new(100, 0);

        if let Some(tp) = self.risk.take_profit_pct {
            let threshold = tp.abs();
            if threshold > Decimal::ZERO && pnl_pct >= threshold {
                self.stop_group(
                    ctx,
                    &format!(
                        "group take profit triggered: pnl_pct={} >= threshold={}",
                        pnl_pct, threshold
                    ),
                );
                return;
            }
        }

        if let Some(sl) = self.risk.stop_loss_pct {
            let threshold = -sl.abs();
            if sl != Decimal::ZERO && pnl_pct <= threshold {
                self.stop_group(
                    ctx,
                    &format!(
                        "group stop loss triggered: pnl_pct={} <= threshold={}",
                        pnl_pct, threshold
                    ),
                );
            }
        }
    }

    fn stop_group(&mut self, ctx: &mut dyn StrategyContext, reason: &str) {
        if self.stopped {
            return;
        }

        self.stopped = true;
        self.stop_reason = Some(reason.to_string());
        ctx.log_warn(&format!("Orchestrator stopping group: {}", reason));

        for leg_index in 0..self.legs.len() {
            self.run_child_stop(ctx, leg_index);
        }

        ctx.stop_strategy(self.id.clone(), reason);
    }
}

impl Strategy for BotOrchestrator {
    fn id(&self) -> &StrategyId {
        &self.id
    }

    fn sync_mechanism(&self) -> SyncMechanism {
        self.legs
            .first()
            .map(|leg| leg.strategy.sync_mechanism())
            .unwrap_or_default()
    }

    fn on_start(&mut self, ctx: &mut dyn StrategyContext) {
        ctx.log_info(&format!(
            "BotOrchestrator started: legs={} allocation={}",
            self.legs.len(),
            self.risk.allocated_capital_quote
        ));

        for leg_index in 0..self.legs.len() {
            self.run_child_start(ctx, leg_index);
            if self.stopped {
                return;
            }
        }
    }

    fn on_event(&mut self, ctx: &mut dyn StrategyContext, event: &Event) {
        if self.stopped {
            return;
        }

        let leg_indexes = self.routed_leg_indexes(event);
        for leg_index in leg_indexes {
            self.run_child_event(ctx, leg_index, event);
            if self.stopped {
                return;
            }
        }

        self.evaluate_group_risk(ctx);
    }

    fn on_timer(&mut self, ctx: &mut dyn StrategyContext, timer_id: TimerId) {
        if self.stopped {
            return;
        }

        for leg_index in 0..self.legs.len() {
            if !self.legs[leg_index].active {
                continue;
            }
            let leg_id = self.legs[leg_index].id.clone();
            let (captured_commands, captured_stop) = {
                let mut child_ctx = ChildStrategyContext::new(ctx, &leg_id);
                self.legs[leg_index]
                    .strategy
                    .on_timer(&mut child_ctx, timer_id);
                (child_ctx.take_commands(), child_ctx.stop_reason.take())
            };
            self.flush_child_context(ctx, leg_index, captured_commands, captured_stop);
            if self.stopped {
                return;
            }
        }
    }

    fn on_stop(&mut self, ctx: &mut dyn StrategyContext) {
        if self.stopped {
            return;
        }

        self.stopped = true;
        self.stop_reason = Some("parent stop requested".to_string());
        ctx.log_info("BotOrchestrator stopping children");

        for leg_index in 0..self.legs.len() {
            self.run_child_stop(ctx, leg_index);
        }
    }
}

#[derive(Default)]
struct CapturedCommands {
    place_orders: Vec<PlaceOrder>,
    batch_orders: Vec<Vec<PlaceOrder>>,
    cancel_orders: Vec<CancelOrder>,
    cancel_alls: Vec<CancelAll>,
}

struct ChildStrategyContext<'a> {
    parent: &'a dyn StrategyContext,
    leg_id: &'a str,
    commands: CapturedCommands,
    stop_reason: Option<String>,
}

impl<'a> ChildStrategyContext<'a> {
    fn new(parent: &'a dyn StrategyContext, leg_id: &'a str) -> Self {
        Self {
            parent,
            leg_id,
            commands: CapturedCommands::default(),
            stop_reason: None,
        }
    }

    fn take_commands(&mut self) -> CapturedCommands {
        std::mem::take(&mut self.commands)
    }

    fn prefix(&self, msg: &str) -> String {
        format!("[{}] {}", self.leg_id, msg)
    }
}

impl StrategyContext for ChildStrategyContext<'_> {
    fn place_order(&mut self, cmd: PlaceOrder) {
        self.commands.place_orders.push(cmd);
    }

    fn place_orders(&mut self, cmds: Vec<PlaceOrder>) {
        if !cmds.is_empty() {
            self.commands.batch_orders.push(cmds);
        }
    }

    fn cancel_order(&mut self, cmd: CancelOrder) {
        self.commands.cancel_orders.push(cmd);
    }

    fn cancel_all(&mut self, cmd: CancelAll) {
        self.commands.cancel_alls.push(cmd);
    }

    fn stop_strategy(&mut self, strategy_id: StrategyId, reason: &str) {
        self.parent.log_warn(&self.prefix(&format!(
            "child strategy {} requested stop: {}",
            strategy_id, reason
        )));
        self.stop_reason = Some(reason.to_string());
    }

    fn set_timer(&mut self, delay: Duration) -> TimerId {
        self.parent.log_debug(&self.prefix(&format!(
            "child timer requested for {:?}; timer is parent-scoped",
            delay
        )));
        TimerId::new(0)
    }

    fn set_interval(&mut self, interval: Duration) -> TimerId {
        self.parent.log_debug(&self.prefix(&format!(
            "child interval requested for {:?}; timer is parent-scoped",
            interval
        )));
        TimerId::new(0)
    }

    fn cancel_timer(&mut self, _timer_id: TimerId) {}

    fn mid_price(&self, instrument: &InstrumentId) -> Option<Price> {
        self.parent.mid_price(instrument)
    }

    fn quote(&self, instrument: &InstrumentId) -> Option<Quote> {
        self.parent.quote(instrument)
    }

    fn instrument_meta(&self, instrument: &InstrumentId) -> Option<&InstrumentMeta> {
        self.parent.instrument_meta(instrument)
    }

    fn balance(&self, asset: &AssetId) -> Balance {
        self.parent.balance(asset)
    }

    fn position(&self, instrument: &InstrumentId) -> Position {
        self.parent.position(instrument)
    }

    fn exchange_health(&self, exchange: &ExchangeInstance) -> ExchangeHealth {
        self.parent.exchange_health(exchange)
    }

    fn order(&self, client_id: &ClientOrderId) -> Option<&LiveOrder> {
        self.parent.order(client_id)
    }

    fn now_ms(&self) -> i64 {
        self.parent.now_ms()
    }

    fn log_info(&self, msg: &str) {
        self.parent.log_info(&self.prefix(msg));
    }

    fn log_warn(&self, msg: &str) {
        self.parent.log_warn(&self.prefix(msg));
    }

    fn log_error(&self, msg: &str) {
        self.parent.log_error(&self.prefix(msg));
    }

    fn log_debug(&self, msg: &str) {
        self.parent.log_debug(&self.prefix(msg));
    }
}
