//! Integration tests for ArbitrageStrategy using MockContext.
//!
//! Tests the strategy's behavior when spreads cross thresholds.

use bot_core::{
    AssetId, Balance, CancelAll, CancelOrder, ClientOrderId, Environment, Event, ExchangeHealth,
    ExchangeId, ExchangeInstance, HyperliquidMarket, InstrumentId, InstrumentMeta, LiveOrder,
    Market, OrderCanceledEvent, OrderCompletedEvent, OrderSide, PlaceOrder, Position, Price, Qty,
    Quote, QuoteEvent, Strategy, StrategyContext, StrategyId, TimerId,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::time::Duration;
use strategy_arbitrage::{ArbitrageConfig, ArbitrageStrategy};

// ============================================================================
// Test Harness: MockContext
// ============================================================================

/// Mock StrategyContext that captures commands for verification.
struct MockContext {
    time_ms: i64,
    orders: Vec<PlaceOrder>,
    quotes: HashMap<InstrumentId, Quote>,
    exchange_health: HashMap<ExchangeInstance, ExchangeHealth>,
    next_timer_id: u64,
    stopped: bool,
    stop_reason: Option<String>,
}

impl MockContext {
    fn new() -> Self {
        Self {
            time_ms: 1000000,
            orders: Vec::new(),
            quotes: HashMap::new(),
            exchange_health: HashMap::new(),
            next_timer_id: 1,
            stopped: false,
            stop_reason: None,
        }
    }

    fn set_quote(&mut self, instrument: InstrumentId, bid: Decimal, ask: Decimal) {
        self.quotes.insert(
            instrument.clone(),
            Quote {
                instrument,
                bid: Price::new(bid),
                ask: Price::new(ask),
                bid_size: Qty::new(dec!(100)),
                ask_size: Qty::new(dec!(100)),
                ts: self.time_ms,
            },
        );
    }

    fn set_exchange_health(&mut self, exchange: ExchangeInstance, health: ExchangeHealth) {
        self.exchange_health.insert(exchange, health);
    }

    fn advance_time(&mut self, ms: i64) {
        self.time_ms += ms;
    }

    fn placed_orders(&self) -> &[PlaceOrder] {
        &self.orders
    }

    fn clear_orders(&mut self) {
        self.orders.clear();
    }
}

impl StrategyContext for MockContext {
    fn place_order(&mut self, cmd: PlaceOrder) {
        self.orders.push(cmd);
    }

    fn place_orders(&mut self, cmds: Vec<PlaceOrder>) {
        self.orders.extend(cmds);
    }

    fn cancel_order(&mut self, _cmd: CancelOrder) {}

    fn cancel_all(&mut self, _cmd: CancelAll) {}

    fn stop_strategy(&mut self, _strategy_id: StrategyId, reason: &str) {
        self.stopped = true;
        self.stop_reason = Some(reason.to_string());
    }

    fn set_timer(&mut self, _delay: Duration) -> TimerId {
        let id = self.next_timer_id;
        self.next_timer_id += 1;
        TimerId::new(id)
    }

    fn set_interval(&mut self, _interval: Duration) -> TimerId {
        let id = self.next_timer_id;
        self.next_timer_id += 1;
        TimerId::new(id)
    }

    fn cancel_timer(&mut self, _timer_id: TimerId) {}

    fn mid_price(&self, instrument: &InstrumentId) -> Option<Price> {
        self.quotes.get(instrument).map(|q| q.mid())
    }

    fn quote(&self, instrument: &InstrumentId) -> Option<Quote> {
        self.quotes.get(instrument).cloned()
    }

    fn instrument_meta(&self, _instrument: &InstrumentId) -> Option<&InstrumentMeta> {
        None
    }

    fn balance(&self, _asset: &AssetId) -> Balance {
        Balance::default()
    }

    fn position(&self, _instrument: &InstrumentId) -> Position {
        Position::default()
    }

    fn exchange_health(&self, exchange: &ExchangeInstance) -> ExchangeHealth {
        self.exchange_health
            .get(exchange)
            .copied()
            .unwrap_or(ExchangeHealth::Active)
    }

    fn order(&self, _client_id: &ClientOrderId) -> Option<&LiveOrder> {
        None
    }

    fn now_ms(&self) -> i64 {
        self.time_ms
    }

    fn log_info(&self, _msg: &str) {}
    fn log_warn(&self, _msg: &str) {}
    fn log_error(&self, _msg: &str) {}
    fn log_debug(&self, _msg: &str) {}
}

// ============================================================================
// Test Fixtures
// ============================================================================

fn test_spot_market() -> Market {
    Market::Hyperliquid(HyperliquidMarket::Spot {
        base: "ETH".to_string(),
        quote: "USDC".to_string(),
        index: 10002,
        instrument_meta: None,
    })
}

fn test_perp_market() -> Market {
    Market::Hyperliquid(HyperliquidMarket::Perp {
        base: "ETH".to_string(),
        quote: "USDC".to_string(),
        index: 1,
        instrument_meta: None,
    })
}

fn test_config() -> ArbitrageConfig {
    ArbitrageConfig {
        strategy_id: StrategyId::new("test-arb"),
        spot_market: test_spot_market(),
        perp_market: test_perp_market(),
        environment: Environment::Testnet,
        order_amount: dec!(0.5),
        perp_leverage: dec!(1),
        min_opening_spread_pct: dec!(0.003),  // 0.3% to open
        min_closing_spread_pct: dec!(-0.001), // -0.1% to close
        spot_slippage_buffer_pct: dec!(0.001),
        perp_slippage_buffer_pct: dec!(0.001),
    }
}

fn make_quote_event(
    exchange: &ExchangeId,
    instrument: &InstrumentId,
    bid: Decimal,
    ask: Decimal,
    ts: i64,
) -> Event {
    Event::Quote(QuoteEvent {
        exchange: exchange.clone(),
        instrument: instrument.clone(),
        bid: Price::new(bid),
        ask: Price::new(ask),
        ts,
    })
}

/// Feed both spot and perp quotes to the strategy.
fn feed_quotes(
    strategy: &mut ArbitrageStrategy,
    ctx: &mut MockContext,
    spot_bid: Decimal,
    spot_ask: Decimal,
    perp_bid: Decimal,
    perp_ask: Decimal,
    spot_exchange: &ExchangeInstance,
    perp_exchange: &ExchangeInstance,
    spot_instrument: &InstrumentId,
    perp_instrument: &InstrumentId,
) {
    ctx.set_quote(spot_instrument.clone(), spot_bid, spot_ask);
    ctx.set_quote(perp_instrument.clone(), perp_bid, perp_ask);

    let event = make_quote_event(
        &spot_exchange.exchange_id,
        spot_instrument,
        spot_bid,
        spot_ask,
        ctx.time_ms,
    );
    strategy.on_event(ctx, &event);

    let event = make_quote_event(
        &perp_exchange.exchange_id,
        perp_instrument,
        perp_bid,
        perp_ask,
        ctx.time_ms,
    );
    strategy.on_event(ctx, &event);
}

/// Transition strategy from NoPosition → HasPosition by opening + simulating fills.
fn open_and_fill(
    strategy: &mut ArbitrageStrategy,
    ctx: &mut MockContext,
    spot_exchange: &ExchangeInstance,
    perp_exchange: &ExchangeInstance,
    spot_instrument: &InstrumentId,
    perp_instrument: &InstrumentId,
) {
    // Feed quotes with wide spread (0.4%) to trigger open
    feed_quotes(
        strategy,
        ctx,
        dec!(2999),
        dec!(3001), // spot mid = 3000
        dec!(3011),
        dec!(3013), // perp mid = 3012 → spread = 0.4%
        spot_exchange,
        perp_exchange,
        spot_instrument,
        perp_instrument,
    );

    assert_eq!(
        ctx.placed_orders().len(),
        2,
        "Opening should place 2 orders"
    );

    // Get order IDs
    let orders: Vec<_> = ctx.placed_orders().iter().cloned().collect();
    let spot_order_id = orders
        .iter()
        .find(|o| o.instrument == *spot_instrument)
        .map(|o| o.client_id.clone())
        .unwrap();
    let perp_order_id = orders
        .iter()
        .find(|o| o.instrument == *perp_instrument)
        .map(|o| o.client_id.clone())
        .unwrap();

    // Simulate fills
    let spot_completed = Event::OrderCompleted(OrderCompletedEvent {
        exchange: spot_exchange.exchange_id.clone(),
        instrument: spot_instrument.clone(),
        client_id: spot_order_id,
        filled_qty: Qty::new(dec!(0.5)),
        avg_fill_px: Some(Price::new(dec!(3003))),
        ts: ctx.time_ms,
    });
    strategy.on_event(ctx, &spot_completed);

    let perp_completed = Event::OrderCompleted(OrderCompletedEvent {
        exchange: perp_exchange.exchange_id.clone(),
        instrument: perp_instrument.clone(),
        client_id: perp_order_id,
        filled_qty: Qty::new(dec!(0.5)),
        avg_fill_px: Some(Price::new(dec!(3009))),
        ts: ctx.time_ms,
    });
    strategy.on_event(ctx, &perp_completed);

    ctx.clear_orders();
    ctx.advance_time(15000);

    // Reset the strategy's internal last mids to neutral values.
    // Feed perp FIRST so that when spot arrives, last_perp_mid is already fresh.
    // (If we fed spot first, it would see stale perp mid from opening and trigger false close.)
    let neutral_perp = make_quote_event(
        &perp_exchange.exchange_id,
        perp_instrument,
        dec!(9999),
        dec!(10001), // perp mid = 10000
        ctx.time_ms,
    );
    strategy.on_event(ctx, &neutral_perp);

    let neutral_spot = make_quote_event(
        &spot_exchange.exchange_id,
        spot_instrument,
        dec!(9999),
        dec!(10001), // spot mid = 10000
        ctx.time_ms,
    );
    strategy.on_event(ctx, &neutral_spot);

    ctx.clear_orders();
}

// ============================================================================
// Tests: Basic Flow
// ============================================================================

#[test]
fn test_no_position_when_spread_below_threshold() {
    let config = test_config();
    let spot_instrument = config.spot_instrument();
    let perp_instrument = config.perp_instrument();
    let spot_exchange = config.spot_exchange();
    let perp_exchange = config.perp_exchange();
    let mut strategy = ArbitrageStrategy::new(config);
    let mut ctx = MockContext::new();

    strategy.on_start(&mut ctx);

    // Spread = (3005 - 3000) / 3000 = 0.17% (below 0.3% threshold)
    feed_quotes(
        &mut strategy,
        &mut ctx,
        dec!(2999),
        dec!(3001), // spot mid = 3000
        dec!(3004),
        dec!(3006), // perp mid = 3005
        &spot_exchange,
        &perp_exchange,
        &spot_instrument,
        &perp_instrument,
    );

    assert!(
        ctx.placed_orders().is_empty(),
        "Should not open position when spread is below threshold"
    );
}

#[test]
fn test_opens_position_when_spread_exceeds_threshold() {
    let config = test_config();
    let spot_instrument = config.spot_instrument();
    let perp_instrument = config.perp_instrument();
    let spot_exchange = config.spot_exchange();
    let perp_exchange = config.perp_exchange();
    let mut strategy = ArbitrageStrategy::new(config);
    let mut ctx = MockContext::new();

    strategy.on_start(&mut ctx);

    // Spread = (3010 - 3000) / 3000 = 0.33% (above 0.3% threshold)
    feed_quotes(
        &mut strategy,
        &mut ctx,
        dec!(2999),
        dec!(3001), // spot mid = 3000
        dec!(3009),
        dec!(3011), // perp mid = 3010
        &spot_exchange,
        &perp_exchange,
        &spot_instrument,
        &perp_instrument,
    );

    let orders = ctx.placed_orders();
    assert_eq!(orders.len(), 2, "Should place spot and perp orders");

    let spot_order = orders.iter().find(|o| o.instrument == spot_instrument);
    let perp_order = orders.iter().find(|o| o.instrument == perp_instrument);

    assert!(spot_order.is_some(), "Should have spot order");
    assert!(perp_order.is_some(), "Should have perp order");
    assert_eq!(
        spot_order.unwrap().side,
        OrderSide::Buy,
        "Spot should be buy"
    );
    assert_eq!(
        perp_order.unwrap().side,
        OrderSide::Sell,
        "Perp should be sell"
    );
}

#[test]
fn test_does_not_open_when_exchange_halted() {
    let config = test_config();
    let spot_instrument = config.spot_instrument();
    let perp_instrument = config.perp_instrument();
    let spot_exchange = config.spot_exchange();
    let perp_exchange = config.perp_exchange();
    let mut strategy = ArbitrageStrategy::new(config);
    let mut ctx = MockContext::new();

    strategy.on_start(&mut ctx);
    ctx.set_exchange_health(spot_exchange.clone(), ExchangeHealth::Halted);

    // 0.5% spread (above threshold) but exchange halted
    feed_quotes(
        &mut strategy,
        &mut ctx,
        dec!(2999),
        dec!(3001),
        dec!(3014),
        dec!(3016),
        &spot_exchange,
        &perp_exchange,
        &spot_instrument,
        &perp_instrument,
    );

    assert!(
        ctx.placed_orders().is_empty(),
        "Should not open position when exchange is halted"
    );
}

#[test]
fn test_spread_convergence_triggers_close() {
    let config = test_config();
    let spot_instrument = config.spot_instrument();
    let perp_instrument = config.perp_instrument();
    let spot_exchange = config.spot_exchange();
    let perp_exchange = config.perp_exchange();
    let mut strategy = ArbitrageStrategy::new(config);
    let mut ctx = MockContext::new();

    strategy.on_start(&mut ctx);
    open_and_fill(
        &mut strategy,
        &mut ctx,
        &spot_exchange,
        &perp_exchange,
        &spot_instrument,
        &perp_instrument,
    );

    // Spread converges to -0.2% (below -0.1% threshold → should close)
    feed_quotes(
        &mut strategy,
        &mut ctx,
        dec!(3004),
        dec!(3006), // spot mid = 3005
        dec!(2998),
        dec!(3000), // perp mid = 2999 → spread = -0.2%
        &spot_exchange,
        &perp_exchange,
        &spot_instrument,
        &perp_instrument,
    );

    let orders = ctx.placed_orders();
    assert_eq!(orders.len(), 2, "Should close position with 2 orders");

    let spot_order = orders.iter().find(|o| o.instrument == spot_instrument);
    let perp_order = orders.iter().find(|o| o.instrument == perp_instrument);

    assert_eq!(
        spot_order.unwrap().side,
        OrderSide::Sell,
        "Close spot = sell"
    );
    assert_eq!(perp_order.unwrap().side, OrderSide::Buy, "Close perp = buy");
}

// ============================================================================
// Tests: Spread Boundary Cases
// ============================================================================

#[test]
fn test_exact_opening_spread_threshold_triggers_open() {
    // min_opening_spread_pct = 0.003 (0.3%)
    // spread = (perp_mid - spot_mid) / spot_mid >= 0.003 → open
    let config = test_config();
    let spot_instrument = config.spot_instrument();
    let perp_instrument = config.perp_instrument();
    let spot_exchange = config.spot_exchange();
    let perp_exchange = config.perp_exchange();
    let mut strategy = ArbitrageStrategy::new(config);
    let mut ctx = MockContext::new();

    strategy.on_start(&mut ctx);

    // spot_mid = 10000, perp_mid = 10030 → spread = 30/10000 = 0.003 (exactly at threshold)
    feed_quotes(
        &mut strategy,
        &mut ctx,
        dec!(9999),
        dec!(10001), // spot mid = 10000
        dec!(10029),
        dec!(10031), // perp mid = 10030
        &spot_exchange,
        &perp_exchange,
        &spot_instrument,
        &perp_instrument,
    );

    assert_eq!(
        ctx.placed_orders().len(),
        2,
        "Should open at exact threshold (>=)"
    );
}

#[test]
fn test_just_below_opening_spread_does_not_open() {
    // spread = 0.0029 → just below 0.003 threshold → should NOT open
    let config = test_config();
    let spot_instrument = config.spot_instrument();
    let perp_instrument = config.perp_instrument();
    let spot_exchange = config.spot_exchange();
    let perp_exchange = config.perp_exchange();
    let mut strategy = ArbitrageStrategy::new(config);
    let mut ctx = MockContext::new();

    strategy.on_start(&mut ctx);

    // spot_mid = 10000, perp_mid = 10029 → spread = 29/10000 = 0.0029 (just below 0.003)
    feed_quotes(
        &mut strategy,
        &mut ctx,
        dec!(9999),
        dec!(10001), // spot mid = 10000
        dec!(10028),
        dec!(10030), // perp mid = 10029
        &spot_exchange,
        &perp_exchange,
        &spot_instrument,
        &perp_instrument,
    );

    assert!(
        ctx.placed_orders().is_empty(),
        "Should NOT open when spread is just below threshold (0.29% < 0.3%)"
    );
}

#[test]
fn test_exact_closing_spread_threshold_triggers_close() {
    // min_closing_spread_pct = -0.001 (-0.1%)
    // spread <= -0.001 → close
    let config = test_config();
    let spot_instrument = config.spot_instrument();
    let perp_instrument = config.perp_instrument();
    let spot_exchange = config.spot_exchange();
    let perp_exchange = config.perp_exchange();
    let mut strategy = ArbitrageStrategy::new(config);
    let mut ctx = MockContext::new();

    strategy.on_start(&mut ctx);
    open_and_fill(
        &mut strategy,
        &mut ctx,
        &spot_exchange,
        &perp_exchange,
        &spot_instrument,
        &perp_instrument,
    );

    // spot_mid = 10000, perp_mid = 9990 → spread = -10/10000 = -0.001 (exactly at threshold)
    feed_quotes(
        &mut strategy,
        &mut ctx,
        dec!(9999),
        dec!(10001), // spot mid = 10000
        dec!(9989),
        dec!(9991), // perp mid = 9990
        &spot_exchange,
        &perp_exchange,
        &spot_instrument,
        &perp_instrument,
    );

    assert_eq!(
        ctx.placed_orders().len(),
        2,
        "Should close at exact threshold (<=)"
    );
}

#[test]
fn test_just_above_closing_spread_does_not_close() {
    // spread = -0.0009 → above -0.001 threshold → should NOT close
    let config = test_config();
    let spot_instrument = config.spot_instrument();
    let perp_instrument = config.perp_instrument();
    let spot_exchange = config.spot_exchange();
    let perp_exchange = config.perp_exchange();
    let mut strategy = ArbitrageStrategy::new(config);
    let mut ctx = MockContext::new();

    strategy.on_start(&mut ctx);
    open_and_fill(
        &mut strategy,
        &mut ctx,
        &spot_exchange,
        &perp_exchange,
        &spot_instrument,
        &perp_instrument,
    );

    // spot_mid = 10000, perp_mid = 9991 → spread = -9/10000 = -0.0009 (just above -0.001)
    feed_quotes(
        &mut strategy,
        &mut ctx,
        dec!(9999),
        dec!(10001), // spot mid = 10000
        dec!(9990),
        dec!(9992), // perp mid = 9991
        &spot_exchange,
        &perp_exchange,
        &spot_instrument,
        &perp_instrument,
    );

    assert!(
        ctx.placed_orders().is_empty(),
        "Should NOT close when spread is just above threshold (-0.09% > -0.1%)"
    );
}

// ============================================================================
// Tests: Slippage Buffers
// ============================================================================

#[test]
fn test_open_slippage_buffers_applied_to_prices() {
    // spot_slippage_buffer_pct = 0.001 (0.1%)
    // perp_slippage_buffer_pct = 0.001 (0.1%)
    // Open: buy spot slightly ABOVE mid, sell perp slightly BELOW mid
    let config = test_config();
    let spot_instrument = config.spot_instrument();
    let perp_instrument = config.perp_instrument();
    let spot_exchange = config.spot_exchange();
    let perp_exchange = config.perp_exchange();
    let mut strategy = ArbitrageStrategy::new(config);
    let mut ctx = MockContext::new();

    strategy.on_start(&mut ctx);

    // spot_mid = 10000, perp_mid = 10050 → spread = 0.5% (opens)
    feed_quotes(
        &mut strategy,
        &mut ctx,
        dec!(9999),
        dec!(10001), // spot mid = 10000
        dec!(10049),
        dec!(10051), // perp mid = 10050
        &spot_exchange,
        &perp_exchange,
        &spot_instrument,
        &perp_instrument,
    );

    let orders = ctx.placed_orders();
    assert_eq!(orders.len(), 2);

    let spot_order = orders
        .iter()
        .find(|o| o.instrument == spot_instrument)
        .unwrap();
    let perp_order = orders
        .iter()
        .find(|o| o.instrument == perp_instrument)
        .unwrap();

    // Spot BUY: price = 10000 * (1 + 0.001) = 10010 (slightly above mid to ensure fill)
    assert!(
        spot_order.price.0 > dec!(10000),
        "Spot buy price {} should be above mid (10000) by slippage buffer",
        spot_order.price
    );
    assert!(
        spot_order.price.0 <= dec!(10011),
        "Spot buy price {} should not exceed mid + buffer (10010)",
        spot_order.price
    );

    // Perp SELL: price = 10050 * (1 - 0.001) = 10039.95 (slightly below mid to ensure fill)
    assert!(
        perp_order.price.0 < dec!(10050),
        "Perp sell price {} should be below mid (10050) by slippage buffer",
        perp_order.price
    );
    assert!(
        perp_order.price.0 >= dec!(10039),
        "Perp sell price {} should not drop below mid - buffer (10039.95)",
        perp_order.price
    );
}

#[test]
fn test_close_slippage_buffers_applied_to_prices() {
    // Close: sell spot slightly BELOW mid, buy perp slightly ABOVE mid
    let config = test_config();
    let spot_instrument = config.spot_instrument();
    let perp_instrument = config.perp_instrument();
    let spot_exchange = config.spot_exchange();
    let perp_exchange = config.perp_exchange();
    let mut strategy = ArbitrageStrategy::new(config);
    let mut ctx = MockContext::new();

    strategy.on_start(&mut ctx);
    open_and_fill(
        &mut strategy,
        &mut ctx,
        &spot_exchange,
        &perp_exchange,
        &spot_instrument,
        &perp_instrument,
    );

    // Spread converges to -0.5% → triggers close
    // spot_mid = 10000, perp_mid = 9950
    feed_quotes(
        &mut strategy,
        &mut ctx,
        dec!(9999),
        dec!(10001), // spot mid = 10000
        dec!(9949),
        dec!(9951), // perp mid = 9950
        &spot_exchange,
        &perp_exchange,
        &spot_instrument,
        &perp_instrument,
    );

    let orders = ctx.placed_orders();
    assert_eq!(orders.len(), 2);

    let spot_order = orders
        .iter()
        .find(|o| o.instrument == spot_instrument)
        .unwrap();
    let perp_order = orders
        .iter()
        .find(|o| o.instrument == perp_instrument)
        .unwrap();

    // Spot SELL: price = 10000 * (1 - 0.001) = 9990 (slightly below mid to ensure fill)
    assert!(
        spot_order.price.0 < dec!(10000),
        "Spot sell price {} should be below mid (10000) by slippage buffer",
        spot_order.price
    );

    // Perp BUY: price = 9950 * (1 + 0.001) = 9959.95 (slightly above mid to ensure fill)
    assert!(
        perp_order.price.0 > dec!(9950),
        "Perp buy price {} should be above mid (9950) by slippage buffer",
        perp_order.price
    );
}

// ============================================================================
// Tests: Order Cancellation → Stop Strategy
// ============================================================================

#[test]
fn test_opening_order_canceled_stops_strategy() {
    let config = test_config();
    let spot_instrument = config.spot_instrument();
    let perp_instrument = config.perp_instrument();
    let spot_exchange = config.spot_exchange();
    let perp_exchange = config.perp_exchange();
    let mut strategy = ArbitrageStrategy::new(config);
    let mut ctx = MockContext::new();

    strategy.on_start(&mut ctx);

    // Open position (0.5% spread)
    feed_quotes(
        &mut strategy,
        &mut ctx,
        dec!(9999),
        dec!(10001),
        dec!(10049),
        dec!(10051),
        &spot_exchange,
        &perp_exchange,
        &spot_instrument,
        &perp_instrument,
    );

    assert_eq!(ctx.placed_orders().len(), 2);

    // Get the spot order ID
    let spot_order_id = ctx
        .placed_orders()
        .iter()
        .find(|o| o.instrument == spot_instrument)
        .map(|o| o.client_id.clone())
        .unwrap();

    // Simulate spot order cancellation (e.g., IoC didn't fill)
    let cancel_event = Event::OrderCanceled(OrderCanceledEvent {
        exchange: spot_exchange.exchange_id.clone(),
        instrument: spot_instrument.clone(),
        client_id: spot_order_id,
        reason: Some("IoC not filled".to_string()),
        ts: ctx.time_ms,
    });
    strategy.on_event(&mut ctx, &cancel_event);

    assert!(
        ctx.stopped,
        "Strategy should be stopped on opening order cancel"
    );
    assert!(
        ctx.stop_reason
            .as_ref()
            .unwrap()
            .contains("Opening order canceled"),
        "Stop reason should mention opening cancel. Got: {}",
        ctx.stop_reason.as_ref().unwrap()
    );
}

#[test]
fn test_closing_order_canceled_stops_strategy() {
    let config = test_config();
    let spot_instrument = config.spot_instrument();
    let perp_instrument = config.perp_instrument();
    let spot_exchange = config.spot_exchange();
    let perp_exchange = config.perp_exchange();
    let mut strategy = ArbitrageStrategy::new(config);
    let mut ctx = MockContext::new();

    strategy.on_start(&mut ctx);
    open_and_fill(
        &mut strategy,
        &mut ctx,
        &spot_exchange,
        &perp_exchange,
        &spot_instrument,
        &perp_instrument,
    );

    // Trigger close (spread = -0.5%)
    feed_quotes(
        &mut strategy,
        &mut ctx,
        dec!(9999),
        dec!(10001),
        dec!(9949),
        dec!(9951),
        &spot_exchange,
        &perp_exchange,
        &spot_instrument,
        &perp_instrument,
    );

    assert_eq!(ctx.placed_orders().len(), 2, "Should place close orders");

    // Get the perp close order ID
    let perp_order_id = ctx
        .placed_orders()
        .iter()
        .find(|o| o.instrument == perp_instrument)
        .map(|o| o.client_id.clone())
        .unwrap();

    // Simulate perp close order cancellation
    let cancel_event = Event::OrderCanceled(OrderCanceledEvent {
        exchange: perp_exchange.exchange_id.clone(),
        instrument: perp_instrument.clone(),
        client_id: perp_order_id,
        reason: Some("IoC not filled".to_string()),
        ts: ctx.time_ms,
    });
    strategy.on_event(&mut ctx, &cancel_event);

    assert!(
        ctx.stopped,
        "Strategy should be stopped on close order cancel"
    );
    assert!(
        ctx.stop_reason
            .as_ref()
            .unwrap()
            .contains("Close order canceled"),
        "Stop reason should mention close cancel. Got: {}",
        ctx.stop_reason.as_ref().unwrap()
    );
}

#[test]
fn test_unrelated_cancel_does_not_stop_strategy() {
    let config = test_config();
    let spot_instrument = config.spot_instrument();
    let spot_exchange = config.spot_exchange();
    let perp_exchange = config.perp_exchange();
    let mut strategy = ArbitrageStrategy::new(config);
    let mut ctx = MockContext::new();

    strategy.on_start(&mut ctx);

    // Strategy is in NoPosition state — cancel for a random order should be ignored
    let random_cancel = Event::OrderCanceled(OrderCanceledEvent {
        exchange: spot_exchange.exchange_id.clone(),
        instrument: spot_instrument.clone(),
        client_id: ClientOrderId::generate(),
        reason: Some("Manual cancel".to_string()),
        ts: ctx.time_ms,
    });
    strategy.on_event(&mut ctx, &random_cancel);

    assert!(
        !ctx.stopped,
        "Should NOT stop for unrelated order cancellation"
    );
}

// ============================================================================
// Tests: Production-like Config
// ============================================================================

#[test]
fn test_production_config_spread_thresholds() {
    // Use the actual production config values from config-v2-arb.json
    let mut config = test_config();
    config.min_opening_spread_pct = dec!(0.0001); // 0.01% to open
    config.min_closing_spread_pct = dec!(-0.03); // -3% to close
    config.spot_slippage_buffer_pct = dec!(0.001);
    config.perp_slippage_buffer_pct = dec!(0.001);

    let spot_instrument = config.spot_instrument();
    let perp_instrument = config.perp_instrument();
    let spot_exchange = config.spot_exchange();
    let perp_exchange = config.perp_exchange();
    let mut strategy = ArbitrageStrategy::new(config);
    let mut ctx = MockContext::new();

    strategy.on_start(&mut ctx);

    // With 0.01% threshold, even a tiny spread should trigger open
    // spot_mid = 10000, perp_mid = 10001 → spread = 1/10000 = 0.0001 (exactly at threshold)
    feed_quotes(
        &mut strategy,
        &mut ctx,
        dec!(9999),
        dec!(10001), // spot mid = 10000
        dec!(10000),
        dec!(10002), // perp mid = 10001
        &spot_exchange,
        &perp_exchange,
        &spot_instrument,
        &perp_instrument,
    );

    assert_eq!(
        ctx.placed_orders().len(),
        2,
        "Production config: should open at 0.01% spread"
    );
}

#[test]
fn test_production_config_close_requires_deep_convergence() {
    // With -3% closing threshold, spread needs to drop significantly
    let mut config = test_config();
    config.min_opening_spread_pct = dec!(0.0001);
    config.min_closing_spread_pct = dec!(-0.03); // -3% to close

    let spot_instrument = config.spot_instrument();
    let perp_instrument = config.perp_instrument();
    let spot_exchange = config.spot_exchange();
    let perp_exchange = config.perp_exchange();
    let mut strategy = ArbitrageStrategy::new(config);
    let mut ctx = MockContext::new();

    strategy.on_start(&mut ctx);
    open_and_fill(
        &mut strategy,
        &mut ctx,
        &spot_exchange,
        &perp_exchange,
        &spot_instrument,
        &perp_instrument,
    );

    // Spread at -1% → should NOT close (need -3%)
    feed_quotes(
        &mut strategy,
        &mut ctx,
        dec!(9999),
        dec!(10001), // spot mid = 10000
        dec!(9899),
        dec!(9901), // perp mid = 9900 → spread = -1%
        &spot_exchange,
        &perp_exchange,
        &spot_instrument,
        &perp_instrument,
    );

    assert!(
        ctx.placed_orders().is_empty(),
        "Production config: should NOT close at -1% spread (need -3%)"
    );

    ctx.advance_time(15000);

    // Spread at -3% → SHOULD close
    // spot_mid = 10000, perp_mid = 9700 → spread = -300/10000 = -3%
    feed_quotes(
        &mut strategy,
        &mut ctx,
        dec!(9999),
        dec!(10001), // spot mid = 10000
        dec!(9699),
        dec!(9701), // perp mid = 9700 → spread = -3%
        &spot_exchange,
        &perp_exchange,
        &spot_instrument,
        &perp_instrument,
    );

    assert_eq!(
        ctx.placed_orders().len(),
        2,
        "Production config: should close at -3% spread"
    );
}
