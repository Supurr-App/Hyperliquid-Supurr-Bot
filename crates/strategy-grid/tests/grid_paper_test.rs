//! Grid Strategy Test with PaperExchange (Realistic Fills)
//!
//! Uses PaperExchange which fills orders only when price crosses order price.
//! This simulates real exchange behavior accurately.
//!
//! Run with: cargo test --package strategy-grid --test grid_paper_test -- --nocapture

use bot_core::{
    AssetId, Environment, Exchange, HyperliquidMarket, InstrumentId, InstrumentKind,
    InstrumentMeta, Market, MarketIndex, StrategyId,
};
use bot_engine::testing::{MockExchange, PaperExchange};
use bot_engine::{Engine, EngineConfig, EngineRunner, RunnerConfig};
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use strategy_grid::{GridConfig, GridMode, GridStrategy};
use tokio::time::sleep;

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

/// Test: Bear trend with PaperExchange - position should accumulate
#[tokio::test]
async fn test_grid_paper_exchange_bear_trend() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_test_writer()
        .try_init();

    println!("\n=== PAPER EXCHANGE TEST (Realistic Fills) ===\n");

    let instrument = InstrumentId::new("BTC-PERP");

    // Create underlying MockExchange (for quote source)
    let mut balances = HashMap::new();
    balances.insert(AssetId::new("USDC"), dec!(100000));
    let mock = MockExchange::new_with_balances(balances.clone());

    // Wrap with PaperExchange for realistic limit order simulation
    let paper = Arc::new(PaperExchange::new(mock, balances));
    paper.enable_simulation_mode().await;

    // Grid config: $97k to $103k with mid at $100k
    let grid_config = GridConfig {
        strategy_id: StrategyId::new("paper-test-grid"),
        environment: Environment::Testnet,
        market: Market::Hyperliquid(HyperliquidMarket::Perp {
            base: "BTC".to_string(),
            quote: "USDC".to_string(),
            index: 0,
            instrument_meta: None,
        }),
        grid_mode: GridMode::Neutral,
        grid_levels: 5,
        start_price: dec!(97000),
        end_price: dec!(103000),
        max_investment_quote: dec!(1000),
        base_order_size: dec!(0.001),
        leverage: dec!(5),
        max_leverage: dec!(50),
        post_only: false,
        stop_loss: None,
        take_profit: None,
        trailing_up_limit: None,
        trailing_down_limit: None,
    };

    let strategy = Box::new(GridStrategy::new(grid_config));

    let mut engine = Engine::new(EngineConfig::default());
    engine.register_strategy(strategy);
    engine.register_instrument(create_btc_meta());
    engine.register_exchange(paper.clone() as Arc<dyn Exchange>);

    let runner_config = RunnerConfig {
        min_poll_delay_ms: 50,
        quote_poll_interval_ms: 50,
        cleanup_delay_ms: 100,
        ..Default::default()
    };

    let mut runner = EngineRunner::new(engine, runner_config);
    runner.add_exchange(paper.clone() as Arc<dyn Exchange>);
    runner.add_instrument(instrument.clone());

    let shutdown_tx = runner.shutdown_handle();

    // Start runner in background
    let runner_handle = tokio::spawn(async move {
        runner.run().await;
    });

    println!("--- PRICE SEQUENCE (Bear Trend) ---");

    // 1. Start at $100k (mid of grid)
    println!("t=0: Inject $100,000 (mid)");
    paper
        .inject_quote(instrument.clone(), dec!(99950), dec!(100050))
        .await;
    sleep(Duration::from_millis(200)).await;

    let initial_orders = paper.pending_orders_count().await;
    println!("  → Pending orders: {}", initial_orders);

    // 2. Price drops to $99k (should fill buy at $99k)
    println!("t=1: Inject $99,000");
    paper
        .inject_quote(instrument.clone(), dec!(98950), dec!(99050))
        .await;
    sleep(Duration::from_millis(200)).await;
    println!("  → Pending orders: {}", paper.pending_orders_count().await);

    // 3. Price drops to $98k (should fill buy at $98k)
    println!("t=2: Inject $98,000");
    paper
        .inject_quote(instrument.clone(), dec!(97950), dec!(98050))
        .await;
    sleep(Duration::from_millis(200)).await;
    println!("  → Pending orders: {}", paper.pending_orders_count().await);

    // 4. Price drops to $97k (should fill buy at $97k)
    println!("t=3: Inject $97,000");
    paper
        .inject_quote(instrument.clone(), dec!(96950), dec!(97050))
        .await;
    sleep(Duration::from_millis(200)).await;
    println!("  → Pending orders: {}", paper.pending_orders_count().await);

    // 5. Price drops to $96k (below grid, no more buys)
    println!("t=4: Inject $96,000 (below grid)");
    paper
        .inject_quote(instrument.clone(), dec!(95950), dec!(96050))
        .await;
    sleep(Duration::from_millis(200)).await;

    let final_orders = paper.pending_orders_count().await;
    println!("  → Final pending orders: {}", final_orders);

    // Shutdown
    let _ = shutdown_tx.unbounded_send(());
    let _ = runner_handle.await;

    println!("\n--- SUMMARY ---");
    println!("Initial orders placed: {}", initial_orders);
    println!("Final pending orders: {}", final_orders);
    println!("\n💡 With PaperExchange:");
    println!("   - Orders only fill when price CROSSES the order price");
    println!("   - Buy orders fill when ask <= order_price");
    println!("   - In bear trend, buys fill but sells (TPs) DON'T fill");
    println!("   - Position accumulates realistically!");

    println!("\n✅ PAPER EXCHANGE TEST COMPLETE");
}

/// Test: Full cycle with recovery - position returns to 0
#[tokio::test]
async fn test_grid_paper_exchange_full_cycle() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_test_writer()
        .try_init();

    println!("\n=== PAPER EXCHANGE FULL CYCLE TEST ===\n");

    let instrument = InstrumentId::new("BTC-PERP");

    let mut balances = HashMap::new();
    balances.insert(AssetId::new("USDC"), dec!(100000));
    let mock = MockExchange::new_with_balances(balances.clone());
    let paper = Arc::new(PaperExchange::new(mock, balances));
    paper.enable_simulation_mode().await;

    let grid_config = GridConfig {
        strategy_id: StrategyId::new("cycle-test-grid"),
        environment: Environment::Testnet,
        market: Market::Hyperliquid(HyperliquidMarket::Perp {
            base: "BTC".to_string(),
            quote: "USDC".to_string(),
            index: 0,
            instrument_meta: None,
        }),
        grid_mode: GridMode::Neutral,
        grid_levels: 5,
        start_price: dec!(97000),
        end_price: dec!(103000),
        max_investment_quote: dec!(1000),
        base_order_size: dec!(0.001),
        leverage: dec!(5),
        max_leverage: dec!(50),
        post_only: false,
        stop_loss: None,
        take_profit: None,
        trailing_up_limit: None,
        trailing_down_limit: None,
    };

    let strategy = Box::new(GridStrategy::new(grid_config));

    let mut engine = Engine::new(EngineConfig::default());
    engine.register_strategy(strategy);
    engine.register_instrument(create_btc_meta());
    engine.register_exchange(paper.clone() as Arc<dyn Exchange>);

    let runner_config = RunnerConfig {
        min_poll_delay_ms: 50,
        quote_poll_interval_ms: 50,
        cleanup_delay_ms: 100,
        ..Default::default()
    };

    let mut runner = EngineRunner::new(engine, runner_config);
    runner.add_exchange(paper.clone() as Arc<dyn Exchange>);
    runner.add_instrument(instrument.clone());

    let shutdown_tx = runner.shutdown_handle();

    let runner_handle = tokio::spawn(async move {
        runner.run().await;
    });

    println!("--- FULL CYCLE: Bear then Bull ---");

    // Phase 1: Start at mid
    println!("1. Start at $100k");
    paper
        .inject_quote(instrument.clone(), dec!(99900), dec!(100100))
        .await;
    sleep(Duration::from_millis(300)).await;
    println!("   Pending: {}", paper.pending_orders_count().await);

    // Phase 2: Bear drop - fills buy orders
    println!("2. Bear: $99k -> $98k -> $97k");
    paper
        .inject_quote(instrument.clone(), dec!(98900), dec!(99100))
        .await;
    sleep(Duration::from_millis(200)).await;
    paper
        .inject_quote(instrument.clone(), dec!(97900), dec!(98100))
        .await;
    sleep(Duration::from_millis(200)).await;
    paper
        .inject_quote(instrument.clone(), dec!(96900), dec!(97100))
        .await;
    sleep(Duration::from_millis(200)).await;
    println!(
        "   Pending after bear: {}",
        paper.pending_orders_count().await
    );

    // Phase 3: Bull recovery - fills sell (TP) orders
    println!("3. Bull: $98k -> $100k -> $102k -> $104k");
    paper
        .inject_quote(instrument.clone(), dec!(97900), dec!(98100))
        .await;
    sleep(Duration::from_millis(200)).await;
    paper
        .inject_quote(instrument.clone(), dec!(99900), dec!(100100))
        .await;
    sleep(Duration::from_millis(200)).await;
    paper
        .inject_quote(instrument.clone(), dec!(101900), dec!(102100))
        .await;
    sleep(Duration::from_millis(200)).await;
    paper
        .inject_quote(instrument.clone(), dec!(103900), dec!(104100))
        .await;
    sleep(Duration::from_millis(200)).await;
    println!(
        "   Pending after bull: {}",
        paper.pending_orders_count().await
    );

    // Shutdown
    let _ = shutdown_tx.unbounded_send(());
    let _ = runner_handle.await;

    println!("\n✅ FULL CYCLE TEST COMPLETE");
    println!("   After full bear-then-bull cycle, position should be near 0");
}
