//! WASM API for browser-based backtesting.
//!
//! Exports a unified `run_backtest()` function that accepts V2 BotConfig in JSON format.
//! Supports all strategy types (Grid, DCA, Market Maker, Arbitrage) with zero strategy-specific code.

use crate::testing::{create_standalone_paper_exchange_with_id, ArcExchange};
use crate::{
    build_instrument_meta, build_strategy, config::BotConfig, Engine, EngineConfig, EngineRunner,
    RunnerConfig,
};
use bot_core::{
    AssetId, Exchange, ExchangeId, ExchangeInstance, InstrumentId, InstrumentKind, InstrumentMeta,
    Market, MarketIndex, Price, Qty, Quote,
};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

// Enable panic hook for better error messages in browser console
#[wasm_bindgen(start)]
pub fn main() {
    console_error_panic_hook::set_once();
}

/// Price point from frontend (ts_ms + price string)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsPricePoint {
    pub ts_ms: i64,
    pub price: String,
}

/// Get WASM version
#[wasm_bindgen]
pub fn get_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Run backtest with V2 BotConfig and historical prices.
/// Returns JSON string with backtest results.
///
/// # Arguments
/// * `prices_json` - JSON array of `{ts_ms, price}` objects
/// * `config_json` - V2 BotConfig JSON (supports Grid/DCA/MM/Arb)
#[wasm_bindgen]
pub fn run_backtest(prices_json: String, config_json: String) -> js_sys::Promise {
    future_to_promise(async move {
        web_sys::console::log_1(&"[WASM Runner] run_backtest started".into());

        // Parse config and prices
        web_sys::console::log_1(&"[WASM Runner] Parsing config...".into());
        let config: BotConfig = serde_json::from_str(&config_json)
            .map_err(|e| JsValue::from_str(&format!("Failed to parse config: {}", e)))?;

        web_sys::console::log_1(&"[WASM Runner] Parsing prices...".into());
        let prices: Vec<JsPricePoint> = serde_json::from_str(&prices_json)
            .map_err(|e| JsValue::from_str(&format!("Failed to parse prices: {}", e)))?;
        web_sys::console::log_1(&format!("[WASM Runner] Parsed {} prices", prices.len()).into());

        // Build strategy using shared builder (works for Grid, DCA, MM, Arb)
        web_sys::console::log_1(&"[WASM Runner] Building strategy...".into());
        let strategy = build_strategy(&config)
            .map_err(|e| JsValue::from_str(&format!("Failed to build strategy: {}", e)))?;
        web_sys::console::log_1(&"[WASM Runner] Strategy built successfully".into());

        // Build instrument meta
        web_sys::console::log_1(&"[WASM Runner] Building instrument meta...".into());
        let instrument_meta = build_instrument_meta(&config);
        let instrument_id = config.instrument_id();

        // Parse environment
        let environment = config.parse_environment();
        web_sys::console::log_1(&format!("[WASM Runner] Environment: {:?}", environment).into());

        // Use strategy allocation as the metrics base, matching native backtests.
        // The exchange balance is only simulation margin; the benchmark equity
        // should not be diluted by a fixed fake wallet bankroll.
        let sim_config = config.effective_simulation_config();
        let simulation_balance =
            Decimal::from_str(&sim_config.starting_balance_usdc).unwrap_or(Decimal::new(10_000, 0));
        let metrics_starting_balance = config
            .strategy_allocated_capital_usdc()
            .unwrap_or(simulation_balance);
        let exchange_starting_balance = if simulation_balance > metrics_starting_balance {
            simulation_balance
        } else {
            metrics_starting_balance
        };

        web_sys::console::log_1(
            &format!(
                "[WASM Runner] Metrics capital: {} USDC, exchange balance: {} USDC",
                metrics_starting_balance, exchange_starting_balance
            )
            .into(),
        );

        // Create paper exchange with initial balance
        let mut initial_balances = HashMap::new();
        initial_balances.insert(AssetId::new("USDC"), exchange_starting_balance);

        let paper = Arc::new(create_standalone_paper_exchange_with_id(
            initial_balances,
            "hyperliquid",
            environment,
        ));

        let fee_rate = Decimal::from_str(&sim_config.fee_rate).unwrap_or(dec!(0.00025));
        paper.set_fee_rate(fee_rate).await;

        // Convert prices to quotes
        let spread = dec!(1.0); // $1 spread
        let quotes: Vec<Quote> = prices
            .iter()
            .filter_map(|p| {
                let price = Decimal::from_str(&p.price).ok()?;
                let half_spread = spread / dec!(2);
                Some(Quote {
                    instrument: instrument_id.clone(),
                    bid: Price::new(price - half_spread),
                    ask: Price::new(price + half_spread),
                    bid_size: Qty::new(dec!(1000)),
                    ask_size: Qty::new(dec!(1000)),
                    ts: p.ts_ms,
                })
            })
            .collect();

        // Queue quotes in paper exchange
        web_sys::console::log_1(
            &format!("[WASM Runner] Queueing {} quotes...", quotes.len()).into(),
        );
        paper.queue_quotes(quotes).await;
        web_sys::console::log_1(&"[WASM Runner] Quotes queued".into());

        let exchange = paper.clone() as Arc<dyn Exchange>;

        // Create engine with default config
        web_sys::console::log_1(&"[WASM Runner] Creating engine...".into());
        let mut engine = Engine::new(EngineConfig::default());
        engine.register_exchange(exchange.clone());
        engine.register_instrument(instrument_meta.clone());
        engine.register_strategy(strategy);

        // Create runner with backtest-optimized config (no delays)
        web_sys::console::log_1(&"[WASM Runner] Creating runner...".into());
        let runner_config = RunnerConfig {
            min_poll_delay_ms: 0, // No delay between polls for backtesting
            cleanup_delay_ms: 0,  // No cleanup delay for backtesting
            metrics_mode: "backtest".to_string(),
            metrics_starting_balance_usdc: Some(metrics_starting_balance),
            ..RunnerConfig::default()
        };
        let mut runner = EngineRunner::new(engine, runner_config);
        runner.add_exchange(exchange);
        runner.add_instrument(instrument_id.clone());

        // Run the backtest
        web_sys::console::log_1(&"[WASM Runner] Starting runner.run()...".into());
        runner.run().await;
        web_sys::console::log_1(&"[WASM Runner] runner.run() completed".into());

        // Get backtest results
        let results = runner.get_backtest_results(&instrument_id);

        // Convert to JSON
        let result_json = serde_json::to_string(&results)
            .map_err(|e| JsValue::from_str(&format!("Failed to serialize result: {}", e)))?;

        Ok(JsValue::from_str(&result_json))
    })
}
