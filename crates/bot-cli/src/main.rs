//! Bot CLI - entry point for running the trading bot.
//!
//! ## Configuration
//!
//! The bot can be configured via:
//! 1. Environment variables (loaded from `.env` file if present)
//! 2. A JSON config file (pass `--config path/to/config.json`)
//!
//! ### Required Environment Variables
//!
//! ```
//! HYPERLIQUID_PK=<your-private-key-hex-without-0x>
//! HYPERLIQUID_ADDRESS=<your-wallet-address>
//! ```
//!
//! ### Optional Environment Variables
//!
//! ```
//! ENVIRONMENT=testnet|mainnet          # default: testnet
//! INSTRUMENT=BTC-PERP                  # default: BTC-PERP
//! MARKET_INDEX=0                       # default: 0
//! BASE_ORDER_SIZE=0.001                # default: 0.001
//! BASE_SPREAD=0.001                    # default: 0.001 (0.1%)
//! MAX_POSITION_SIZE=0.1                # default: 0.1
//! SKEW_MODE=both|size|price|none       # default: both
//! PRICE_SKEW_GAMMA=0.05                # default: 0.05
//! SIZE_SKEW_FLOOR=0.2                  # default: 0.2
//! MIN_PRICE_CHANGE_PCT=0.0005          # default: 0.0005
//! STOP_LOSS=-100                       # optional
//! TAKE_PROFIT=100                      # optional
//! RUST_LOG=bot=debug                   # logging level
//! ```

use anyhow::{Context, Result};
use bot_core::{AssetId, InstrumentId, InstrumentMeta, Price, Qty, Quote};
use bot_engine::testing::{create_standalone_paper_exchange_with_id, ArcExchange, PaperExchange};
use bot_engine::{build_strategy, BotConfig, Engine, EngineConfig, TradeSyncerConfig};
use exchange_hyperliquid::{
    new_client_with_registration, BuilderFee, Hip3Config, HyperliquidConfig, OutcomeConfig,
};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

/// Trading mode: live, paper, or backtest
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TradingMode {
    Live,
    Paper,
    Backtest { prices_path: PathBuf },
}

impl Default for TradingMode {
    fn default() -> Self {
        Self::Live
    }
}

fn parse_args() -> (Option<PathBuf>, bool, TradingMode) {
    let args: Vec<String> = std::env::args().collect();
    let mut config_path = None;
    let mut dry_run = false;
    let mut mode = TradingMode::Live;
    let mut prices_path: Option<PathBuf> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--config" | "-c" => {
                if i + 1 < args.len() {
                    config_path = Some(PathBuf::from(&args[i + 1]));
                    i += 1;
                }
            }
            "--dry-run" | "-d" => {
                dry_run = true;
            }
            "--prices" | "-p" => {
                if i + 1 < args.len() {
                    prices_path = Some(PathBuf::from(&args[i + 1]));
                    i += 1;
                }
            }
            "--mode" | "-m" => {
                if i + 1 < args.len() {
                    match args[i + 1].to_lowercase().as_str() {
                        "paper" => mode = TradingMode::Paper,
                        "live" => mode = TradingMode::Live,
                        other => {
                            eprintln!("Warning: Unknown mode '{}', defaulting to 'live'", other);
                        }
                    }
                    i += 1;
                }
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            _ => {}
        }
        i += 1;
    }

    // If --prices was provided, override mode to Backtest
    if let Some(path) = prices_path {
        mode = TradingMode::Backtest { prices_path: path };
    }

    (config_path, dry_run, mode)
}

fn print_help() {
    println!(
        r#"
Trading Bot - Hyperliquid Market Maker & Grid Bot

USAGE:
    bot [OPTIONS]

OPTIONS:
    -c, --config <FILE>    Path to JSON config file
    -d, --dry-run          Print config and exit without trading
    -m, --mode <MODE>      Trading mode: 'live' (default) or 'paper'
    -h, --help             Print help information


STRATEGIES:
    Market Maker (default):  Set "strategy_type": "mm" in config
    Grid Bot:                Set "strategy_type": "grid" in config

CARGO ALIASES:
    cargo mm               Run market maker with config.json
    cargo mm-dry           Dry run market maker
    cargo grid             Run grid bot with config-grid.json
    cargo grid-dry         Dry run grid bot

EXAMPLE:
    # Market maker
    cargo mm
    
    # Grid bot
    cargo grid
    
    # Using custom config file
    cargo run --bin bot -- --config my-config.json

    # Dry run (validate config without trading)
    cargo grid-dry
"#
    );
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load .env file if present
    dotenvy::dotenv().ok();

    // Parse CLI arguments
    let (config_path, dry_run, trading_mode) = parse_args();

    // Initialize tracing (suppress noisy logs, use quieter level for backtest)
    let is_backtest = matches!(trading_mode, TradingMode::Backtest { .. });
    let log_level = if is_backtest { "warn" } else { "debug" };

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive(format!("bot={}", log_level).parse().unwrap())
                .add_directive(format!("bot_engine={}", log_level).parse().unwrap())
                .add_directive("hyper_util=warn".parse().unwrap())
                .add_directive("hyper=warn".parse().unwrap())
                .add_directive("reqwest=warn".parse().unwrap()),
        )
        .init();

    // Load configuration
    let mut config = if let Some(path) = config_path {
        tracing::info!("Loading config from: {:?}", path);
        BotConfig::from_file(&path)?
    } else {
        tracing::info!("Loading config from environment variables");
        BotConfig::from_env()?
    };

    // Resolve wallet credentials: config fields → ~/.supurr/credentials.json fallback
    let (resolved_pk, resolved_addr) = config.resolve_credentials()?;
    if config.private_key.is_empty() || config.address.is_empty() {
        tracing::info!(
            "Credentials resolved from ~/.supurr/credentials.json (address={})",
            &resolved_addr[..8.min(resolved_addr.len())]
        );
    }
    config.private_key = resolved_pk;
    config.address = resolved_addr;

    let environment = config.parse_environment();
    let strategy_type = config.strategy_type.to_lowercase();

    tracing::info!(
        "Environment: {} | Strategy: {} | Market: {} | Mode: {:?}",
        config.environment,
        strategy_type,
        config.instrument_id(),
        trading_mode
    );

    // Build strategy using shared build_strategy() from bot_engine
    let strategy_box = build_strategy(&config)?;

    // Log strategy info and handle dry-run
    tracing::info!(
        "{} strategy configured and validated successfully",
        strategy_type
    );

    if dry_run {
        tracing::info!("Dry run mode - exiting without trading");
        println!("\n✓ Configuration is valid. Remove --dry-run to start trading.");
        return Ok(());
    }

    // Configure Hyperliquid client
    let builder_fee = config.builder_fee.as_ref().map(|bf| BuilderFee {
        address: bf.address.clone(),
        fee_tenths_bp: bf.fee_tenths_bp,
    });

    if let Some(ref bf) = builder_fee {
        tracing::info!(
            "Builder fee configured: address={}, fee={} tenths-of-bp ({:.4}%)",
            bf.address,
            bf.fee_tenths_bp,
            bf.fee_tenths_bp as f64 / 1000.0
        );
    }

    // Build HIP-3 config from Market if it's a HIP-3 market
    let hip3 = config.hip3_config().map(|h| {
        tracing::info!(
            "HIP-3 configured: dex={}, index={}, quote={}, asset_index={}",
            h.dex_name,
            h.dex_index,
            h.quote_currency,
            h.asset_index
        );
        Hip3Config {
            dex_name: h.dex_name.clone(),
            dex_index: h.dex_index,
            quote_currency: h.quote_currency.clone(),
            asset_index: h.asset_index,
        }
    });

    // Determine spot_coin for alias resolution (from primary market)
    let spot_coin = config.primary_market().spot_coin();

    if let Some(ref coin) = spot_coin {
        tracing::info!("Spot coin configured: {} (for alias resolution)", coin);
    }
    // Build outcome config from market if applicable
    let outcome = config.primary_market().outcome_params().map(
        |(outcome_id, side, name)| OutcomeConfig {
            outcome_id,
            side,
            name,
        },
    );

    // #simplify, hip,spot,perps should be abstracted away
    let hl_config = HyperliquidConfig {
        environment,
        private_key: config.private_key.clone(),
        main_address: Some(config.address.clone()),
        vault_address: config.vault_address.clone(),
        timeout_secs: 10,
        proxy_url: None,
        base_url_override: None,
        builder_fee,
        hip3,
        is_spot: config.is_spot(),
        spot_coin,
        spot_market_index: config.primary_market().spot_market_index(),
        is_outcome: config.is_outcome(),
        outcome,
    };

    // Create exchange client with registration handle
    let (real_exchange, hl_client) = match new_client_with_registration(hl_config) {
        Ok(result) => result,
        Err(e) => {
            tracing::error!("Failed to create exchange client: {}", e);
            report_init_error(&config, &format!("init_failed:{}", e)).await;
            return Err(e.into());
        }
    };

    // Determine exchange based on trading mode
    // - Live: use real exchange directly
    // - Paper: wrap real exchange in PaperExchange for simulated fills (uses real quotes)
    // - Backtest: use standalone PaperExchange with pre-loaded historical prices
    let sim_config = config.effective_simulation_config();
    let (exchange, backtest_paper): (Arc<dyn bot_core::Exchange>, Option<Arc<PaperExchange<_>>>) =
        match &trading_mode {
            TradingMode::Paper => {
                tracing::info!("📄 PAPER TRADING MODE - orders will be simulated locally");
                let starting_balance = Decimal::from_str(&sim_config.starting_balance_usdc)
                    .unwrap_or(Decimal::new(10_000, 0));
                let fee_rate = Decimal::from_str(&sim_config.fee_rate)
                    .unwrap_or(dec!(0.00025));

                let mut initial_balances = std::collections::HashMap::new();
                initial_balances.insert(AssetId::new("USDC"), starting_balance);
                initial_balances.insert(AssetId::new("USDH"), starting_balance);
                let wrapped_exchange = ArcExchange::new(real_exchange);
                let paper = Arc::new(PaperExchange::new(wrapped_exchange, initial_balances));
                paper.set_fee_rate(fee_rate).await;

                tracing::info!(
                    "Paper exchange: balance={} USDC, fee_rate={}",
                    starting_balance, fee_rate
                );

                // Inject strategy leverage into MarginLedger
                if let Some((leverage, max_leverage)) = config.strategy_leverage() {
                    let instrument = config.instrument_id();
                    paper.set_instrument_leverage(&instrument, leverage, max_leverage).await;
                    tracing::info!(
                        "Paper leverage set: {}x (max {}x) for {}",
                        leverage, max_leverage, instrument
                    );
                }

                (paper.clone() as Arc<dyn bot_core::Exchange>, None)
            }
            TradingMode::Backtest { prices_path } => {
                tracing::info!(
                    "📊 BACKTEST MODE - running with historical prices from {:?}",
                    prices_path
                );

                let starting_balance = Decimal::from_str(&sim_config.starting_balance_usdc)
                    .unwrap_or(Decimal::new(10_000, 0));
                let fee_rate = Decimal::from_str(&sim_config.fee_rate)
                    .unwrap_or(dec!(0.00025));

                // Load prices from JSON file
                let prices_str = std::fs::read_to_string(prices_path)
                    .with_context(|| format!("Failed to read prices file: {:?}", prices_path))?;

                #[derive(serde::Deserialize)]
                struct PricePoint {
                    ts: i64,
                    price: String,
                }
                #[derive(serde::Deserialize)]
                struct PricesFile {
                    prices: Vec<PricePoint>,
                }

                let prices_file: PricesFile =
                    serde_json::from_str(&prices_str).context("Failed to parse prices JSON")?;

                tracing::info!("Loaded {} historical prices", prices_file.prices.len());

                // Create standalone paper exchange with "hyperliquid" ID for strategy compatibility
                let mut initial_balances = std::collections::HashMap::new();
                initial_balances.insert(AssetId::new("USDC"), starting_balance);
                let paper = Arc::new(create_standalone_paper_exchange_with_id(
                    initial_balances,
                    "hyperliquid",
                    config.parse_environment(),
                ));

                paper.set_fee_rate(fee_rate).await;

                tracing::info!(
                    "Backtest exchange: balance={} USDC, fee_rate={}",
                    starting_balance, fee_rate
                );

                // Inject strategy leverage into MarginLedger
                if let Some((leverage, max_leverage)) = config.strategy_leverage() {
                    let instrument = config.instrument_id();
                    paper.set_instrument_leverage(&instrument, leverage, max_leverage).await;
                    tracing::info!(
                        "Backtest leverage set: {}x (max {}x) for {}",
                        leverage, max_leverage, instrument
                    );
                }

                // Convert prices to quotes and queue them
                let instrument = config.instrument_id();
                let spread = dec!(1.0); // $1 spread for simulation
                let quotes: Vec<Quote> = prices_file
                    .prices
                    .iter()
                    .filter_map(|p| {
                        let price = Decimal::from_str(&p.price).ok()?;
                        let half_spread = spread / dec!(2);
                        Some(Quote {
                            instrument: instrument.clone(),
                            bid: Price::new(price - half_spread),
                            ask: Price::new(price + half_spread),
                            bid_size: Qty::new(dec!(1000)),
                            ask_size: Qty::new(dec!(1000)),
                            ts: p.ts,
                        })
                    })
                    .collect();

                tracing::info!("Queued {} quotes for backtest", quotes.len());
                paper.queue_quotes(quotes).await;

                (paper.clone() as Arc<dyn bot_core::Exchange>, Some(paper))
            }
            TradingMode::Live => (real_exchange, None),
        };

    // Note: register_user() is called later, after strategy is registered,
    // so we can check sync_mechanism()

    // Determine quote currency (USDC by default, or from HIP-3 config)
    let quote_currency = config
        .hip3_config()
        .map(|h| h.quote_currency.clone())
        .unwrap_or_else(|| "USDC".to_string());

    // Determine instrument kind based on config
    let instrument_kind = config.primary_market().instrument_kind();

    // Register instrument metadata (derived from primary market)
    let primary_market = config.primary_market();

    // Extract instrument_meta from Market, use defaults if not provided
    let (tick_size, lot_size, min_qty, min_notional) = primary_market
        .instrument_meta()
        .map(|im| (im.tick_size, im.lot_size, im.min_qty, im.min_notional))
        .unwrap_or((Decimal::new(1, 1), Decimal::new(1, 4), None, None));

    // # simplify, engine shouldn't care about Exchange's market things
    let instrument_meta = InstrumentMeta {
        instrument_id: config.instrument_id(),
        market_index: config.market_index(),
        base_asset: AssetId::new(primary_market.base()),
        quote_asset: AssetId::new(primary_market.quote()),
        tick_size,
        lot_size,
        min_qty,
        min_notional,
        fee_asset_default: Some(AssetId::new(&quote_currency)),
        kind: instrument_kind,
    };

    // Create engine
    let mut engine = Engine::new(EngineConfig::default());
    engine.register_exchange(exchange.clone());

    // Register instruments — multi-market strategies (arb) need all markets,
    // single-market strategies use the primary instrument.
    let instruments: Vec<InstrumentId> = if config.markets.len() > 1 {
        // Multi-market strategy: register every market in the array
        let mut ids = Vec::new();
        for market in &config.markets {
            let quote_currency = market.quote();
            let (tick_size, lot_size, min_qty, min_notional) = market
                .instrument_meta()
                .map(|im| (im.tick_size, im.lot_size, im.min_qty, im.min_notional))
                .unwrap_or((Decimal::new(1, 1), Decimal::new(1, 4), None, None));

            let meta = InstrumentMeta {
                instrument_id: market.instrument_id(),
                market_index: market.market_index(),
                base_asset: AssetId::new(market.base()),
                quote_asset: AssetId::new(quote_currency),
                tick_size,
                lot_size,
                min_qty,
                min_notional,
                fee_asset_default: Some(AssetId::new(quote_currency)),
                kind: market.instrument_kind(),
            };

            tracing::info!(
                "Registering instrument {} (index {:?}, kind={:?})",
                meta.instrument_id,
                meta.market_index,
                meta.kind,
            );

            ids.push(meta.instrument_id.clone());
            // # simplify, why register meta? its overcomplicating stuff.
            engine.register_instrument(meta);
        }
        ids
    } else {
        engine.register_instrument(instrument_meta.clone());
        vec![instrument_meta.instrument_id.clone()]
    };

    // Register the strategy (already boxed above)
    engine.register_strategy(strategy_box);

    // TODO: Refactor to Exchange lifecycle hooks for multi-exchange support
    // Instead of direct hl_client calls, add Exchange trait methods:
    //   async fn on_start(&self, sync_mechanism: SyncMechanism) -> Result<()>
    //   async fn on_stop(&self, sync_mechanism: SyncMechanism) -> Result<()>
    // This would make registration exchange-agnostic and scalable.
    // For now, this Hyperliquid-specific implementation works correctly.

    // Register user for fills tracking ONLY in Live mode with Poll mechanism
    // Paper/Backtest modes don't need real fill subscriptions (avoids HTTP 422 noise)
    if matches!(trading_mode, TradingMode::Live)
        && engine.sync_mechanism() == bot_core::SyncMechanism::Poll
    {
        if let Err(e) = hl_client.register_user().await {
            tracing::warn!(
                "Failed to register for fills tracking (proxy may not be running): {}",
                e
            );
        } else {
            tracing::info!("Registered for fills tracking (Poll mechanism)");
        }
    } else if !matches!(trading_mode, TradingMode::Live) {
        tracing::info!("Skipping fills registration (non-live mode)");
    } else {
        tracing::info!("Skipping fills registration (using Snapshot mechanism)");
    }
    // #simplify leverage should go inside exhcnage init hook
    // Update leverage on Hyperliquid for perp markets
    // Extract leverage from strategy config and set it on the exchange
    // Skip for Paper/Backtest modes - only needed for live trading
    let is_arb = config.strategy_type == "arbitrage" || config.strategy_type == "arb";
    if (!config.is_spot() && !config.is_outcome() || is_arb) && matches!(trading_mode, TradingMode::Live) {
        let strategy_leverage: Option<u32> = if let Some(ref g) = config.grid {
            Decimal::from_str(&g.leverage).ok().and_then(|d| d.to_u32())
        } else if let Some(ref d) = config.dca {
            Decimal::from_str(&d.leverage).ok().and_then(|d| d.to_u32())
        } else if let Some(ref a) = config.arbitrage {
            Decimal::from_str(&a.perp_leverage)
                .ok()
                .and_then(|d| d.to_u32())
        } else {
            None
        };

        if let Some(leverage) = strategy_leverage {
            // For arb strategy, leverage must be set on the perp market, not spot.
            // config.market_index() returns the primary market (spot for arb).
            let market_index =
                if config.strategy_type == "arbitrage" || config.strategy_type == "arb" {
                    config
                        .markets
                        .iter()
                        .find(|m| !m.is_spot())
                        .map(|m| m.market_index())
                        .unwrap_or_else(|| config.market_index())
                } else {
                    config.market_index()
                };
            tracing::info!(
                "Setting leverage to {}x for market {:?}",
                leverage,
                market_index
            );
            if let Err(e) = hl_client
                .update_leverage(&market_index, leverage, true)
                .await
            {
                tracing::error!("Failed to set leverage: {}", e);
                // Don't fail startup - leverage might already be set correctly
            }
        }
    } else if !config.is_spot() {
        tracing::info!("Skipping leverage update (Paper/Backtest mode)");
    }

    // Spawn the engine runner (instruments already set above for both arb and non-arb)
    // For backtest mode, use 0ms poll delay for fast-forward execution
    let poll_delay = if matches!(trading_mode, TradingMode::Backtest { .. }) {
        tracing::info!("📊 BACKTEST MODE - using 0ms poll delay for fast-forward execution");
        0
    } else {
        config.poll_delay_ms
    };
    let runner_config = bot_engine::RunnerConfig {
        min_poll_delay_ms: poll_delay,
        quote_poll_interval_ms: poll_delay,
        ..Default::default()
    };

    tracing::info!("Poll delay: {}ms", poll_delay);

    // Build syncer config if present and enabled
    let syncer_config = config.sync.as_ref().and_then(|sync| {
        if !sync.enabled {
            tracing::info!("Trade syncing disabled in config");
            return None;
        }
        tracing::info!(
            "Trade syncing enabled: bot_id={}, upstream_url={}, interval={}ms",
            sync.bot_id,
            sync.upstream_url,
            sync.sync_interval_ms
        );
        Some(TradeSyncerConfig {
            bot_id: sync.bot_id.clone(),
            upstream_url: sync.upstream_url.clone(),
            sync_interval_ms: sync.sync_interval_ms,
            timeout_secs: sync.timeout_secs,
            max_retries: 3,
            retry_delay_ms: 1000,
            instruments: instruments.clone(),
        })
    });

    // Cache sync mechanism before engine is moved to runner
    let sync_mechanism = engine.sync_mechanism();

    // Configure account syncer for Snapshot strategies (e.g., Arbitrage)
    let account_syncer_config = if sync_mechanism == bot_core::SyncMechanism::Snapshot {
        config.sync.as_ref().and_then(|sync| {
            if !sync.enabled {
                return None;
            }
            tracing::info!(
                "Account syncing enabled for Snapshot strategy: bot_id={}, upstream_url={}",
                sync.bot_id,
                sync.upstream_url
            );
            Some(bot_engine::AccountSyncerConfig {
                bot_id: sync.bot_id.clone(),
                upstream_url: sync.upstream_url.clone(),
                sync_interval_ms: 10_000,
                timeout_secs: 10,
                max_retries: 3,
                retry_delay_ms: 1000,
            })
        })
    } else {
        None
    };
    // #simplify exchange, instrumets, all other except runner config can get via engine.
    let (runner_handle, shutdown_tx) = bot_engine::spawn_runner_with_syncer(
        engine,
        vec![exchange],
        instruments,
        runner_config,
        syncer_config,
        account_syncer_config,
    );

    // For backtest mode, spawn a monitor that triggers shutdown when prices are exhausted
    if let Some(paper) = backtest_paper {
        let backtest_shutdown_tx = shutdown_tx.clone();
        tokio::spawn(async move {
            loop {
                if !paper.has_queued_quotes().await {
                    // Allow final processing
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    tracing::info!("📊 Backtest complete - all historical prices processed");
                    let _ = backtest_shutdown_tx.unbounded_send(());
                    break;
                }
                tokio::task::yield_now().await;
            }
        });
    }

    tracing::info!("====================================");
    tracing::info!(
        "Bot started on {} - Press Ctrl+C to stop",
        config.environment
    );
    tracing::info!("====================================");

    // Wait for shutdown signal (SIGINT/Ctrl+C or SIGTERM) or runner completion
    let mut runner_handle = runner_handle;

    // Create shutdown future that handles both SIGINT and SIGTERM (Unix)
    let shutdown_signal = async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm =
                signal(SignalKind::terminate()).expect("Failed to create SIGTERM handler");
            let mut sigint =
                signal(SignalKind::interrupt()).expect("Failed to create SIGINT handler");
            tokio::select! {
                _ = sigterm.recv() => tracing::info!("SIGTERM received, initiating graceful shutdown..."),
                _ = sigint.recv() => tracing::info!("SIGINT received, initiating graceful shutdown..."),
            }
        }
        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("Ctrl+C received, initiating graceful shutdown...");
        }
    };

    tokio::select! {
        result = &mut runner_handle => {
            // Runner exited on its own (strategy stopped)
            match result {
                Ok(_) => tracing::info!("Strategy stopped, exiting..."),
                Err(e) => tracing::error!("Runner task error: {}", e),
            }
        }
        _ = shutdown_signal => {
            let _ = shutdown_tx.unbounded_send(());
            tracing::info!("Waiting for cleanup to complete...");
            match runner_handle.await {
                Ok(_) => tracing::info!("Cleanup completed"),
                Err(e) => tracing::error!("Runner task error during cleanup: {}", e),
            }
        }
    }

    // TODO: Refactor to exchange.on_stop(sync_mechanism) lifecycle hook
    // See on_start() TODO above for full architecture design.

    // Deregister from fills tracking (only if we registered — Live + Poll only)
    if matches!(trading_mode, TradingMode::Live)
        && sync_mechanism == bot_core::SyncMechanism::Poll
    {
        if let Err(e) = hl_client.deregister_user().await {
            tracing::warn!("Failed to deregister from fills tracking: {}", e);
        } else {
            tracing::info!("Deregistered from fills tracking");
        }
    }

    tracing::info!("Bot stopped.");
    Ok(())
}

/// Report an initialization error to the upstream sync API.
///
/// This is a lightweight, standalone reporter for errors that happen before
/// the engine and syncers are created (e.g., invalid private key, exchange
/// client creation failure). It sends a one-shot `stop_bot: true` request
/// to the same `/sync/{bot_id}` endpoint used by `TradeSyncer::shutdown_sync`.
async fn report_init_error(config: &BotConfig, error_reason: &str) {
    let sync = match config.sync.as_ref() {
        Some(s) if s.enabled && !s.bot_id.is_empty() && !s.upstream_url.is_empty() => s,
        _ => {
            tracing::warn!("[InitError] No sync config available, cannot report error to upstream");
            return;
        }
    };

    let url = format!(
        "{}/sync/{}",
        sync.upstream_url.trim_end_matches('/'),
        sync.bot_id
    );

    let payload = serde_json::json!({
        "trades": [],
        "ts": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        "current_price": null,
        "stop_bot": true,
        "stop_reason": error_reason,
    });

    tracing::info!(
        "[InitError] Reporting init failure to upstream: bot_id={}, reason={}",
        sync.bot_id,
        error_reason
    );

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("[InitError] Failed to create HTTP client: {}", e);
            return;
        }
    };

    match client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                tracing::info!(
                    "[InitError] Successfully reported init error to upstream ({})",
                    status
                );
            } else {
                let body = resp.text().await.unwrap_or_default();
                tracing::warn!("[InitError] Upstream returned {}: {}", status, body);
            }
        }
        Err(e) => {
            tracing::error!("[InitError] Failed to report init error: {}", e);
        }
    }
}
