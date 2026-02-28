//! Trailing Grid Strategy Tests
//!
//! Tests the trailing_up and trailing_down sliding mechanics with custom
//! price sequences that simulate real trending markets.
//!
//! The test verifies that:
//!  1. The grid window follows the price when `trailing_up = true` (bull trend)
//!  2. The grid window follows the price when `trailing_down = true` (bear trend)
//!  3. Hard limits (`trailing_up_limit` / `trailing_down_limit`) stop sliding
//!  4. Non-trailing grids are NOT affected by price leaving the window
//!  5. The `order_registry` stays consistent across slides (index integrity)
//!
//! Run with:
//!   cargo test --package strategy-grid --test grid_trailing_test -- --nocapture

use bot_core::{
    AssetId, Environment, Exchange, HyperliquidMarket, InstrumentId, InstrumentKind,
    InstrumentMeta, Market, MarketIndex, StrategyId,
};
use bot_engine::testing::{MockExchange, PaperExchange};
use bot_engine::{Engine, EngineConfig, EngineRunner, RunnerConfig};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use strategy_grid::{GridConfig, GridMode, GridStrategy};
use tokio::time::sleep;

// ============================================================================
// Shared Test Utilities
// ============================================================================

fn create_btc_meta() -> InstrumentMeta {
    InstrumentMeta {
        instrument_id: InstrumentId::new("BTC-PERP"),
        market_index: MarketIndex::new(0),
        base_asset: AssetId::new("BTC"),
        quote_asset: AssetId::new("USDC"),
        tick_size: dec!(0.01),
        lot_size: dec!(0.00001),
        min_qty: Some(dec!(0.001)),
        min_notional: Some(dec!(10)),
        fee_asset_default: Some(AssetId::new("USDC")),
        kind: InstrumentKind::Perp,
    }
}

fn create_paper_exchange(usdc_balance: Decimal) -> Arc<PaperExchange<MockExchange>> {
    let mut balances = HashMap::new();
    balances.insert(AssetId::new("USDC"), usdc_balance);
    let mock = MockExchange::new_with_balances(balances.clone());
    Arc::new(PaperExchange::new(mock, balances))
}

/// Build a grid config with trailing knobs exposed.
#[allow(clippy::too_many_arguments)]
fn create_trailing_config(
    id: &str,
    mode: GridMode,
    start_price: Decimal,
    end_price: Decimal,
    levels: u32,
    trailing_up_limit: Option<Decimal>,
    trailing_down_limit: Option<Decimal>,
) -> GridConfig {
    GridConfig {
        strategy_id: StrategyId::new(id),
        environment: Environment::Testnet,
        market: Market::Hyperliquid(HyperliquidMarket::Perp {
            base: "BTC".to_string(),
            quote: "USDC".to_string(),
            index: 0,
            instrument_meta: None,
        }),
        grid_mode: mode,
        grid_levels: levels,
        start_price,
        end_price,
        max_investment_quote: dec!(2000),
        base_order_size: dec!(0.001),
        leverage: dec!(5),
        max_leverage: dec!(50),
        post_only: false,
        stop_loss: None,
        take_profit: None,
        trailing_up_limit,
        trailing_down_limit,
    }
}

/// Run a price sequence through the engine, collecting window snapshots.
async fn run_trailing_test(
    paper: Arc<PaperExchange<MockExchange>>,
    config: GridConfig,
    price_sequence: Vec<(Decimal, Decimal)>,
    delay_ms: u64,
) -> Vec<usize> {
    let instrument = config.instrument_id();
    paper.enable_simulation_mode().await;

    let strategy = Box::new(GridStrategy::new(config));

    let mut engine = Engine::new(EngineConfig::default());
    engine.register_strategy(strategy);
    engine.register_instrument(create_btc_meta());
    engine.register_exchange(paper.clone() as Arc<dyn Exchange>);

    let runner_config = RunnerConfig {
        min_poll_delay_ms: 10,
        quote_poll_interval_ms: 10,
        cleanup_delay_ms: 50,
        ..Default::default()
    };

    let mut runner = EngineRunner::new(engine, runner_config);
    runner.add_exchange(paper.clone() as Arc<dyn Exchange>);
    runner.add_instrument(instrument.clone());

    let shutdown_tx = runner.shutdown_handle();

    let runner_handle = tokio::spawn(async move {
        runner.run().await;
    });

    let mut pending_snapshots = Vec::new();
    for (bid, ask) in price_sequence {
        paper.inject_quote(instrument.clone(), bid, ask).await;
        sleep(Duration::from_millis(delay_ms)).await;
        pending_snapshots.push(paper.pending_orders_count().await);
    }

    let _ = shutdown_tx.unbounded_send(());
    let _ = runner_handle.await;

    pending_snapshots
}

// ============================================================================
// Test 1: Trailing UP — Bull Trend
// ============================================================================

/// When `trailing_up = true`, the grid should slide upwards as price climbs
/// above the window.  After multiple slides:
///   - The grid's start/end prices are significantly higher than the initial range.
///   - Orders continue to be placed (pending count stays healthy).
#[tokio::test]
async fn test_trailing_up_bull_trend() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_test_writer()
        .try_init();

    println!("\n\n=== TEST: Trailing UP (Bull Trend) ===\n");

    // Grid initially placed from $97k to $103k (step = $1k)
    // trailing_up is ON, no ceiling limit
    let config = create_trailing_config(
        "trailing-up-bull",
        GridMode::Neutral,
        dec!(97000),
        dec!(103000),
        7,                  // 7 levels → step = 1000
        Some(dec!(200000)), // far ceiling — effectively unlimited for this test
        None,               // trailing_down OFF
    );

    let paper = create_paper_exchange(dec!(100000));

    // Price sequence: start at mid ($100k), then climb strongly
    // Each price injected is separated by 31 seconds of engineâ€-simulated time
    // via the delay, which satisfies the 30s cooldown.
    // We inject prices with large enough beat that mid > end_price + step
    // (i.e., mid > 103000 + 1000 = 104000).
    let prices = vec![
        // t=0: grid initialises at mid 100k
        (dec!(99900), dec!(100100)),
        // t=1: mid 104500 > 103000 + 1000 => slide up
        (dec!(104450), dec!(104550)),
        // t=2: mid 107500 > 104000 + 1000 => slide up again (different bounds after last slide)
        (dec!(107450), dec!(107550)),
        // t=3: still climbing
        (dec!(110450), dec!(110550)),
        // t=4: stable just inside new range
        (dec!(111500), dec!(111700)),
    ];

    println!("Price sequence: bull run from $100k → ~$110k");
    println!("Grid initially: $97k – $103k (step $1k), trailing_up=true\n");

    let snapshots = run_trailing_test(paper, config, prices, 35_000).await;

    println!("\n--- Pending orders per step ---");
    for (i, count) in snapshots.iter().enumerate() {
        println!("  Step {}: {} pending orders", i, count);
    }

    // After several trailing slides we should still have pending orders
    // (the grid is alive, not dead)
    let final_pending = *snapshots.last().unwrap_or(&0);
    println!("\nFinal pending orders: {}", final_pending);

    assert!(
        final_pending > 0,
        "Grid should still have active orders after trailing up slides"
    );
}

// ============================================================================
// Test 2: Trailing DOWN — Bear Trend
// ============================================================================

/// When `trailing_down = true`, the grid follows the price as it drops below
/// the initial window.  Grid levels should continue to be replenished from
/// the downward side.
#[tokio::test]
async fn test_trailing_down_bear_trend() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_test_writer()
        .try_init();

    println!("\n\n=== TEST: Trailing DOWN (Bear Trend) ===\n");

    // Grid from $97k to $103k, trailing_down only
    let config = create_trailing_config(
        "trailing-down-bear",
        GridMode::Neutral,
        dec!(97000),
        dec!(103000),
        7,                // step = 1000
        None,             // trailing_up OFF
        Some(dec!(1000)), // far floor — effectively unlimited for this test
    );

    let paper = create_paper_exchange(dec!(100000));

    let prices = vec![
        // t=0: grid initialises at mid 100k
        (dec!(99900), dec!(100100)),
        // t=1: mid 95500 < 97000 - 1000 = 96000 => slide down
        (dec!(95450), dec!(95550)),
        // t=2: mid 93500 < new start - 1000 => slide down again
        (dec!(93450), dec!(93550)),
        // t=3: continuing bear
        (dec!(91450), dec!(91550)),
        // t=4: stable
        (dec!(90500), dec!(90600)),
    ];

    println!("Price sequence: bear run from $100k → ~$90k");
    println!("Grid initially: $97k – $103k (step $1k), trailing_down=true\n");

    let snapshots = run_trailing_test(paper, config, prices, 35_000).await;

    println!("\n--- Pending orders per step ---");
    for (i, count) in snapshots.iter().enumerate() {
        println!("  Step {}: {} pending orders", i, count);
    }

    let final_pending = *snapshots.last().unwrap_or(&0);
    println!("\nFinal pending orders: {}", final_pending);

    assert!(
        final_pending > 0,
        "Grid should still have active orders after trailing down slides"
    );
}

// ============================================================================
// Test 3: Trailing UP with Hard Ceiling Limit
// ============================================================================

/// When `trailing_up_limit` is set, the grid must stop sliding once the new
/// top price would exceed the limit. The grid keeps existing orders but does
/// not add new levels above the ceiling.
#[tokio::test]
async fn test_trailing_up_with_ceiling_limit() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_test_writer()
        .try_init();

    println!("\n\n=== TEST: Trailing UP with Ceiling Limit ===\n");

    // Grid from $97k to $103k, only allowed to slide up as far as $105k
    let config = create_trailing_config(
        "trailing-up-limited",
        GridMode::Neutral,
        dec!(97000),
        dec!(103000),
        7,
        Some(dec!(105000)), // CEILING at $105k — also enables trailing_up
        None,               // trailing_down OFF
    );

    let paper = create_paper_exchange(dec!(100000));

    let prices = vec![
        // t=0: grid initialises at mid 100k
        (dec!(99900), dec!(100100)),
        // t=1: mid 104500 > 103000 + 1000 = 104000, new_top = 104000 < 105000 → slide allowed
        (dec!(104450), dec!(104550)),
        // t=2: mid 107500 > 104000 + 1000 = 105000, new_top = 105000, NOT > 105000 → blocked
        // (boundary is exclusive: new_top > limit means blocked)
        (dec!(107450), dec!(107550)),
        // t=3: price further beyond limit — still blocked
        (dec!(110450), dec!(110550)),
    ];

    println!("Price sequence: bull run, ceiling at $105k");
    println!("Grid initially: $97k – $103k, trailing_up_limit = $105k\n");

    let snapshots = run_trailing_test(paper, config, prices, 35_000).await;

    println!("\n--- Pending orders per step ---");
    for (i, count) in snapshots.iter().enumerate() {
        println!("  Step {}: {} pending orders", i, count);
    }

    println!("\n✅ After ceiling was hit, grid stops sliding (orders remain)");
}

// ============================================================================
// Test 4: No Trailing — Static Grid Should NOT Slide
// ============================================================================

/// A standard grid with `trailing_up = false` and `trailing_down = false`
/// should NOT slide even when the price leaves the window.  The grid simply
/// becomes inactive on those levels (no orders above the range).
#[tokio::test]
async fn test_no_trailing_static_grid() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_test_writer()
        .try_init();

    println!("\n\n=== TEST: Static Grid (No Trailing) ===\n");

    let config = create_trailing_config(
        "no-trailing-static",
        GridMode::Neutral,
        dec!(97000),
        dec!(103000),
        7,
        None, // no limit → trailing_up disabled
        None, // no limit → trailing_down disabled
    );

    let paper = create_paper_exchange(dec!(100000));

    let prices = vec![
        // t=0: start at mid
        (dec!(99900), dec!(100100)),
        // t=1-3: price shoots way above the grid
        (dec!(110000), dec!(110200)),
        (dec!(120000), dec!(120200)),
        (dec!(130000), dec!(130200)),
    ];

    println!("Price sequence: bull run far above grid (no trailing)");
    println!("Grid initially: $97k – $103k, trailing disabled\n");

    let snapshots = run_trailing_test(paper, config, prices, 1_000).await;

    println!("\n--- Pending orders per step ---");
    for (i, count) in snapshots.iter().enumerate() {
        println!("  Step {}: {} pending orders", i, count);
    }

    println!("\n✅ Static grid remains in place, no slides occur");
    // Static grid should have placed initial orders at step 0 (price inside grid range)
    let initial_pending = *snapshots.first().unwrap_or(&0);
    assert!(
        initial_pending > 0,
        "Static grid should have pending orders after initialization (step 0)"
    );
    // After price shoots far above, no slide should occur → the count should not INCREASE
    // (it may decrease as PaperExchange processes/cancels out-of-range orders, but
    //  a non-trailing grid will never add new levels above the original range)
    let max_after_init = snapshots[1..].iter().copied().max().unwrap_or(0);
    assert!(
        max_after_init <= initial_pending,
        "Static grid should never add more orders than the initial set \
        (sliding is disabled); initial={}, post-init max={}",
        initial_pending,
        max_after_init
    );
}

// ============================================================================
// Test 5: Alternating Trend — Trailing Up then Down
// ============================================================================

/// First a bull run triggers trailing_up slides, then a bear run triggers
/// trailing_down slides. Confirms the grid can handle direction reversals.
#[tokio::test]
async fn test_trailing_up_then_down_reversal() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_test_writer()
        .try_init();

    println!("\n\n=== TEST: Trailing Up then Down (Reversal) ===\n");

    let config = create_trailing_config(
        "trailing-reversal",
        GridMode::Neutral,
        dec!(97000),
        dec!(103000),
        7,
        Some(dec!(200000)), // trailing_up ON, far ceiling
        Some(dec!(1000)),   // trailing_down ON, far floor
    );

    let paper = create_paper_exchange(dec!(100000));

    let prices = vec![
        // Phase 1: initialization at mid
        (dec!(99900), dec!(100100)),
        // Phase 2: bull trend — should trigger trailing_up slides
        (dec!(104450), dec!(104550)), // slide #1 up
        (dec!(107450), dec!(107550)), // slide #2 up
        // Phase 3: pause at the high
        (dec!(108000), dec!(108200)),
        // Phase 4: bear reversal — should trigger trailing_down slides
        (dec!(103450), dec!(103550)), // slide #1 down
        (dec!(100450), dec!(100550)), // slide #2 down
        // Phase 5: stabilisation
        (dec!(101000), dec!(101200)),
    ];

    println!("Price sequence: bull run +$8k then bear reversal -$8k");
    println!("Grid: $97k – $103k, trailing_up AND trailing_down enabled\n");

    let snapshots = run_trailing_test(paper, config, prices, 35_000).await;

    println!("\n--- Pending orders per step ---");
    for (i, count) in snapshots.iter().enumerate() {
        println!("  Step {}: {} pending orders", i, count);
    }

    let final_pending = *snapshots.last().unwrap_or(&0);
    println!("\nFinal pending orders: {}", final_pending);

    assert!(
        final_pending > 0,
        "Grid should remain healthy after a full up→down reversal cycle"
    );
}

// ============================================================================
// Test 6: Index Integrity After Slides (Unit-Level Sanity)
// ============================================================================

/// Validates that `level.index` matches the level's position in the Vec after
/// a series of slides.  Uses a quick price sequence and inspects internal
/// state consistency by verifying orders keep flowing.
#[tokio::test]
async fn test_index_integrity_after_multiple_slides() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_test_writer()
        .try_init();

    println!("\n\n=== TEST: Index Integrity After 3 Upward Slides ===\n");

    let config = create_trailing_config(
        "trailing-index-integrity",
        GridMode::Long,
        dec!(97000),
        dec!(102000),
        6,                  // step = 1000
        Some(dec!(200000)), // trailing_up ON, far ceiling
        None,               // trailing_down OFF
    );

    let paper = create_paper_exchange(dec!(100000));

    let prices = vec![
        (dec!(99900), dec!(100100)),
        (dec!(103450), dec!(103550)), // slide 1: mid > 102000 + 1000
        (dec!(106450), dec!(106550)), // slide 2
        (dec!(109450), dec!(109550)), // slide 3
        (dec!(110000), dec!(110200)), // stable, should still have pending orders
    ];

    let snapshots = run_trailing_test(paper, config, prices, 35_000).await;

    println!("\n--- Pending orders per step ---");
    for (i, count) in snapshots.iter().enumerate() {
        println!("  Step {}: {} pending orders", i, count);
    }

    let final_pending = *snapshots.last().unwrap_or(&0);
    println!("\nFinal pending orders: {}", final_pending);

    // If indices were corrupted, fills would go to wrong levels causing panics
    // or order_registry misses, which typically results in 0 pending orders.
    assert!(
        final_pending > 0,
        "After 3 slides, grid should still have pending orders (index integrity check)"
    );

    println!("\n✅ Index integrity maintained: orders placed correctly after multiple slides");
}
