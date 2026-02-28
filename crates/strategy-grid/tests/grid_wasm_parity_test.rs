//! Grid Strategy WASM Parity Test
//!
//! This test uses the EXACT same pattern as wasm_api.rs to ensure
//! identical behavior between native and WASM backtests.
//!
//! Run with: cargo test --package strategy-grid --test grid_wasm_parity_test -- --nocapture

use bot_core::{
    AssetId, Environment, Exchange, HyperliquidMarket, InstrumentId, InstrumentKind,
    InstrumentMeta, Market, MarketIndex, Price, Qty, Quote, StrategyId,
};
use bot_engine::testing::MockExchange;
use bot_engine::{Engine, EngineConfig, EngineRunner, RunnerConfig};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::sync::Arc;
use strategy_grid::{GridConfig, GridMode, GridStrategy};

/// Create instrument meta matching WASM setup
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

/// Simulate price data similar to what comes from price server
fn create_test_quotes(instrument: &InstrumentId, spread: Decimal) -> Vec<Quote> {
    // Simulate a price range: $99k -> $98k -> $100k -> $101k
    let prices = vec![
        (1000, dec!(99000)),  // Start
        (2000, dec!(98500)),  // Drop
        (3000, dec!(98000)),  // Further drop (fill buy orders)
        (4000, dec!(99000)),  // Recover
        (5000, dec!(100000)), // Mid
        (6000, dec!(101000)), // Rise (fill sell orders)
        (7000, dec!(101500)), // Further rise
        (8000, dec!(100500)), // Settle
    ];

    prices
        .into_iter()
        .map(|(ts_ms, price)| {
            let half_spread = spread / dec!(2);
            Quote {
                instrument: instrument.clone(),
                bid: Price::new(price - half_spread),
                ask: Price::new(price + half_spread),
                bid_size: Qty::new(dec!(1000)),
                ask_size: Qty::new(dec!(1000)),
                ts: ts_ms,
            }
        })
        .collect()
}

#[tokio::test]
async fn test_grid_wasm_parity_simple() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_test_writer()
        .try_init();

    println!("\n=== WASM PARITY TEST ===\n");

    // Setup instrument
    let instrument = InstrumentId::new("BTC-PERP");
    let spread = dec!(1); // $1 spread

    // Create MockExchange (same as WASM)
    let mut balances = HashMap::new();
    balances.insert(AssetId::new("USDC"), dec!(100000));
    let mock_exchange = Arc::new(MockExchange::new_with_balances(balances));

    // Create quotes and queue them (same as WASM)
    let quotes = create_test_quotes(&instrument, spread);
    println!("Queued {} quotes", quotes.len());
    for q in &quotes {
        println!("  ts={} bid={} ask={}", q.ts, q.bid.0, q.ask.0);
    }
    mock_exchange.queue_quotes(quotes).await;

    // Enable immediate fill mode (same as WASM!)
    mock_exchange.set_fill_all_immediately(true).await;

    // Create grid config (match WASM test)
    let grid_config = GridConfig {
        strategy_id: StrategyId::new("test-grid"),
        environment: Environment::Testnet,
        market: Market::Hyperliquid(HyperliquidMarket::Perp {
            base: "BTC".to_string(),
            quote: "USDC".to_string(),
            index: 0,
            instrument_meta: None,
        }),
        grid_mode: GridMode::Neutral,
        grid_levels: 5,
        start_price: dec!(97000), // Grid lower bound
        end_price: dec!(103000),  // Grid upper bound
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

    // Validate config
    let errors = grid_config.validate();
    if !errors.is_empty() {
        panic!("Invalid grid config: {:?}", errors);
    }

    // Create strategy
    let strategy = Box::new(GridStrategy::new(grid_config));

    // Create engine (same as WASM)
    let mut engine = Engine::new(EngineConfig::default());
    engine.register_strategy(strategy);
    engine.register_instrument(create_btc_meta());

    // Create runner with FAST poll delay (same as WASM!)
    let runner_config = RunnerConfig {
        min_poll_delay_ms: 1, // <-- KEY: same as WASM!
        ..Default::default()
    };

    let mut runner = EngineRunner::new(engine, runner_config);
    runner.add_exchange(mock_exchange.clone() as Arc<dyn Exchange>);
    runner.add_instrument(instrument.clone());

    // Get shutdown handle
    let shutdown_tx = runner.shutdown_handle();

    // Clone for monitoring
    let mock_monitor = mock_exchange.clone();

    // Spawn shutdown monitor (same pattern as WASM)
    tokio::spawn(async move {
        loop {
            if !mock_monitor.has_queued_quotes().await {
                let _ = shutdown_tx.unbounded_send(());
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    });

    // Run the engine
    runner.run().await;

    // Collect results (same as WASM)
    let fills = mock_exchange.fills().await;

    println!("\n=== RESULTS (with position tracking) ===");
    println!("Total fills: {}", fills.len());
    println!(
        "\n{:<6} {:<6} {:>12} {:>12} {:>15}",
        "Fill#", "Side", "Qty", "Price", "Position After"
    );
    println!("{}", "-".repeat(55));

    let mut total_volume = dec!(0);
    let mut position_qty = dec!(0);

    for (i, fill) in fills.iter().enumerate() {
        let notional = fill.price.0 * fill.qty.0;
        total_volume += notional;

        let position_change = match fill.side {
            bot_core::OrderSide::Buy => {
                position_qty += fill.qty.0;
                format!("+{}", fill.qty.0)
            }
            bot_core::OrderSide::Sell => {
                position_qty -= fill.qty.0;
                format!("-{}", fill.qty.0)
            }
        };

        println!(
            "{:<6} {:<6} {:>12} {:>12} {:>15}",
            format!("#{}", i + 1),
            format!("{:?}", fill.side),
            position_change,
            format!("${}", fill.price.0),
            format!("{:.5}", position_qty)
        );
    }

    println!("{}", "-".repeat(55));
    println!("\nFinal position: {}", position_qty);
    println!("Total volume: ${}", total_volume);
    println!("Trade count: {}", fills.len());

    // Assertions
    assert!(fills.len() > 0, "Should have at least one fill");

    println!("\n✅ WASM PARITY TEST COMPLETE");
}

/// Test with specific prices from price server (copy actual data here)
#[tokio::test]
async fn test_grid_with_real_prices() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("debug")
        .with_test_writer()
        .try_init();

    println!("\n=== REAL PRICE DATA TEST ===\n");

    // Copy actual price data from the debug page here!
    // Example: [{"ts_ms": 1769632114000, "price": "89640.5"}, ...]
    let price_data = vec![
        (1769632114000_i64, "89640.5"),
        (1769632115000, "89650.0"),
        (1769632116000, "89600.0"),
        (1769632117000, "89550.0"),
        (1769632118000, "89700.0"),
        // Add more from your actual data...
    ];

    let instrument = InstrumentId::new("BTC-PERP");
    let spread = dec!(50); // Match WASM spread config

    let quotes: Vec<Quote> = price_data
        .iter()
        .map(|(ts_ms, price_str)| {
            let price: Decimal = price_str.parse().unwrap();
            let half_spread = spread / dec!(2);
            Quote {
                instrument: instrument.clone(),
                bid: Price::new(price - half_spread),
                ask: Price::new(price + half_spread),
                bid_size: Qty::new(dec!(1000)),
                ask_size: Qty::new(dec!(1000)),
                ts: *ts_ms,
            }
        })
        .collect();

    println!("Created {} quotes from price data", quotes.len());

    // Calculate grid bounds from prices (match WASM logic)
    let prices: Vec<Decimal> = price_data.iter().map(|(_, p)| p.parse().unwrap()).collect();
    let min_price = prices.iter().cloned().min().unwrap();
    let max_price = prices.iter().cloned().max().unwrap();
    let mid_price = (min_price + max_price) / dec!(2);

    let start_price = mid_price * dec!(0.95);
    let end_price = mid_price * dec!(1.05);

    println!("Price range: ${} - ${}", min_price, max_price);
    println!("Grid range: ${} - ${}", start_price, end_price);

    // Setup MockExchange
    let mut balances = HashMap::new();
    balances.insert(AssetId::new("USDC"), dec!(100000));
    let mock_exchange = Arc::new(MockExchange::new_with_balances(balances));

    mock_exchange.queue_quotes(quotes).await;
    mock_exchange.set_fill_all_immediately(true).await;

    let grid_config = GridConfig {
        strategy_id: StrategyId::new("real-test-grid"),
        environment: Environment::Testnet,
        market: Market::Hyperliquid(HyperliquidMarket::Perp {
            base: "BTC".to_string(),
            quote: "USDC".to_string(),
            index: 0,
            instrument_meta: None,
        }),
        grid_mode: GridMode::Neutral,
        grid_levels: 5,
        start_price,
        end_price,
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

    let runner_config = RunnerConfig {
        min_poll_delay_ms: 1,
        ..Default::default()
    };

    let mut runner = EngineRunner::new(engine, runner_config);
    runner.add_exchange(mock_exchange.clone() as Arc<dyn Exchange>);
    runner.add_instrument(instrument.clone());

    let shutdown_tx = runner.shutdown_handle();
    let mock_monitor = mock_exchange.clone();

    tokio::spawn(async move {
        loop {
            if !mock_monitor.has_queued_quotes().await {
                let _ = shutdown_tx.unbounded_send(());
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    });

    runner.run().await;

    let fills = mock_exchange.fills().await;
    println!("\n=== RESULTS ===");
    println!("Trade count: {}", fills.len());

    for fill in &fills {
        println!("  {:?} {} @ {}", fill.side, fill.qty.0, fill.price.0);
    }

    println!("\n✅ REAL PRICE DATA TEST COMPLETE");
}

/// Test: Trending market - position accumulates (realistic scenario)
/// Price keeps dropping, buys pile up with no sells
#[tokio::test]
async fn test_grid_trending_market_position_accumulates() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_test_writer()
        .try_init();

    println!("\n=== TRENDING MARKET TEST (Position Accumulation) ===\n");

    let instrument = InstrumentId::new("BTC-PERP");

    // Simulate BEAR TREND: price keeps dropping (no recovery)
    let prices = vec![
        (1000, dec!(100000)), // Start at mid
        (2000, dec!(99500)),  // Drop
        (3000, dec!(99000)),  // Fill buy at 99k
        (4000, dec!(98500)),  //
        (5000, dec!(98000)),  // Fill buy at 98k
        (6000, dec!(97500)),  //
        (7000, dec!(97000)),  // Fill buy at 97k
        (8000, dec!(96500)),  // Below grid - no more buys
                              // NOTE: Price NEVER goes back up, so sells never fill
    ];

    let quotes: Vec<Quote> = prices
        .iter()
        .map(|(ts, price)| Quote {
            instrument: instrument.clone(),
            bid: Price::new(*price - dec!(0.5)),
            ask: Price::new(*price + dec!(0.5)),
            bid_size: Qty::new(dec!(1000)),
            ask_size: Qty::new(dec!(1000)),
            ts: *ts,
        })
        .collect();

    println!("Price path (bear trend):");
    for (ts, px) in &prices {
        println!("  t={}: ${}", ts, px);
    }

    // Use PaperExchange for realistic price-crossing fills
    // Unlike MockExchange with set_fill_all_immediately(true),
    // PaperExchange only fills orders when price CROSSES the order price
    let mut balances = HashMap::new();
    balances.insert(AssetId::new("USDC"), dec!(100000));
    let paper_exchange = Arc::new(bot_engine::testing::create_standalone_paper_exchange(
        balances,
    ));

    paper_exchange.queue_quotes(quotes).await;

    // Set realistic fee rate (0.04% = 4 bps) to verify fee tracking
    paper_exchange.set_fee_rate(dec!(0.0004)).await;

    let grid_config = GridConfig {
        strategy_id: StrategyId::new("trend-test-grid"),
        environment: Environment::Testnet,
        market: Market::Hyperliquid(HyperliquidMarket::Perp {
            base: "BTC".to_string(),
            quote: "USDC".to_string(),
            index: 0,
            instrument_meta: None,
        }),
        grid_mode: GridMode::Neutral,
        grid_levels: 5,
        start_price: dec!(97000), // Grid: 97k to 103k
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

    let runner_config = RunnerConfig {
        min_poll_delay_ms: 1,
        ..Default::default()
    };

    let mut runner = EngineRunner::new(engine, runner_config);
    runner.add_exchange(paper_exchange.clone() as Arc<dyn Exchange>);
    runner.add_instrument(instrument.clone());

    let shutdown_tx = runner.shutdown_handle();
    let paper_monitor = paper_exchange.clone();

    tokio::spawn(async move {
        loop {
            if !paper_monitor.has_queued_quotes().await {
                let _ = shutdown_tx.unbounded_send(());
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    });

    runner.run().await;

    // =========================================================================
    // Engine.position() is the SOURCE OF TRUTH for position/PnL tracking.
    // paper_exchange.fills() may contain fills that were generated but never
    // polled by the Engine (race condition at shutdown), so we don't use it.
    // =========================================================================
    let engine_position = runner.engine().position(&instrument);

    println!("\n=== ENGINE POSITION (Source of Truth) ===");
    println!("  Position qty:      {}", engine_position.qty);
    println!("  Avg entry price:   {:?}", engine_position.avg_entry_px);
    println!("  Realized PnL:      {}", engine_position.realized_pnl);
    println!("  Unrealized PnL:    {:?}", engine_position.unrealized_pnl);
    println!("  Total fees:        {}", engine_position.total_fees);

    // In a bear trend, we expect:
    // - Position > 0 (accumulated long from buy fills)
    // - No sells filled (price never recovered to TP levels)
    assert!(
        engine_position.qty > dec!(0),
        "Bear trend should result in positive (long) position"
    );

    // Position should be non-trivial (we had multiple buy fills)
    assert!(
        engine_position.qty >= dec!(0.01),
        "Expected at least 0.01 position from grid buys"
    );

    println!("\n💡 In trending markets, position accumulates because:");
    println!("   - Buy orders fill as price drops");
    println!("   - Sell orders (TPs) never fill because price doesn't recover");
    println!("\n✅ ENGINE POSITION TRACKING VERIFIED!");
    println!("   Engine.position() is now the authoritative source for tests and UI.");

    println!("\n✅ TRENDING MARKET TEST COMPLETE");
}
