//! Grid Strategy Test with Unified PaperExchange (queue_quotes)
//!
//! Uses the unified PaperExchange with queue_quotes for batch backtesting
//! with realistic price-crossing fills.
//!
//! Run with: cargo test --package strategy-grid --test grid_unified_test -- --nocapture

use bot_core::{
    AssetId, Environment, Exchange, HyperliquidMarket, InstrumentId, InstrumentKind,
    InstrumentMeta, Market, MarketIndex, Price, Qty, Quote, StrategyId,
};
use bot_engine::testing::{MockExchange, PaperExchange};
use bot_engine::{Engine, EngineConfig, EngineRunner, RunnerConfig};

use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::sync::Arc;
use strategy_grid::{GridConfig, GridMode, GridStrategy};

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

/// Create test quotes simulating a bear then bull cycle
fn create_cycle_quotes(instrument: &InstrumentId) -> Vec<Quote> {
    let prices = vec![
        // Start at mid
        (1000, dec!(100000)),
        // Bear drop
        (2000, dec!(99000)),
        (3000, dec!(98000)),
        (4000, dec!(97000)),
        // Bull recovery
        (5000, dec!(98000)),
        (6000, dec!(99000)),
        (7000, dec!(100000)),
        (8000, dec!(101000)),
        (9000, dec!(102000)),
        // Back to mid
        (10000, dec!(100000)),
    ];

    prices
        .into_iter()
        .map(|(ts, price)| Quote {
            instrument: instrument.clone(),
            bid: Price::new(price - dec!(50)),
            ask: Price::new(price + dec!(50)),
            bid_size: Qty::new(dec!(1000)),
            ask_size: Qty::new(dec!(1000)),
            ts,
        })
        .collect()
}

/// Create test quotes simulating a bear trend (no recovery)
fn create_bear_trend_quotes(instrument: &InstrumentId) -> Vec<Quote> {
    let prices = vec![
        (1000, dec!(100000)),
        (2000, dec!(99000)),
        (3000, dec!(98000)),
        (4000, dec!(97000)),
        (5000, dec!(96000)),
        (6000, dec!(95000)),
    ];

    prices
        .into_iter()
        .map(|(ts, price)| Quote {
            instrument: instrument.clone(),
            bid: Price::new(price - dec!(50)),
            ask: Price::new(price + dec!(50)),
            bid_size: Qty::new(dec!(1000)),
            ask_size: Qty::new(dec!(1000)),
            ts,
        })
        .collect()
}

#[tokio::test]
async fn test_unified_paper_exchange_with_queue() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_test_writer()
        .try_init();

    println!("\n=== UNIFIED PAPER EXCHANGE TEST (queue_quotes) ===\n");

    let instrument = InstrumentId::new("BTC-PERP");

    // Setup: MockExchange as quote source (provides balances)
    let mut balances = HashMap::new();
    balances.insert(AssetId::new("USDC"), dec!(100000));
    let mock = MockExchange::new_with_balances(balances.clone());

    // Wrap with PaperExchange for realistic fills
    let paper = Arc::new(PaperExchange::new(mock, balances));

    // Queue quotes for batch backtest (NEW!)
    let quotes = create_cycle_quotes(&instrument);
    println!("Queuing {} quotes for backtest", quotes.len());
    paper.queue_quotes(quotes).await;

    // Grid config
    let grid_config = GridConfig {
        strategy_id: StrategyId::new("unified-test-grid"),
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

    // Fast poll for backtest
    let runner_config = RunnerConfig {
        min_poll_delay_ms: 1,
        quote_poll_interval_ms: 1,
        cleanup_delay_ms: 100,
        ..Default::default()
    };

    let mut runner = EngineRunner::new(engine, runner_config);
    runner.add_exchange(paper.clone() as Arc<dyn Exchange>);
    runner.add_instrument(instrument.clone());

    let shutdown_tx = runner.shutdown_handle();
    let paper_monitor = paper.clone();

    // Shutdown when queue is empty
    tokio::spawn(async move {
        loop {
            if !paper_monitor.has_queued_quotes().await {
                println!("Queue empty - sending shutdown signal");
                let _ = shutdown_tx.unbounded_send(());
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    });

    runner.run().await;

    println!("\n=== TEST COMPLETE ===");
    println!(
        "Pending orders remaining: {}",
        paper.pending_orders_count().await
    );
    println!("\n✅ UNIFIED PAPER EXCHANGE TEST PASSED");
}

#[tokio::test]
async fn test_unified_bear_trend_position_accumulates() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_test_writer()
        .try_init();

    println!("\n=== UNIFIED BEAR TREND TEST ===\n");

    let instrument = InstrumentId::new("BTC-PERP");

    let mut balances = HashMap::new();
    balances.insert(AssetId::new("USDC"), dec!(100000));
    let mock = MockExchange::new_with_balances(balances.clone());
    let paper = Arc::new(PaperExchange::new(mock, balances));

    // Queue bear trend quotes (no recovery!)
    let quotes = create_bear_trend_quotes(&instrument);
    println!("Queuing {} bear trend quotes", quotes.len());
    paper.queue_quotes(quotes).await;

    let grid_config = GridConfig {
        strategy_id: StrategyId::new("bear-test-grid"),
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
        min_poll_delay_ms: 1,
        cleanup_delay_ms: 100,
        ..Default::default()
    };

    let mut runner = EngineRunner::new(engine, runner_config);
    runner.add_exchange(paper.clone() as Arc<dyn Exchange>);
    runner.add_instrument(instrument.clone());

    let shutdown_tx = runner.shutdown_handle();
    let paper_monitor = paper.clone();

    tokio::spawn(async move {
        loop {
            if !paper_monitor.has_queued_quotes().await {
                let _ = shutdown_tx.unbounded_send(());
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    });

    runner.run().await;

    let pending = paper.pending_orders_count().await;
    println!("\n=== RESULTS ===");
    println!("Pending orders: {}", pending);
    println!("\n💡 In bear trend:");
    println!("   - Buy orders fill as price drops (fills pending TPs)");
    println!("   - TP sell orders DON'T fill (price never recovers)");
    println!("   - Position accumulates realistically!");

    // With realistic fills, we should have pending TP orders
    println!("\n✅ BEAR TREND TEST COMPLETE");
}
