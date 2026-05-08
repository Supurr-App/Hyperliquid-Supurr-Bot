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
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Deterministic condition evaluated by the orchestrator from local engine state.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OrchestratorCondition {
    PriceAbove {
        #[serde(default)]
        instrument: Option<String>,
        price: Decimal,
    },
    PriceBelow {
        #[serde(default)]
        instrument: Option<String>,
        price: Decimal,
    },
    SpreadBelow {
        #[serde(default)]
        instrument: Option<String>,
        #[serde(default)]
        max_bps: Option<Decimal>,
        #[serde(default)]
        max_abs: Option<Decimal>,
    },
    BalanceAbove {
        asset: String,
        available: Decimal,
    },
    GroupPnlPctAbove {
        pct: Decimal,
    },
    GroupPnlPctBelow {
        pct: Decimal,
    },
    MaxRunningTime {
        secs: u64,
    },
}

/// Condition buckets controlling child lifecycle.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct OrchestratorConditions {
    #[serde(default)]
    pub start_conditions: Vec<OrchestratorCondition>,
    #[serde(default)]
    pub validation_conditions: Vec<OrchestratorCondition>,
    #[serde(default)]
    pub risk_conditions: Vec<OrchestratorCondition>,
}

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
    started: bool,
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
            started: false,
            active: true,
        }
    }
}

/// In-process parent strategy that coordinates multiple child strategies.
pub struct BotOrchestrator {
    id: StrategyId,
    legs: Vec<OrchestratorLeg>,
    risk: GroupRiskConfig,
    conditions: OrchestratorConditions,
    children_started_at_ms: Option<i64>,
    control_timer_id: Option<TimerId>,
    stopped: bool,
    stop_reason: Option<String>,
}

impl BotOrchestrator {
    pub fn new(id: StrategyId, legs: Vec<OrchestratorLeg>, risk: GroupRiskConfig) -> Self {
        Self::with_conditions(id, legs, risk, OrchestratorConditions::default())
    }

    pub fn with_conditions(
        id: StrategyId,
        legs: Vec<OrchestratorLeg>,
        risk: GroupRiskConfig,
        conditions: OrchestratorConditions,
    ) -> Self {
        Self {
            id,
            legs,
            risk,
            conditions,
            children_started_at_ms: None,
            control_timer_id: None,
            stopped: false,
            stop_reason: None,
        }
    }

    pub fn legs(&self) -> &[OrchestratorLeg] {
        &self.legs
    }

    fn run_child_start(&mut self, ctx: &mut dyn StrategyContext, leg_index: usize) {
        if !self.legs[leg_index].active || self.legs[leg_index].started {
            return;
        }

        let leg_id = self.legs[leg_index].id.clone();
        let (captured_commands, captured_stop) = {
            let mut child_ctx = ChildStrategyContext::new(ctx, &leg_id);
            self.legs[leg_index].strategy.on_start(&mut child_ctx);
            (child_ctx.take_commands(), child_ctx.stop_reason.take())
        };
        self.legs[leg_index].started = true;
        self.flush_child_context(ctx, leg_index, captured_commands, captured_stop);
    }

    fn run_child_event(&mut self, ctx: &mut dyn StrategyContext, leg_index: usize, event: &Event) {
        if !self.legs[leg_index].active || !self.legs[leg_index].started {
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
        if !self.legs[leg_index].started {
            self.legs[leg_index].active = false;
            return;
        }

        let leg_id = self.legs[leg_index].id.clone();
        let (captured_commands, captured_stop) = {
            let mut child_ctx = ChildStrategyContext::new(ctx, &leg_id);
            self.legs[leg_index].strategy.on_stop(&mut child_ctx);
            (child_ctx.take_commands(), child_ctx.stop_reason.take())
        };
        self.legs[leg_index].active = false;
        self.legs[leg_index].started = false;
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
                .filter_map(|(idx, leg)| (leg.active && leg.started).then_some(idx))
                .collect();
        };

        self.legs
            .iter()
            .enumerate()
            .filter_map(|(idx, leg)| {
                (leg.active && leg.started && &leg.instrument == instrument).then_some(idx)
            })
            .collect()
    }

    fn current_group_pnl(&self, ctx: &dyn StrategyContext) -> Decimal {
        self.legs
            .iter()
            .map(|leg| ctx.position(&leg.instrument).current_pnl())
            .sum()
    }

    fn group_pnl_pct(&self, ctx: &dyn StrategyContext) -> Option<Decimal> {
        if self.risk.allocated_capital_quote <= Decimal::ZERO {
            return None;
        }
        Some(
            (self.current_group_pnl(ctx) / self.risk.allocated_capital_quote)
                * Decimal::new(100, 0),
        )
    }

    fn condition_instrument(
        &self,
        requested: &Option<String>,
        default: Option<&InstrumentId>,
    ) -> Option<InstrumentId> {
        requested
            .as_ref()
            .map(|value| InstrumentId::new(value.clone()))
            .or_else(|| default.cloned())
    }

    fn evaluate_condition(
        &self,
        ctx: &dyn StrategyContext,
        condition: &OrchestratorCondition,
    ) -> Option<bool> {
        let default_instrument = self.legs.first().map(|leg| &leg.instrument);

        match condition {
            OrchestratorCondition::PriceAbove { instrument, price } => {
                let instrument = self.condition_instrument(instrument, default_instrument)?;
                let mid = ctx.mid_price(&instrument)?;
                Some(mid.0 > *price)
            }
            OrchestratorCondition::PriceBelow { instrument, price } => {
                let instrument = self.condition_instrument(instrument, default_instrument)?;
                let mid = ctx.mid_price(&instrument)?;
                Some(mid.0 < *price)
            }
            OrchestratorCondition::SpreadBelow {
                instrument,
                max_bps,
                max_abs,
            } => {
                let instrument = self.condition_instrument(instrument, default_instrument)?;
                let quote = ctx.quote(&instrument)?;
                let bps_ok = max_bps.map(|limit| quote.spread_bps() < limit);
                let abs_ok = max_abs.map(|limit| quote.spread() < limit);
                bps_ok
                    .or(abs_ok)
                    .map(|first| first && abs_ok.unwrap_or(true) && bps_ok.unwrap_or(true))
            }
            OrchestratorCondition::BalanceAbove { asset, available } => {
                let balance = ctx.balance(&AssetId::new(asset.clone()));
                Some(balance.available > *available)
            }
            OrchestratorCondition::GroupPnlPctAbove { pct } => {
                Some(self.group_pnl_pct(ctx)? > *pct)
            }
            OrchestratorCondition::GroupPnlPctBelow { pct } => {
                Some(self.group_pnl_pct(ctx)? < *pct)
            }
            OrchestratorCondition::MaxRunningTime { secs } => {
                let started_at = self.children_started_at_ms?;
                let elapsed_ms = ctx.now_ms().saturating_sub(started_at);
                Some(elapsed_ms >= (*secs as i64) * 1000)
            }
        }
    }

    fn start_conditions_passed(&self, ctx: &dyn StrategyContext) -> bool {
        self.conditions
            .start_conditions
            .iter()
            .all(|condition| self.evaluate_condition(ctx, condition) == Some(true))
    }

    fn first_failed_validation(&self, ctx: &dyn StrategyContext) -> Option<String> {
        self.conditions
            .validation_conditions
            .iter()
            .find_map(|condition| {
                (self.evaluate_condition(ctx, condition) == Some(false))
                    .then(|| format!("{:?}", condition))
            })
    }

    fn first_triggered_risk(&self, ctx: &dyn StrategyContext) -> Option<String> {
        self.conditions
            .risk_conditions
            .iter()
            .find_map(|condition| {
                (self.evaluate_condition(ctx, condition) == Some(true))
                    .then(|| format!("{:?}", condition))
            })
    }

    fn children_started(&self) -> bool {
        self.legs.iter().any(|leg| leg.started)
    }

    fn start_children_if_ready(&mut self, ctx: &mut dyn StrategyContext) {
        if self.children_started() || !self.start_conditions_passed(ctx) {
            return;
        }

        ctx.log_info("Orchestrator start conditions passed; starting children");
        self.children_started_at_ms = Some(ctx.now_ms());
        for leg_index in 0..self.legs.len() {
            self.run_child_start(ctx, leg_index);
            if self.stopped {
                return;
            }
        }
    }

    fn evaluate_validation(&mut self, ctx: &mut dyn StrategyContext) {
        if let Some(reason) = self.first_failed_validation(ctx) {
            self.stop_group(ctx, &format!("validation condition failed: {}", reason));
        }
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

    fn evaluate_risk_conditions(&mut self, ctx: &mut dyn StrategyContext) {
        if !self.children_started() {
            return;
        }

        if let Some(reason) = self.first_triggered_risk(ctx) {
            self.stop_group(ctx, &format!("risk condition triggered: {}", reason));
            return;
        }

        self.evaluate_group_risk(ctx);
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

        self.control_timer_id = Some(ctx.set_interval(Duration::from_secs(1)));
        self.evaluate_validation(ctx);
        self.start_children_if_ready(ctx);
    }

    fn on_event(&mut self, ctx: &mut dyn StrategyContext, event: &Event) {
        if self.stopped {
            return;
        }

        self.evaluate_validation(ctx);
        if self.stopped {
            return;
        }

        self.start_children_if_ready(ctx);
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

        self.evaluate_risk_conditions(ctx);
    }

    fn on_timer(&mut self, ctx: &mut dyn StrategyContext, timer_id: TimerId) {
        if self.stopped {
            return;
        }

        let is_control_timer = self.control_timer_id == Some(timer_id);

        self.evaluate_validation(ctx);
        if self.stopped {
            return;
        }

        self.start_children_if_ready(ctx);
        if self.stopped {
            return;
        }

        if is_control_timer {
            self.evaluate_risk_conditions(ctx);
            return;
        }

        for leg_index in 0..self.legs.len() {
            if !self.legs[leg_index].active || !self.legs[leg_index].started {
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

        self.evaluate_risk_conditions(ctx);
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

#[cfg(test)]
mod tests {
    use super::*;
    use bot_core::Qty;
    use std::collections::HashMap;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[derive(Default)]
    struct Counts {
        starts: AtomicUsize,
        stops: AtomicUsize,
    }

    struct CountingStrategy {
        id: StrategyId,
        counts: Arc<Counts>,
    }

    impl CountingStrategy {
        fn new(id: &str, counts: Arc<Counts>) -> Self {
            Self {
                id: StrategyId::new(id),
                counts,
            }
        }
    }

    impl Strategy for CountingStrategy {
        fn id(&self) -> &StrategyId {
            &self.id
        }

        fn on_start(&mut self, _ctx: &mut dyn StrategyContext) {
            self.counts.starts.fetch_add(1, Ordering::SeqCst);
        }

        fn on_event(&mut self, _ctx: &mut dyn StrategyContext, _event: &Event) {}

        fn on_timer(&mut self, _ctx: &mut dyn StrategyContext, _timer_id: TimerId) {}

        fn on_stop(&mut self, _ctx: &mut dyn StrategyContext) {
            self.counts.stops.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct TestContext {
        now_ms: i64,
        quotes: HashMap<InstrumentId, Quote>,
        balances: HashMap<AssetId, Balance>,
        positions: HashMap<InstrumentId, Position>,
        stops: Vec<String>,
        intervals: usize,
    }

    impl Default for TestContext {
        fn default() -> Self {
            Self {
                now_ms: 0,
                quotes: HashMap::new(),
                balances: HashMap::new(),
                positions: HashMap::new(),
                stops: Vec::new(),
                intervals: 0,
            }
        }
    }

    impl TestContext {
        fn set_quote(&mut self, instrument: &str, bid: Decimal, ask: Decimal) {
            let instrument = InstrumentId::new(instrument);
            self.quotes.insert(
                instrument.clone(),
                Quote {
                    instrument,
                    bid: Price::new(bid),
                    ask: Price::new(ask),
                    bid_size: Qty::new(Decimal::ONE),
                    ask_size: Qty::new(Decimal::ONE),
                    ts: self.now_ms,
                },
            );
        }
    }

    impl StrategyContext for TestContext {
        fn place_order(&mut self, _cmd: PlaceOrder) {}

        fn place_orders(&mut self, _cmds: Vec<PlaceOrder>) {}

        fn cancel_order(&mut self, _cmd: CancelOrder) {}

        fn cancel_all(&mut self, _cmd: CancelAll) {}

        fn stop_strategy(&mut self, _strategy_id: StrategyId, reason: &str) {
            self.stops.push(reason.to_string());
        }

        fn set_timer(&mut self, _delay: Duration) -> TimerId {
            TimerId::new(1)
        }

        fn set_interval(&mut self, _interval: Duration) -> TimerId {
            self.intervals += 1;
            TimerId::new(self.intervals as u64)
        }

        fn cancel_timer(&mut self, _timer_id: TimerId) {}

        fn mid_price(&self, instrument: &InstrumentId) -> Option<Price> {
            self.quotes.get(instrument).map(Quote::mid)
        }

        fn quote(&self, instrument: &InstrumentId) -> Option<Quote> {
            self.quotes.get(instrument).cloned()
        }

        fn instrument_meta(&self, _instrument: &InstrumentId) -> Option<&InstrumentMeta> {
            None
        }

        fn balance(&self, asset: &AssetId) -> Balance {
            self.balances.get(asset).copied().unwrap_or_default()
        }

        fn position(&self, instrument: &InstrumentId) -> Position {
            self.positions.get(instrument).cloned().unwrap_or_default()
        }

        fn exchange_health(&self, _exchange: &ExchangeInstance) -> ExchangeHealth {
            ExchangeHealth::Active
        }

        fn order(&self, _client_id: &ClientOrderId) -> Option<&LiveOrder> {
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

    fn one_leg(counts: Arc<Counts>) -> OrchestratorLeg {
        OrchestratorLeg::new(
            "leg",
            InstrumentId::new("BTC-PERP"),
            Box::new(CountingStrategy::new("child", counts)),
        )
    }

    fn orchestrator(counts: Arc<Counts>, conditions: OrchestratorConditions) -> BotOrchestrator {
        BotOrchestrator::with_conditions(
            StrategyId::new("parent"),
            vec![one_leg(counts)],
            GroupRiskConfig::disabled(Decimal::new(100, 0)),
            conditions,
        )
    }

    #[test]
    fn empty_start_conditions_start_children_immediately() {
        let counts = Arc::new(Counts::default());
        let mut orchestrator = orchestrator(counts.clone(), OrchestratorConditions::default());
        let mut ctx = TestContext::default();

        orchestrator.on_start(&mut ctx);

        assert_eq!(counts.starts.load(Ordering::SeqCst), 1);
        assert_eq!(ctx.stops.len(), 0);
    }

    #[test]
    fn start_condition_waits_until_price_passes() {
        let counts = Arc::new(Counts::default());
        let mut orchestrator = orchestrator(
            counts.clone(),
            OrchestratorConditions {
                start_conditions: vec![OrchestratorCondition::PriceAbove {
                    instrument: Some("BTC-PERP".to_string()),
                    price: Decimal::new(100, 0),
                }],
                ..Default::default()
            },
        );
        let mut ctx = TestContext::default();

        orchestrator.on_start(&mut ctx);
        assert_eq!(counts.starts.load(Ordering::SeqCst), 0);

        ctx.set_quote("BTC-PERP", Decimal::new(101, 0), Decimal::new(103, 0));
        orchestrator.on_timer(&mut ctx, TimerId::new(1));

        assert_eq!(counts.starts.load(Ordering::SeqCst), 1);
        assert_eq!(ctx.stops.len(), 0);
    }

    #[test]
    fn failed_validation_condition_stops_without_starting_child() {
        let counts = Arc::new(Counts::default());
        let mut orchestrator = orchestrator(
            counts.clone(),
            OrchestratorConditions {
                validation_conditions: vec![OrchestratorCondition::BalanceAbove {
                    asset: "USDC".to_string(),
                    available: Decimal::new(50, 0),
                }],
                ..Default::default()
            },
        );
        let mut ctx = TestContext::default();
        ctx.balances.insert(
            AssetId::new("USDC"),
            Balance::new(Decimal::new(10, 0), Decimal::new(10, 0), Decimal::ZERO),
        );

        orchestrator.on_start(&mut ctx);

        assert_eq!(counts.starts.load(Ordering::SeqCst), 0);
        assert_eq!(counts.stops.load(Ordering::SeqCst), 0);
        assert!(ctx.stops[0].contains("validation condition failed"));
    }

    #[test]
    fn max_running_time_risk_condition_stops_started_child() {
        let counts = Arc::new(Counts::default());
        let mut orchestrator = orchestrator(
            counts.clone(),
            OrchestratorConditions {
                risk_conditions: vec![OrchestratorCondition::MaxRunningTime { secs: 10 }],
                ..Default::default()
            },
        );
        let mut ctx = TestContext::default();

        orchestrator.on_start(&mut ctx);
        assert_eq!(counts.starts.load(Ordering::SeqCst), 1);

        ctx.now_ms = 10_000;
        orchestrator.on_timer(&mut ctx, TimerId::new(1));

        assert_eq!(counts.stops.load(Ordering::SeqCst), 1);
        assert!(ctx.stops[0].contains("risk condition triggered"));
    }

    #[test]
    fn spread_condition_supports_absolute_or_bps_thresholds() {
        let counts = Arc::new(Counts::default());
        let mut orchestrator = orchestrator(
            counts.clone(),
            OrchestratorConditions {
                start_conditions: vec![OrchestratorCondition::SpreadBelow {
                    instrument: Some("BTC-PERP".to_string()),
                    max_bps: Some(Decimal::new(100, 0)),
                    max_abs: Some(Decimal::new(5, 0)),
                }],
                ..Default::default()
            },
        );
        let mut ctx = TestContext::default();
        ctx.set_quote("BTC-PERP", Decimal::new(100, 0), Decimal::new(101, 0));

        orchestrator.on_start(&mut ctx);

        assert_eq!(counts.starts.load(Ordering::SeqCst), 1);
    }
}
