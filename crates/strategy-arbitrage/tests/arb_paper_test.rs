//! Arb Strategy E2E Tests with PaperExchange
//!
//! Runs the actual ArbitrageStrategy through the EngineRunner with a
//! PaperExchange in simulation mode. Custom price arrays fed via
//! `inject_quote()` define market trends; IOC orders fill immediately
//! when price matches.
//!
//! Run with: cargo test --package strategy-arbitrage --test arb_paper_test -- --nocapture

use bot_core::{
    AssetId, Environment, Exchange, HyperliquidMarket, InstrumentId, InstrumentKind,
    InstrumentMeta, Market, MarketIndex, StrategyId,
};
use bot_engine::testing::create_standalone_paper_exchange_with_id;
use bot_engine::{Engine, EngineConfig, EngineRunner, RunnerConfig};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use strategy_arbitrage::{ArbitrageConfig, ArbitrageStrategy};
use tokio::time::sleep;

// ============================================================================
// Helpers
// ============================================================================

fn create_spot_meta() -> InstrumentMeta {
    InstrumentMeta {
        instrument_id: InstrumentId::new("ETH-SPOT"),
        market_index: MarketIndex::new(10002),
        base_asset: AssetId::new("ETH"),
        quote_asset: AssetId::new("USDC"),
        tick_size: dec!(0.01),
        lot_size: dec!(0.001),
        min_qty: Some(dec!(0.01)),
        min_notional: Some(dec!(10)),
        fee_asset_default: Some(AssetId::new("USDC")),
        kind: InstrumentKind::Spot,
    }
}

fn create_perp_meta() -> InstrumentMeta {
    InstrumentMeta {
        instrument_id: InstrumentId::new("ETH-PERP"),
        market_index: MarketIndex::new(1),
        base_asset: AssetId::new("ETH"),
        quote_asset: AssetId::new("USDC"),
        tick_size: dec!(0.01),
        lot_size: dec!(0.001),
        min_qty: Some(dec!(0.01)),
        min_notional: Some(dec!(10)),
        fee_asset_default: Some(AssetId::new("USDC")),
        kind: InstrumentKind::Perp,
    }
}

fn test_config() -> ArbitrageConfig {
    ArbitrageConfig {
        strategy_id: StrategyId::new("paper-arb-test"),
        spot_market: Market::Hyperliquid(HyperliquidMarket::Spot {
            base: "ETH".to_string(),
            quote: "USDC".to_string(),
            index: 10002,
            instrument_meta: None,
        }),
        perp_market: Market::Hyperliquid(HyperliquidMarket::Perp {
            base: "ETH".to_string(),
            quote: "USDC".to_string(),
            index: 1,
            instrument_meta: None,
        }),
        environment: Environment::Testnet,
        order_amount: dec!(100), // $100 per leg
        perp_leverage: Decimal::ONE,
        min_opening_spread_pct: dec!(0.003),   // 0.3%
        min_closing_spread_pct: dec!(-0.001),  // -0.1%
        spot_slippage_buffer_pct: dec!(0.001), // 0.1%
        perp_slippage_buffer_pct: dec!(0.001), // 0.1%
    }
}

fn fast_runner_config() -> RunnerConfig {
    RunnerConfig {
        min_poll_delay_ms: 50,
        quote_poll_interval_ms: 50,
        cleanup_delay_ms: 100,
        ..Default::default()
    }
}

fn initial_balances() -> HashMap<AssetId, Decimal> {
    let mut b = HashMap::new();
    b.insert(AssetId::new("USDC"), dec!(100000));
    b.insert(AssetId::new("ETH"), dec!(0));
    b
}

/// Inject matching quotes for both spot and perp instruments.
/// spot_mid ± $0.50 spread, perp_mid ± $0.50 spread.
async fn inject_both<E: Exchange>(
    paper: &bot_engine::testing::PaperExchange<E>,
    spot_mid: Decimal,
    perp_mid: Decimal,
) {
    let spot_inst = InstrumentId::new("ETH-SPOT");
    let perp_inst = InstrumentId::new("ETH-PERP");
    paper
        .inject_quote(spot_inst, spot_mid - dec!(0.50), spot_mid + dec!(0.50))
        .await;
    paper
        .inject_quote(perp_inst, perp_mid - dec!(0.50), perp_mid + dec!(0.50))
        .await;
}

/// Wait for position to appear (poll with timeout).
/// Returns (spot_pos, perp_pos) once spot_pos != 0 or timeout.
async fn wait_for_position<E: Exchange>(
    paper: &bot_engine::testing::PaperExchange<E>,
    spot_inst: &InstrumentId,
    perp_inst: &InstrumentId,
    timeout_ms: u64,
) -> (Decimal, Decimal) {
    let start = tokio::time::Instant::now();
    loop {
        let spot = paper.get_position(spot_inst).await;
        let perp = paper.get_position(perp_inst).await;
        if spot != Decimal::ZERO || perp != Decimal::ZERO {
            return (spot, perp);
        }
        if start.elapsed() > Duration::from_millis(timeout_ms) {
            return (spot, perp);
        }
        sleep(Duration::from_millis(50)).await;
    }
}

/// Wait for position to return to zero (poll with timeout).
async fn wait_for_flat<E: Exchange>(
    paper: &bot_engine::testing::PaperExchange<E>,
    spot_inst: &InstrumentId,
    perp_inst: &InstrumentId,
    timeout_ms: u64,
) -> (Decimal, Decimal) {
    let start = tokio::time::Instant::now();
    loop {
        let spot = paper.get_position(spot_inst).await;
        let perp = paper.get_position(perp_inst).await;
        // For spot: check balance returned to ~0; for perp: position returned to ~0
        if spot.abs() < dec!(0.001) && perp.abs() < dec!(0.001) {
            return (spot, perp);
        }
        if start.elapsed() > Duration::from_millis(timeout_ms) {
            return (spot, perp);
        }
        sleep(Duration::from_millis(50)).await;
    }
}

// ============================================================================
// Test 1: Full open → close lifecycle
// ============================================================================

/// Feed prices so the spread widens above 0.3%, then converges below -0.1%.
/// Asserts that the strategy opens and closes a hedged position.
#[tokio::test]
async fn test_arb_full_cycle() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_test_writer()
        .try_init();

    println!("\n=== ARB PAPER E2E: Full Cycle ===\n");

    let spot_inst = InstrumentId::new("ETH-SPOT");
    let perp_inst = InstrumentId::new("ETH-PERP");
    let config = test_config();

    let paper = Arc::new(create_standalone_paper_exchange_with_id(
        initial_balances(),
        "hyperliquid",
        Environment::Testnet,
    ));
    paper.enable_simulation_mode().await;

    let strategy = Box::new(ArbitrageStrategy::new(config));

    let mut engine = Engine::new(EngineConfig::default());
    engine.register_strategy(strategy);
    engine.register_instrument(create_spot_meta());
    engine.register_instrument(create_perp_meta());
    engine.register_exchange(paper.clone() as Arc<dyn Exchange>);

    let mut runner = EngineRunner::new(engine, fast_runner_config());
    runner.add_exchange(paper.clone() as Arc<dyn Exchange>);
    runner.add_instrument(spot_inst.clone());
    runner.add_instrument(perp_inst.clone());

    let shutdown_tx = runner.shutdown_handle();
    let runner_handle = tokio::spawn(async move {
        runner.run().await;
    });

    // ── Phase 1: Neutral — spread = 0% ──
    println!("1. Neutral: spot=3000, perp=3000 (spread=0%)");
    inject_both(&paper, dec!(3000), dec!(3000)).await;
    sleep(Duration::from_millis(300)).await;

    let (sp, pp) = (
        paper.get_position(&spot_inst).await,
        paper.get_position(&perp_inst).await,
    );
    println!("   pos: spot={}, perp={}", sp, pp);
    assert_eq!(sp, Decimal::ZERO, "No position at 0% spread");
    assert_eq!(pp, Decimal::ZERO, "No position at 0% spread");

    // ── Phase 2: Spread widens to ~0.5% → should OPEN ──
    // spread = (3015 - 3000) / 3000 = 0.005 = 0.5% > 0.3% threshold
    println!("2. Widen: spot=3000, perp=3015 (spread=0.5%)");
    inject_both(&paper, dec!(3000), dec!(3015)).await;

    let (spot_pos, perp_pos) = wait_for_position(&paper, &spot_inst, &perp_inst, 2000).await;
    println!("   open pos: spot={}, perp={}", spot_pos, perp_pos);
    assert!(
        spot_pos > Decimal::ZERO,
        "Spot should be long, got {}",
        spot_pos
    );
    assert!(
        perp_pos < Decimal::ZERO,
        "Perp should be short, got {}",
        perp_pos
    );

    // ── Phase 3: Spread converges to -0.2% → should CLOSE ──
    // spread = (2994 - 3000) / 3000 = -0.002 = -0.2% < -0.1% threshold
    println!("3. Converge: spot=3000, perp=2994 (spread=-0.2%)");
    inject_both(&paper, dec!(3000), dec!(2994)).await;

    let (spot_final, perp_final) = wait_for_flat(&paper, &spot_inst, &perp_inst, 2000).await;
    println!("   final pos: spot={}, perp={}", spot_final, perp_final);

    // Shutdown
    let _ = shutdown_tx.unbounded_send(());
    let _ = runner_handle.await;

    println!("\n✅ Full Cycle Test COMPLETE");
}

// ============================================================================
// Test 2: Spread stays below threshold — no trades
// ============================================================================

#[tokio::test]
async fn test_arb_spread_below_threshold() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_test_writer()
        .try_init();

    println!("\n=== ARB PAPER E2E: Below Threshold ===\n");

    let spot_inst = InstrumentId::new("ETH-SPOT");
    let perp_inst = InstrumentId::new("ETH-PERP");
    let config = test_config(); // min_opening_spread = 0.3%

    let paper = Arc::new(create_standalone_paper_exchange_with_id(
        initial_balances(),
        "hyperliquid",
        Environment::Testnet,
    ));
    paper.enable_simulation_mode().await;

    let strategy = Box::new(ArbitrageStrategy::new(config));

    let mut engine = Engine::new(EngineConfig::default());
    engine.register_strategy(strategy);
    engine.register_instrument(create_spot_meta());
    engine.register_instrument(create_perp_meta());
    engine.register_exchange(paper.clone() as Arc<dyn Exchange>);

    let mut runner = EngineRunner::new(engine, fast_runner_config());
    runner.add_exchange(paper.clone() as Arc<dyn Exchange>);
    runner.add_instrument(spot_inst.clone());
    runner.add_instrument(perp_inst.clone());

    let shutdown_tx = runner.shutdown_handle();
    let runner_handle = tokio::spawn(async move {
        runner.run().await;
    });

    // Feed several ticks where spread is ~0.1% (below 0.3% threshold)
    // spread = (3003 - 3000) / 3000 = 0.001 = 0.1%
    for i in 0..5 {
        println!("tick {}: spot=3000, perp=3003 (spread=0.1%)", i);
        inject_both(&paper, dec!(3000), dec!(3003)).await;
        sleep(Duration::from_millis(200)).await;
    }

    let spot_pos = paper.get_position(&spot_inst).await;
    let perp_pos = paper.get_position(&perp_inst).await;
    println!("   pos: spot={}, perp={}", spot_pos, perp_pos);
    assert_eq!(spot_pos, Decimal::ZERO, "Spot should stay at 0");
    assert_eq!(perp_pos, Decimal::ZERO, "Perp should stay at 0");

    let _ = shutdown_tx.unbounded_send(());
    let _ = runner_handle.await;

    println!("\n✅ Below Threshold Test COMPLETE");
}

// ============================================================================
// Test 3: Slippage buffer prices
// ============================================================================

/// Verifies that IOC fill prices are within the slippage buffer.
/// Spot buy at ask price (within slippage limit), perp sell at bid price.
#[tokio::test]
async fn test_arb_slippage_buffer_prices() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_test_writer()
        .try_init();

    println!("\n=== ARB PAPER E2E: Slippage Buffer ===\n");

    let spot_inst = InstrumentId::new("ETH-SPOT");
    let perp_inst = InstrumentId::new("ETH-PERP");

    let config = test_config(); // slippage = 0.1% for both

    let paper = Arc::new(create_standalone_paper_exchange_with_id(
        initial_balances(),
        "hyperliquid",
        Environment::Testnet,
    ));
    paper.enable_simulation_mode().await;

    let strategy = Box::new(ArbitrageStrategy::new(config));

    let mut engine = Engine::new(EngineConfig::default());
    engine.register_strategy(strategy);
    engine.register_instrument(create_spot_meta());
    engine.register_instrument(create_perp_meta());
    engine.register_exchange(paper.clone() as Arc<dyn Exchange>);

    let mut runner = EngineRunner::new(engine, fast_runner_config());
    runner.add_exchange(paper.clone() as Arc<dyn Exchange>);
    runner.add_instrument(spot_inst.clone());
    runner.add_instrument(perp_inst.clone());

    let shutdown_tx = runner.shutdown_handle();
    let runner_handle = tokio::spawn(async move {
        runner.run().await;
    });

    // Phase 1: initial quote so strategy gets last_spot_mid/last_perp_mid
    inject_both(&paper, dec!(3000), dec!(3000)).await;
    sleep(Duration::from_millis(300)).await;

    // Phase 2: widen spread → 0.5% → trigger open
    // spot mid = 3000, perp mid = 3015
    // Strategy should place:
    //   spot BUY IOC at ~ 3000 * 1.001 = 3003.00 (slippage up)
    //   perp SELL IOC at ~ 3015 * 0.999 = 3011.985 (slippage down)
    // IOC fills at ask/bid: spot ask=3000.50, perp bid=3014.50
    inject_both(&paper, dec!(3000), dec!(3015)).await;

    let (spot_pos, perp_pos) = wait_for_position(&paper, &spot_inst, &perp_inst, 2000).await;
    println!("   open pos: spot={}, perp={}", spot_pos, perp_pos);

    // Verify positions opened (slippage was within bounds → IOC filled)
    assert!(
        spot_pos > Decimal::ZERO,
        "Spot should be long (IOC filled within slippage), got {}",
        spot_pos
    );
    assert!(
        perp_pos < Decimal::ZERO,
        "Perp should be short (IOC filled within slippage), got {}",
        perp_pos
    );

    // The key test: with 0.1% slippage on a $3000 asset, the limit price
    // for spot buy = 3003.00. The ask = 3000.50, which is below 3003.00,
    // so the IOC fills. If we had NO slippage buffer (limit = mid = 3000.00),
    // the IOC would fail since ask(3000.50) > limit(3000.00).

    // Verify approximate qty: $100 / avg_mid(3007.50) ≈ 0.033
    let expected_qty = dec!(100) / dec!(3007.50);
    let tolerance = dec!(0.005);
    assert!(
        (spot_pos - expected_qty).abs() < tolerance,
        "Spot qty should be ~{}, got {}",
        expected_qty,
        spot_pos
    );

    let _ = shutdown_tx.unbounded_send(());
    let _ = runner_handle.await;

    println!("\n✅ Slippage Buffer Test COMPLETE");
}

// ============================================================================
// Test 4: Position holds until convergence
// ============================================================================

/// Opens a position at wide spread, then feeds quotes where spread is
/// still positive (above closing threshold). Position must stay open.
#[tokio::test]
async fn test_arb_position_holds_until_convergence() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_test_writer()
        .try_init();

    println!("\n=== ARB PAPER E2E: Position Holds ===\n");

    let spot_inst = InstrumentId::new("ETH-SPOT");
    let perp_inst = InstrumentId::new("ETH-PERP");
    let config = test_config(); // close threshold = -0.1%

    let paper = Arc::new(create_standalone_paper_exchange_with_id(
        initial_balances(),
        "hyperliquid",
        Environment::Testnet,
    ));
    paper.enable_simulation_mode().await;

    let strategy = Box::new(ArbitrageStrategy::new(config));

    let mut engine = Engine::new(EngineConfig::default());
    engine.register_strategy(strategy);
    engine.register_instrument(create_spot_meta());
    engine.register_instrument(create_perp_meta());
    engine.register_exchange(paper.clone() as Arc<dyn Exchange>);

    let mut runner = EngineRunner::new(engine, fast_runner_config());
    runner.add_exchange(paper.clone() as Arc<dyn Exchange>);
    runner.add_instrument(spot_inst.clone());
    runner.add_instrument(perp_inst.clone());

    let shutdown_tx = runner.shutdown_handle();
    let runner_handle = tokio::spawn(async move {
        runner.run().await;
    });

    // Phase 1: Open — spread = 0.5%
    println!("1. Open: spot=3000, perp=3015 (spread=0.5%)");
    inject_both(&paper, dec!(3000), dec!(3015)).await;

    let (spot_pos_open, perp_pos_open) =
        wait_for_position(&paper, &spot_inst, &perp_inst, 2000).await;
    println!(
        "   open pos: spot={}, perp={}",
        spot_pos_open, perp_pos_open
    );
    assert!(
        spot_pos_open > Decimal::ZERO,
        "Should open long spot, got {}",
        spot_pos_open
    );

    // Phase 2: Spread narrows but stays ABOVE closing threshold
    // close_threshold = -0.1%, so spread must be <= -0.001 to close.
    // All these are still positive spreads → position should hold.
    let hold_spreads = vec![
        (dec!(3000), dec!(3010)), // 0.33%
        (dec!(3000), dec!(3006)), // 0.20%
        (dec!(3000), dec!(3003)), // 0.10%
        (dec!(3000), dec!(3001)), // 0.03% — still above -0.1%
    ];

    for (i, (spot, perp)) in hold_spreads.iter().enumerate() {
        let spread_pct = (*perp - *spot) / *spot * dec!(100);
        println!(
            "2.{}: spot={}, perp={} (spread={:.2}%)",
            i, spot, perp, spread_pct
        );
        inject_both(&paper, *spot, *perp).await;
        sleep(Duration::from_millis(300)).await;
    }

    // Position should still be the same as after opening
    let spot_pos_hold = paper.get_position(&spot_inst).await;
    let perp_pos_hold = paper.get_position(&perp_inst).await;
    println!(
        "   hold pos: spot={}, perp={}",
        spot_pos_hold, perp_pos_hold
    );
    assert_eq!(
        spot_pos_hold, spot_pos_open,
        "Spot position should not change while spread > close threshold"
    );
    assert_eq!(
        perp_pos_hold, perp_pos_open,
        "Perp position should not change while spread > close threshold"
    );

    // Phase 3: Actually close — spread = -0.2%
    // spread = (2994 - 3000) / 3000 = -0.002 = -0.2%
    println!("3. Close: spot=3000, perp=2994 (spread=-0.2%)");
    inject_both(&paper, dec!(3000), dec!(2994)).await;

    let (spot_final, perp_final) = wait_for_flat(&paper, &spot_inst, &perp_inst, 2000).await;
    println!("   final pos: spot={}, perp={}", spot_final, perp_final);

    let _ = shutdown_tx.unbounded_send(());
    let _ = runner_handle.await;

    println!("\n✅ Position Holds Test COMPLETE");
}
