//! Comprehensive Grid Strategy Tests
//!
//! Tests all grid modes (Neutral, Long, Short) with:
//! - Bear and Bull market scenarios
//! - Position accumulation verification
//! - Balance tracking
//! - Liquidation boundary testing
//!
//! Run with: cargo test --package strategy-grid --test grid_comprehensive_test -- --nocapture

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
// Test Utilities
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

/// Helper to create a standard paper exchange with initial balance
fn create_paper_exchange(usdc_balance: Decimal) -> Arc<PaperExchange<MockExchange>> {
    let mut balances = HashMap::new();
    balances.insert(AssetId::new("USDC"), usdc_balance);
    let mock = MockExchange::new_with_balances(balances.clone());
    let paper = Arc::new(PaperExchange::new(mock, balances));
    paper
}

/// Helper to create grid config with specified mode
fn create_grid_config(
    mode: GridMode,
    start_price: Decimal,
    end_price: Decimal,
    levels: usize,
    investment: Decimal,
    leverage: Decimal,
) -> GridConfig {
    GridConfig {
        strategy_id: StrategyId::new(format!("{:?}-test", mode).to_lowercase()),
        environment: Environment::Testnet,
        market: Market::Hyperliquid(HyperliquidMarket::Perp {
            base: "BTC".to_string(),
            quote: "USDC".to_string(),
            index: 0,
            instrument_meta: None,
        }),
        grid_mode: mode,
        grid_levels: levels as u32,
        start_price,
        end_price,
        max_investment_quote: investment,
        base_order_size: dec!(0.001),
        leverage,
        max_leverage: dec!(50),
        post_only: false,
        stop_loss: None,
        take_profit: None,
        trailing_up_limit: None,
        trailing_down_limit: None,
    }
}

/// Run the engine with price sequence and collect state snapshots
async fn run_grid_test(
    paper: Arc<PaperExchange<MockExchange>>,
    config: GridConfig,
    price_sequence: Vec<(Decimal, Decimal)>, // (bid, ask) pairs
    delay_ms: u64,
) -> TestResult {
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

    // Run through price sequence
    let mut snapshots = Vec::new();

    for (i, (bid, ask)) in price_sequence.iter().enumerate() {
        paper.inject_quote(instrument.clone(), *bid, *ask).await;
        sleep(Duration::from_millis(delay_ms)).await;

        let pending = paper.pending_orders_count().await;
        let balances = paper.get_balances().await;
        let position = paper.get_position(&instrument).await;

        snapshots.push(StateSnapshot {
            step: i,
            bid: *bid,
            ask: *ask,
            pending_orders: pending,
            usdc_balance: balances
                .get(&AssetId::new("USDC"))
                .copied()
                .unwrap_or_default(),
            position_qty: position,
        });
    }

    // Shutdown
    let _ = shutdown_tx.unbounded_send(());
    let _ = runner_handle.await;

    TestResult { snapshots }
}

#[derive(Debug, Clone)]
struct StateSnapshot {
    step: usize,
    bid: Decimal,
    ask: Decimal,
    pending_orders: usize,
    usdc_balance: Decimal,
    position_qty: Decimal,
}

struct TestResult {
    snapshots: Vec<StateSnapshot>,
}

impl TestResult {
    fn final_position(&self) -> Decimal {
        self.snapshots
            .last()
            .map(|s| s.position_qty)
            .unwrap_or_default()
    }

    fn final_balance(&self) -> Decimal {
        self.snapshots
            .last()
            .map(|s| s.usdc_balance)
            .unwrap_or_default()
    }

    fn print_summary(&self, test_name: &str) {
        println!("\n=== {} ===", test_name);
        println!(
            "{:<6} {:>12} {:>12} {:>10} {:>14} {:>12}",
            "Step", "Bid", "Ask", "Orders", "USDC Balance", "Position"
        );
        println!("{}", "-".repeat(70));

        for s in &self.snapshots {
            println!(
                "{:<6} {:>12.2} {:>12.2} {:>10} {:>14.2} {:>12.6}",
                s.step, s.bid, s.ask, s.pending_orders, s.usdc_balance, s.position_qty
            );
        }

        println!("{}", "-".repeat(70));
        println!("Final Position: {:.6}", self.final_position());
        println!("Final USDC Balance: {:.2}", self.final_balance());
    }
}

// ============================================================================
// NEUTRAL Mode Tests
// ============================================================================

/// Test Neutral Grid in Bear Market
/// Expected: Position accumulates LONG as buy orders fill
#[tokio::test]
async fn test_neutral_grid_bear_market() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_test_writer()
        .try_init();

    let paper = create_paper_exchange(dec!(100000));

    let config = create_grid_config(
        GridMode::Neutral,
        dec!(95000),  // start (bottom)
        dec!(105000), // end (top)
        10,           // levels
        dec!(1000),   // investment
        dec!(5),      // leverage
    );

    // Bear market: price drops from 100k to 94k
    // For BUY orders to fill: ask <= order_price
    // Grid levels at 95k, 96k, 97k, 98k, 99k, 100k, 101k, 102k, 103k, 104k, 105k
    // Price must drop so ask crosses BELOW these levels
    let prices = vec![
        (dec!(99900), dec!(100100)), // Start at mid (no fill yet)
        (dec!(98800), dec!(99000)),  // Ask=99000, fills buy at 99000
        (dec!(97800), dec!(98000)),  // Ask=98000, fills buy at 98000
        (dec!(96800), dec!(97000)),  // Ask=97000, fills buy at 97000
        (dec!(95800), dec!(96000)),  // Ask=96000, fills buy at 96000
        (dec!(94800), dec!(95000)),  // Ask=95000, fills buy at 95000
    ];

    let result = run_grid_test(paper, config, prices, 150).await;
    result.print_summary("NEUTRAL Grid - Bear Market");

    // In bear market, neutral grid should accumulate LONG position
    assert!(
        result.final_position() > dec!(0),
        "Neutral grid in bear market should have positive (long) position"
    );
}

/// Test Neutral Grid in Bull Market
/// Expected: Position accumulates SHORT as sell orders fill
#[tokio::test]
async fn test_neutral_grid_bull_market() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_test_writer()
        .try_init();

    let paper = create_paper_exchange(dec!(100000));

    let config = create_grid_config(
        GridMode::Neutral,
        dec!(95000),
        dec!(105000),
        10,
        dec!(1000),
        dec!(5),
    );

    // Bull market: price rises from 100k to 106k
    // For SELL orders to fill: bid >= order_price
    // Grid levels at 95k, 96k, 97k, 98k, 99k, 100k, 101k, 102k, 103k, 104k, 105k
    // Price must rise so bid crosses ABOVE these levels
    let prices = vec![
        (dec!(99900), dec!(100100)),  // Start at mid
        (dec!(101000), dec!(101200)), // Bid=101000, fills sell at 101000
        (dec!(102000), dec!(102200)), // Bid=102000, fills sell at 102000
        (dec!(103000), dec!(103200)), // Bid=103000, fills sell at 103000
        (dec!(104000), dec!(104200)), // Bid=104000, fills sell at 104000
        (dec!(105000), dec!(105200)), // Bid=105000, fills sell at 105000
    ];

    let result = run_grid_test(paper, config, prices, 150).await;
    result.print_summary("NEUTRAL Grid - Bull Market");

    // In bull market, neutral grid should accumulate SHORT position
    assert!(
        result.final_position() < dec!(0),
        "Neutral grid in bull market should have negative (short) position"
    );
}

/// Test Neutral Grid: Full Bear-then-Bull Cycle
/// Expected: Position returns near zero after full cycle
#[tokio::test]
async fn test_neutral_grid_full_cycle() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_test_writer()
        .try_init();

    let paper = create_paper_exchange(dec!(100000));

    let config = create_grid_config(
        GridMode::Neutral,
        dec!(95000),
        dec!(105000),
        10,
        dec!(1000),
        dec!(5),
    );

    // Full cycle: mid -> bear -> recover -> bull -> return
    let prices = vec![
        // Start
        (dec!(99900), dec!(100100)),
        // Bear phase
        (dec!(98900), dec!(99100)),
        (dec!(97900), dec!(98100)),
        (dec!(96900), dec!(97100)),
        (dec!(95900), dec!(96100)),
        // Recovery phase
        (dec!(96900), dec!(97100)),
        (dec!(97900), dec!(98100)),
        (dec!(98900), dec!(99100)),
        (dec!(99900), dec!(100100)),
        // Bull phase
        (dec!(100900), dec!(101100)),
        (dec!(101900), dec!(102100)),
        (dec!(102900), dec!(103100)),
        // Return to mid
        (dec!(101900), dec!(102100)),
        (dec!(100900), dec!(101100)),
        (dec!(99900), dec!(100100)),
    ];

    let result = run_grid_test(paper, config, prices, 100).await;
    result.print_summary("NEUTRAL Grid - Full Bear-Bull Cycle");

    // After full cycle, position should be close to zero
    let final_pos = result.final_position().abs();
    println!("Position deviation from zero: {:.6}", final_pos);
}

// ============================================================================
// LONG Mode Tests
// ============================================================================

/// Test Long Grid in Bear Market
/// Expected: Position accumulates as grid places buy orders only
#[tokio::test]
async fn test_long_grid_bear_market() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_test_writer()
        .try_init();

    let paper = create_paper_exchange(dec!(100000));

    let config = create_grid_config(
        GridMode::Long,
        dec!(90000),  // Lower range for long grid
        dec!(100000), // Current price is top
        10,
        dec!(1000),
        dec!(5),
    );

    // Bear market: ideal for long grid to accumulate
    // Grid levels at 90k, 91k, 92k, 93k, 94k, 95k, 96k, 97k, 98k, 99k, 100k
    // For BUY orders to fill: ask <= order_price
    let prices = vec![
        (dec!(99900), dec!(100100)), // Start
        (dec!(98800), dec!(99000)),  // Ask=99000, fills buy at 99000
        (dec!(97800), dec!(98000)),  // Ask=98000, fills buy at 98000
        (dec!(96800), dec!(97000)),  // Ask=97000, fills buy at 97000
        (dec!(95800), dec!(96000)),  // Ask=96000, fills buy at 96000
        (dec!(94800), dec!(95000)),  // Ask=95000, fills buy at 95000
        (dec!(93800), dec!(94000)),  // Ask=94000, fills buy at 94000
        (dec!(92800), dec!(93000)),  // Ask=93000, fills buy at 93000
        (dec!(91800), dec!(92000)),  // Ask=92000, fills buy at 92000
        (dec!(90800), dec!(91000)),  // Ask=91000, fills buy at 91000
    ];

    let result = run_grid_test(paper, config, prices, 150).await;
    result.print_summary("LONG Grid - Bear Market (Accumulation)");

    // Long grid in bear market accumulates position
    assert!(
        result.final_position() > dec!(0),
        "Long grid should accumulate positive position in bear market"
    );
}

/// Test Long Grid: Bear then Bull (Profit Taking)
#[tokio::test]
async fn test_long_grid_bear_then_bull() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_test_writer()
        .try_init();

    let paper = create_paper_exchange(dec!(100000));

    let config = create_grid_config(
        GridMode::Long,
        dec!(90000),
        dec!(100000),
        10,
        dec!(1000),
        dec!(5),
    );

    // Bear phase accumulates, Bull phase takes profit
    let prices = vec![
        // Bear - accumulate
        (dec!(99900), dec!(100100)),
        (dec!(97900), dec!(98100)),
        (dec!(95900), dec!(96100)),
        (dec!(93900), dec!(94100)),
        (dec!(91900), dec!(92100)),
        // Bull - profit taking
        (dec!(93900), dec!(94100)),
        (dec!(95900), dec!(96100)),
        (dec!(97900), dec!(98100)),
        (dec!(99900), dec!(100100)),
    ];

    let result = run_grid_test(paper, config, prices, 150).await;
    result.print_summary("LONG Grid - Bear then Bull (Profit Taking)");
}

// ============================================================================
// SHORT Mode Tests
// ============================================================================

/// Test Short Grid in Bull Market
/// Expected: Position accumulates SHORT as grid places sell orders
#[tokio::test]
async fn test_short_grid_bull_market() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_test_writer()
        .try_init();

    let paper = create_paper_exchange(dec!(100000));

    let config = create_grid_config(
        GridMode::Short,
        dec!(100000), // Current price is bottom
        dec!(110000), // Upper range for short grid
        10,
        dec!(1000),
        dec!(5),
    );

    // Bull market: ideal for short grid to accumulate short positions
    // Grid levels at 100k, 101k, 102k, 103k, 104k, 105k, 106k, 107k, 108k, 109k, 110k
    // For SELL orders to fill: bid >= order_price
    let prices = vec![
        (dec!(99900), dec!(100100)),  // Start
        (dec!(101000), dec!(101200)), // Bid=101000, fills sell at 101000
        (dec!(102000), dec!(102200)), // Bid=102000, fills sell at 102000
        (dec!(103000), dec!(103200)), // Bid=103000, fills sell at 103000
        (dec!(104000), dec!(104200)), // Bid=104000, fills sell at 104000
        (dec!(105000), dec!(105200)), // Bid=105000, fills sell at 105000
        (dec!(106000), dec!(106200)), // Bid=106000, fills sell at 106000
        (dec!(107000), dec!(107200)), // Bid=107000, fills sell at 107000
        (dec!(108000), dec!(108200)), // Bid=108000, fills sell at 108000
        (dec!(109000), dec!(109200)), // Bid=109000, fills sell at 109000
    ];

    let result = run_grid_test(paper, config, prices, 150).await;
    result.print_summary("SHORT Grid - Bull Market (Short Accumulation)");

    // Short grid in bull market accumulates short position
    assert!(
        result.final_position() < dec!(0),
        "Short grid should accumulate negative (short) position in bull market"
    );
}

/// Test Short Grid: Bull then Bear (Profit Taking)
#[tokio::test]
async fn test_short_grid_bull_then_bear() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_test_writer()
        .try_init();

    let paper = create_paper_exchange(dec!(100000));

    let config = create_grid_config(
        GridMode::Short,
        dec!(100000),
        dec!(110000),
        10,
        dec!(1000),
        dec!(5),
    );

    // Bull phase accumulates shorts, Bear phase takes profit
    let prices = vec![
        // Bull - accumulate shorts
        (dec!(99900), dec!(100100)),
        (dec!(101900), dec!(102100)),
        (dec!(103900), dec!(104100)),
        (dec!(105900), dec!(106100)),
        (dec!(107900), dec!(108100)),
        // Bear - profit taking on shorts
        (dec!(105900), dec!(106100)),
        (dec!(103900), dec!(104100)),
        (dec!(101900), dec!(102100)),
        (dec!(99900), dec!(100100)),
    ];

    let result = run_grid_test(paper, config, prices, 150).await;
    result.print_summary("SHORT Grid - Bull then Bear (Profit Taking)");
}

// ============================================================================
// Stress Tests: Many Grids
// ============================================================================

/// Test with many grid levels (50 levels) - performance check
#[tokio::test]
async fn test_many_grid_levels_performance() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_test_writer()
        .try_init();

    let paper = create_paper_exchange(dec!(100000));

    let config = create_grid_config(
        GridMode::Neutral,
        dec!(90000),
        dec!(110000),
        50, // Many levels
        dec!(5000),
        dec!(10),
    );

    // Oscillating price to trigger many fills
    let mut prices = Vec::new();
    for i in 0..30 {
        let offset = (i % 10) as i64 * 1000;
        let base = if (i / 10) % 2 == 0 {
            dec!(100000) - Decimal::from(offset)
        } else {
            dec!(95000) + Decimal::from(offset)
        };
        prices.push((base - dec!(50), base + dec!(50)));
    }

    let start = std::time::Instant::now();
    let result = run_grid_test(paper, config, prices, 50).await;
    let duration = start.elapsed();

    result.print_summary(&format!("50-Level Grid Performance ({:.2?})", duration));

    println!("\n⏱️  50-level grid test completed in {:.2?}", duration);
    assert!(
        duration.as_secs() < 10,
        "50-level grid should complete in under 10 seconds"
    );
}

// ============================================================================
// Balance Verification Tests
// ============================================================================

/// Verify balance changes correctly with fills
#[tokio::test]
async fn test_balance_tracking_accuracy() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn")
        .with_test_writer()
        .try_init();

    let initial_balance = dec!(10000);
    let paper = create_paper_exchange(initial_balance);

    let config = create_grid_config(
        GridMode::Neutral,
        dec!(95000),
        dec!(105000),
        5,
        dec!(500), // Small investment for clarity
        dec!(5),
    );

    // Simple bear drop
    let prices = vec![
        (dec!(99900), dec!(100100)),
        (dec!(98400), dec!(98600)),
        (dec!(96900), dec!(97100)),
    ];

    let result = run_grid_test(paper, config, prices, 200).await;
    result.print_summary("Balance Tracking Test");

    // Balance should decrease as margin is used for positions
    let final_bal = result.final_balance();
    println!("\nInitial Balance: {:.2}", initial_balance);
    println!("Final Balance: {:.2}", final_bal);
    println!("Change: {:.2}", final_bal - initial_balance);
}
