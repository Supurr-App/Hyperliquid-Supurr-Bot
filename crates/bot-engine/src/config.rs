//! Bot configuration types and strategy builder.
//!
//! This module contains:
//! - `BotConfig` — V2 unified config format
//! - Strategy-specific JSON config structs (GridConfigJson, DCAConfigJson, etc.)
//! - `build_strategy()` — construct Box<dyn Strategy> from BotConfig
//! - `build_instrument_meta()` — construct InstrumentMeta from Market

use anyhow::{Context, Result};
use bot_core::{AssetId, Environment, InstrumentId, InstrumentMeta, Market, Strategy, StrategyId};
use rust_decimal::Decimal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;

// Strategy imports
use strategy_arbitrage::{ArbitrageConfig, ArbitrageStrategy};
use strategy_dca::{DCAConfig, DCADirection, DCAStrategy};
use strategy_grid::{GridConfig, GridMode, GridStrategy};
use strategy_market_maker::{MarketMaker, MarketMakerConfig, SkewMode};
use strategy_rsi::{RsiStrategy, RsiStrategyConfig};
use strategy_tick_trader::{TickTrader, TickTraderConfig};

// =============================================================================
// Bot Configuration (V2 Format)
// =============================================================================

/// Bot configuration - V2 format with markets array.
/// This is the unified config format for all strategies.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BotConfig {
    /// Environment: "testnet" or "mainnet"
    pub environment: String,

    /// Private key (hex, with or without 0x prefix)
    pub private_key: String,

    /// Wallet address
    pub address: String,

    /// Optional vault address
    #[serde(default)]
    pub vault_address: Option<String>,

    /// Strategy type: "grid", "mm", "dca", or "arbitrage"
    pub strategy_type: String,

    /// Markets to trade on (V2 format - uses bot_core::Market enum)
    pub markets: Vec<Market>,

    /// Polling delay in milliseconds
    #[serde(default = "default_poll_delay_ms")]
    pub poll_delay_ms: u64,

    // -------------------------------------------------------------------------
    // Strategy-specific configs (only one should be set based on strategy_type)
    // -------------------------------------------------------------------------
    /// Grid strategy configuration
    #[serde(default)]
    pub grid: Option<GridConfigJson>,

    /// Market Maker strategy configuration
    #[serde(default)]
    pub mm: Option<MMConfigJson>,

    /// DCA strategy configuration
    #[serde(default)]
    pub dca: Option<DCAConfigJson>,

    /// Arbitrage strategy configuration
    #[serde(default)]
    pub arbitrage: Option<ArbitrageConfigJson>,

    // -------------------------------------------------------------------------
    // Common config
    // -------------------------------------------------------------------------
    /// Builder fee configuration
    #[serde(default)]
    pub builder_fee: Option<BuilderFeeConfig>,

    /// Trade sync configuration for upstream API PnL tracking
    #[serde(default)]
    pub sync: Option<SyncConfigJson>,

    // -------------------------------------------------------------------------
    // Custom strategy configs (captured automatically via serde flatten)
    // -------------------------------------------------------------------------
    /// Catch-all for custom strategy configuration sections.
    /// Any JSON key that doesn't match a named field above lands here.
    /// Custom strategies read their config via `config.custom_config("mystrategy")`.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl BotConfig {
    /// Get the primary market from the markets array
    pub fn primary_market(&self) -> &Market {
        &self.markets[0]
    }

    /// Check if this is a spot market
    pub fn is_spot(&self) -> bool {
        self.primary_market().is_spot()
    }

    /// Check if this is a prediction market outcome
    pub fn is_outcome(&self) -> bool {
        self.primary_market().is_outcome()
    }

    /// Get instrument ID from primary market
    pub fn instrument_id(&self) -> InstrumentId {
        self.primary_market().instrument_id()
    }

    /// Get market index from primary market
    pub fn market_index(&self) -> bot_core::MarketIndex {
        self.primary_market().market_index()
    }

    /// Get HIP-3 config if this is a HIP-3 market
    pub fn hip3_config(&self) -> Option<bot_core::market::Hip3MarketConfig> {
        self.primary_market().hip3_config()
    }

    /// Get custom strategy config as typed struct.
    /// Looks up the strategy name in the `extra` catch-all map and deserializes.
    ///
    /// # Example
    /// ```rust,ignore
    /// let my_config: MyConfig = config.custom_config("mystrategy")?;
    /// ```
    pub fn custom_config<T: serde::de::DeserializeOwned>(&self, strategy_name: &str) -> Result<T> {
        let raw = self.extra.get(strategy_name).with_context(|| {
            format!(
                "Custom strategy config missing: add '\"{}\"' section to your JSON config",
                strategy_name
            )
        })?;
        serde_json::from_value(raw.clone())
            .with_context(|| format!("Failed to parse '{}' config section", strategy_name))
    }

    /// Load config from environment variables.
    /// V2 configs require a JSON file, so this returns an error.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_env() -> Result<Self> {
        Err(anyhow::anyhow!(
            "V2 config format requires a JSON file. Use --config <file>"
        ))
    }

    /// Load config from a JSON file
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_file(path: &PathBuf) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {:?}", path))?;
        let config: Self = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {:?}", path))?;

        // Validate markets array is not empty
        if config.markets.is_empty() {
            anyhow::bail!("Config must have at least one market in the 'markets' array");
        }

        Ok(config)
    }

    /// Parse environment from string
    pub fn parse_environment(&self) -> Environment {
        match self.environment.to_lowercase().as_str() {
            "mainnet" | "main" | "prod" => Environment::Mainnet,
            _ => Environment::Testnet,
        }
    }
}

// =============================================================================
// Market Maker Config
// =============================================================================

/// Market Maker strategy configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MMConfigJson {
    /// Base order size in base asset
    pub base_order_size: String,
    /// Base spread between bid and ask
    pub base_spread: String,
    /// Maximum position size in base asset
    pub max_position_size: String,
    /// Skew mode: "both", "size", "price", or "none"
    #[serde(default = "default_skew_mode")]
    pub skew_mode: String,
    /// Price skew gamma (how aggressively to skew quotes based on position)
    #[serde(default = "default_price_skew_gamma")]
    pub price_skew_gamma: String,
    /// Size skew floor (minimum size for quotes)
    #[serde(default = "default_size_skew_floor")]
    pub size_skew_floor: String,
    /// Minimum price change to update quotes
    #[serde(default = "default_min_price_change_pct")]
    pub min_price_change_pct: String,
    /// Stop loss (optional)
    #[serde(default)]
    pub stop_loss: Option<String>,
    /// Take profit (optional)
    #[serde(default)]
    pub take_profit: Option<String>,
}

// =============================================================================
// Grid Config
// =============================================================================

/// Grid strategy configuration from JSON
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GridConfigJson {
    /// Grid mode: "long", "short", "neutral"
    #[serde(default = "default_grid_mode")]
    pub mode: String,
    /// Number of grid levels
    #[serde(default = "default_grid_levels")]
    pub levels: u32,
    /// Start price of the grid
    pub start_price: String,
    /// End price of the grid
    pub end_price: String,
    /// Maximum investment in quote currency (USDC)
    pub max_investment_quote: String,
    /// Leverage to use
    #[serde(default = "default_leverage")]
    pub leverage: String,
    /// Maximum leverage allowed (for liquidation calculation)
    #[serde(default = "default_max_leverage")]
    pub max_leverage: String,
    /// Use post-only orders
    #[serde(default)]
    pub post_only: bool,
    /// Stop loss (optional)
    #[serde(default)]
    pub stop_loss: Option<String>,
    /// Take profit (optional)
    #[serde(default)]
    pub take_profit: Option<String>,
    /// Trailing upper limit price (optional). When set, enables trailing-up:
    /// the grid slides up as price rises, until the top of the window would
    /// exceed this ceiling.
    #[serde(default)]
    pub trailing_up_limit: Option<String>,
    /// Trailing lower limit price (optional). When set, enables trailing-down:
    /// the grid slides down as price falls, until the bottom of the window
    /// would go below this floor.
    #[serde(default)]
    pub trailing_down_limit: Option<String>,
}

// =============================================================================
// Arbitrage Config
// =============================================================================

/// Arbitrage strategy configuration from JSON
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ArbitrageConfigJson {
    /// Order amount in quote asset / USDC (e.g., "100" = $100 worth)
    pub order_amount: String,
    /// Leverage for perp position
    pub perp_leverage: String,
    /// Minimum spread to open (e.g., "0.003" = 0.3%)
    pub min_opening_spread_pct: String,
    /// Minimum spread to close (e.g., "-0.001" = -0.1%)
    pub min_closing_spread_pct: String,
    /// Slippage buffer for spot orders
    #[serde(default = "default_slippage")]
    pub spot_slippage_buffer_pct: String,
    /// Slippage buffer for perp orders
    #[serde(default = "default_slippage")]
    pub perp_slippage_buffer_pct: String,
}

// =============================================================================
// DCA Config
// =============================================================================

/// DCA strategy configuration from JSON
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DCAConfigJson {
    /// Direction: "long" or "short"
    #[serde(default = "default_dca_direction")]
    pub direction: String,
    /// Price to trigger base order
    pub trigger_price: String,
    /// Base order size in base asset
    pub base_order_size: String,
    /// DCA order size in base asset
    pub dca_order_size: String,
    /// Maximum number of DCA orders
    #[serde(default = "default_max_dca_orders")]
    pub max_dca_orders: u32,
    /// Size multiplier for each subsequent DCA order
    #[serde(default = "default_size_multiplier")]
    pub size_multiplier: String,
    /// Price deviation percentage to trigger first DCA
    pub price_deviation_pct: String,
    /// Deviation multiplier for subsequent triggers
    #[serde(default = "default_deviation_multiplier")]
    pub deviation_multiplier: String,
    /// Take profit percentage from average entry
    pub take_profit_pct: String,
    /// Optional stop loss as absolute PnL threshold (negative value)
    #[serde(default)]
    pub stop_loss: Option<String>,
    /// Leverage (1 for spot-like)
    #[serde(default = "default_leverage")]
    pub leverage: String,
    /// Max leverage allowed
    #[serde(default = "default_max_leverage")]
    pub max_leverage: String,
    /// Whether to restart cycle after take profit
    #[serde(default)]
    pub restart_on_complete: bool,
    /// Cooldown period in seconds between cycles (default: 60)
    #[serde(default = "default_cooldown_period")]
    pub cooldown_period_secs: u64,
}

// =============================================================================
// Common Config Structs
// =============================================================================

/// Builder fee configuration for JSON config
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BuilderFeeConfig {
    /// Builder address to receive the fee
    pub address: String,
    /// Fee in tenths of a basis point (e.g., 30 = 3 bp = 0.03%)
    pub fee_tenths_bp: u32,
}

/// Trade syncer configuration for upstream API PnL tracking
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SyncConfigJson {
    /// Bot ID for upstream API (required if enabled)
    pub bot_id: String,
    /// Upstream API base URL (e.g., "https://api.example.com/bot-api")
    pub upstream_url: String,
    /// Sync interval in milliseconds (default: 10000)
    #[serde(default = "default_sync_interval_ms")]
    pub sync_interval_ms: u64,
    /// HTTP timeout in seconds (default: 10)
    #[serde(default = "default_sync_timeout")]
    pub timeout_secs: u64,
    /// Enable syncing (default: true if sync section is present)
    #[serde(default = "default_sync_enabled")]
    pub enabled: bool,
}

// =============================================================================
// Default value functions
// =============================================================================

fn default_dca_direction() -> String {
    "long".to_string()
}

fn default_max_dca_orders() -> u32 {
    5
}

fn default_size_multiplier() -> String {
    "2.0".to_string()
}

fn default_deviation_multiplier() -> String {
    "1.0".to_string()
}

fn default_cooldown_period() -> u64 {
    60 // 60 seconds like Binance
}

fn default_slippage() -> String {
    "0.001".to_string()
}

fn default_grid_mode() -> String {
    "long".to_string()
}

fn default_grid_levels() -> u32 {
    20
}

fn default_leverage() -> String {
    "5".to_string()
}

fn default_max_leverage() -> String {
    "50".to_string()
}

fn default_sync_interval_ms() -> u64 {
    10_000
}

fn default_sync_timeout() -> u64 {
    10
}

fn default_sync_enabled() -> bool {
    true
}

fn default_poll_delay_ms() -> u64 {
    500
}

fn default_skew_mode() -> String {
    "both".to_string()
}
fn default_price_skew_gamma() -> String {
    "0.05".to_string()
}
fn default_size_skew_floor() -> String {
    "0.2".to_string()
}
fn default_min_price_change_pct() -> String {
    "0.0005".to_string()
}

// =============================================================================
// Strategy Builder
// =============================================================================

/// Build the strategy from a V2 BotConfig.
/// Works identically on native and WASM.
pub fn build_strategy(config: &BotConfig) -> Result<Box<dyn Strategy>> {
    let environment = config.parse_environment();
    let strategy_type = config.strategy_type.to_lowercase();

    let is_arb = strategy_type == "arbitrage" || strategy_type == "arb";
    let is_grid = strategy_type == "grid";
    let is_dca = strategy_type == "dca";
    let is_mm = strategy_type == "mm" || strategy_type == "market_maker";

    if is_arb {
        // Arbitrage strategy — requires exactly 2 markets: [spot, perp]
        anyhow::ensure!(
            config.markets.len() >= 2,
            "Arbitrage requires 2 markets in config: markets[0]=spot, markets[1]=perp"
        );

        let arb_json = config
            .arbitrage
            .as_ref()
            .context("Arbitrage config missing: add 'arbitrage' section to config")?;

        let spot_market = config.markets[0].clone();
        let perp_market = config.markets[1].clone();

        let arb_config = ArbitrageConfig {
            strategy_id: StrategyId::new(format!("{}-arb", spot_market.base().to_lowercase())),
            spot_market,
            perp_market,
            environment,
            order_amount: Decimal::from_str(&arb_json.order_amount)
                .context("Invalid order_amount")?,
            perp_leverage: Decimal::from_str(&arb_json.perp_leverage)
                .context("Invalid perp_leverage")?,
            min_opening_spread_pct: Decimal::from_str(&arb_json.min_opening_spread_pct)
                .context("Invalid min_opening_spread_pct")?,
            min_closing_spread_pct: Decimal::from_str(&arb_json.min_closing_spread_pct)
                .context("Invalid min_closing_spread_pct")?,
            spot_slippage_buffer_pct: Decimal::from_str(&arb_json.spot_slippage_buffer_pct)
                .context("Invalid spot_slippage_buffer_pct")?,
            perp_slippage_buffer_pct: Decimal::from_str(&arb_json.perp_slippage_buffer_pct)
                .context("Invalid perp_slippage_buffer_pct")?,
        };

        // Validate arb config
        let errors = arb_config.validate();
        if !errors.is_empty() {
            anyhow::bail!(
                "Arbitrage configuration validation failed: {}",
                errors.join(", ")
            );
        }

        Ok(Box::new(ArbitrageStrategy::new(arb_config)))
    } else if is_grid {
        // Grid strategy
        let grid_json = config
            .grid
            .as_ref()
            .context("Grid config missing: add 'grid' section to config")?;

        let grid_config = GridConfig {
            strategy_id: StrategyId::new(format!("{}-grid", config.primary_market().base())),
            environment,
            market: config.primary_market().clone(),
            grid_mode: match grid_json.mode.to_lowercase().as_str() {
                "short" => GridMode::Short,
                "neutral" => GridMode::Neutral,
                _ => GridMode::Long,
            },
            grid_levels: grid_json.levels,
            start_price: Decimal::from_str(&grid_json.start_price)
                .context("Invalid start_price")?,
            end_price: Decimal::from_str(&grid_json.end_price).context("Invalid end_price")?,
            max_investment_quote: Decimal::from_str(&grid_json.max_investment_quote)
                .context("Invalid max_investment_quote")?,
            base_order_size: Decimal::new(1, 3), // fallback 0.001
            leverage: Decimal::from_str(&grid_json.leverage).context("Invalid leverage")?,
            max_leverage: Decimal::from_str(&grid_json.max_leverage)
                .context("Invalid max_leverage")?,
            post_only: grid_json.post_only,
            stop_loss: grid_json
                .stop_loss
                .as_ref()
                .map(|s| Decimal::from_str(s))
                .transpose()
                .context("Invalid grid stop_loss")?,
            take_profit: grid_json
                .take_profit
                .as_ref()
                .map(|s| Decimal::from_str(s))
                .transpose()
                .context("Invalid grid take_profit")?,
            // Trailing is opt-in; enabled by the presence of a limit price.
            trailing_up_limit: grid_json
                .trailing_up_limit
                .as_ref()
                .map(|s| Decimal::from_str(s))
                .transpose()
                .context("Invalid grid trailing_up_limit")?,
            trailing_down_limit: grid_json
                .trailing_down_limit
                .as_ref()
                .map(|s| Decimal::from_str(s))
                .transpose()
                .context("Invalid grid trailing_down_limit")?,
        };

        // Validate grid config
        let errors = grid_config.validate();
        if !errors.is_empty() {
            anyhow::bail!(
                "Grid configuration validation failed: {}",
                errors.join(", ")
            );
        }

        Ok(Box::new(GridStrategy::new(grid_config)))
    } else if is_dca {
        // DCA strategy
        let dca_json = config
            .dca
            .as_ref()
            .context("DCA config missing: add 'dca' section to config")?;

        let dca_config = DCAConfig {
            strategy_id: StrategyId::new(format!("{}-dca", config.primary_market().base())),
            environment,
            market: config.primary_market().clone(),
            direction: match dca_json.direction.to_lowercase().as_str() {
                "short" => DCADirection::Short,
                _ => DCADirection::Long,
            },
            trigger_price: Decimal::from_str(&dca_json.trigger_price)
                .context("Invalid trigger_price")?,
            base_order_size: Decimal::from_str(&dca_json.base_order_size)
                .context("Invalid base_order_size")?,
            dca_order_size: Decimal::from_str(&dca_json.dca_order_size)
                .context("Invalid dca_order_size")?,
            max_dca_orders: dca_json.max_dca_orders,
            size_multiplier: Decimal::from_str(&dca_json.size_multiplier)
                .context("Invalid size_multiplier")?,
            price_deviation_pct: Decimal::from_str(&dca_json.price_deviation_pct)
                .context("Invalid price_deviation_pct")?,
            deviation_multiplier: Decimal::from_str(&dca_json.deviation_multiplier)
                .context("Invalid deviation_multiplier")?,
            take_profit_pct: Decimal::from_str(&dca_json.take_profit_pct)
                .context("Invalid take_profit_pct")?,
            stop_loss: dca_json
                .stop_loss
                .as_ref()
                .map(|s| Decimal::from_str(s))
                .transpose()
                .context("Invalid stop_loss")?,
            leverage: Decimal::from_str(&dca_json.leverage).context("Invalid leverage")?,
            max_leverage: Decimal::from_str(&dca_json.max_leverage)
                .context("Invalid max_leverage")?,
            restart_on_complete: dca_json.restart_on_complete,
            cooldown_period_secs: dca_json.cooldown_period_secs,
        };

        // Validate DCA config
        let errors = dca_config.validate();
        if !errors.is_empty() {
            anyhow::bail!("DCA configuration validation failed: {}", errors.join(", "));
        }

        Ok(Box::new(DCAStrategy::new(dca_config)))
    } else if strategy_type == "tick_trader" {
        // Tick-trader: custom strategy using custom_config() pattern
        let tick_config: TickTraderConfig = config.custom_config("tick_trader")?;
        let errors = tick_config.validate();
        if !errors.is_empty() {
            anyhow::bail!(
                "Tick trader config validation failed: {}",
                errors.join(", ")
            );
        }
        Ok(Box::new(TickTrader::new(tick_config)))
    } else if strategy_type == "rsi" {
        // RSI strategy: uses inline Wilder's RSI indicator + bar aggregation
        let rsi_config: RsiStrategyConfig = config.custom_config("rsi")?;
        let errors = rsi_config.validate();
        if !errors.is_empty() {
            anyhow::bail!("RSI config validation failed: {}", errors.join(", "));
        }
        let market = config.primary_market().clone();
        let environment = config.parse_environment();
        Ok(Box::new(RsiStrategy::new(rsi_config, market, environment)))
    } else if is_mm {
        // Market maker strategy
        let mm_json = config
            .mm
            .as_ref()
            .context("MM config missing: add 'mm' section to config for strategy_type='mm'")?;

        let base_order_size =
            Decimal::from_str(&mm_json.base_order_size).context("Invalid base_order_size")?;
        let base_spread = Decimal::from_str(&mm_json.base_spread).context("Invalid base_spread")?;
        let max_position_size =
            Decimal::from_str(&mm_json.max_position_size).context("Invalid max_position_size")?;
        let price_skew_gamma =
            Decimal::from_str(&mm_json.price_skew_gamma).context("Invalid price_skew_gamma")?;
        let size_skew_floor =
            Decimal::from_str(&mm_json.size_skew_floor).context("Invalid size_skew_floor")?;
        let min_price_change_pct = Decimal::from_str(&mm_json.min_price_change_pct)
            .context("Invalid min_price_change_pct")?;

        let stop_loss = mm_json
            .stop_loss
            .as_ref()
            .map(|s| Decimal::from_str(s))
            .transpose()
            .context("Invalid stop_loss")?;
        let take_profit = mm_json
            .take_profit
            .as_ref()
            .map(|s| Decimal::from_str(s))
            .transpose()
            .context("Invalid take_profit")?;

        let skew_mode = match mm_json.skew_mode.to_lowercase().as_str() {
            "none" => SkewMode::None,
            "size" => SkewMode::Size,
            "price" => SkewMode::Price,
            "both" | _ => SkewMode::Both,
        };

        let mm_config = MarketMakerConfig {
            strategy_id: StrategyId::new(format!("{}-mm", config.primary_market().base())),
            environment,
            market: config.primary_market().clone(),
            base_order_size,
            base_spread,
            target_position_pct: Decimal::new(5, 1), // 0.5
            min_position_pct: Decimal::new(1, 1),    // 0.1
            max_position_pct: Decimal::new(9, 1),    // 0.9
            max_position_size,
            skew_mode,
            price_skew_gamma,
            size_skew_floor,
            min_price_change_pct,
            stop_loss,
            take_profit,
        };

        // Validate MM config
        let errors = mm_config.validate();
        if !errors.is_empty() {
            anyhow::bail!("Configuration validation failed: {}", errors.join(", "));
        }

        Ok(Box::new(MarketMaker::new(mm_config)))
    } else {
        // =====================================================================
        // Custom strategy — registered by AI agents / advanced users.
        //
        // To register a custom strategy:
        // 1. Create your strategy crate (use supurr_skill/templates/strategy-template/)
        // 2. Add it to workspace Cargo.toml
        // 3. Add dependency in bot-engine/Cargo.toml
        // 4. Add a branch here:
        //      "mystrategy" => strategy_mystrategy::build_from_json(&config),
        // =====================================================================
        anyhow::bail!(
            "Unknown strategy type: '{}'. Built-in types: grid, dca, mm, arb. \
             For custom strategies, see STRATEGY_API.md.",
            strategy_type
        )
    }
}

/// Build InstrumentMeta from BotConfig's primary market.
pub fn build_instrument_meta(config: &BotConfig) -> InstrumentMeta {
    let primary_market = config.primary_market();
    let quote_currency = primary_market.quote();

    // Extract instrument_meta from Market, use defaults if not provided
    let (tick_size, lot_size, min_qty, min_notional) = primary_market
        .instrument_meta()
        .map(|im| (im.tick_size, im.lot_size, im.min_qty, im.min_notional))
        .unwrap_or((Decimal::new(1, 1), Decimal::new(1, 4), None, None));

    InstrumentMeta {
        instrument_id: primary_market.instrument_id(),
        market_index: primary_market.market_index(),
        base_asset: AssetId::new(primary_market.base()),
        quote_asset: AssetId::new(quote_currency),
        tick_size,
        lot_size,
        min_qty,
        min_notional,
        fee_asset_default: Some(AssetId::new(quote_currency)),
        kind: primary_market.instrument_kind(),
    }
}

/// Build InstrumentMeta for ALL markets in `config.markets[]`.
///
/// For multi-instrument strategies (e.g., Arbitrage with spot + perp),
/// this returns one `InstrumentMeta` per market entry.
pub fn build_instrument_metas(config: &BotConfig) -> Vec<InstrumentMeta> {
    config
        .markets
        .iter()
        .map(|market| {
            let quote_currency = market.quote();
            let (tick_size, lot_size, min_qty, min_notional) = market
                .instrument_meta()
                .map(|im| (im.tick_size, im.lot_size, im.min_qty, im.min_notional))
                .unwrap_or((Decimal::new(1, 1), Decimal::new(1, 4), None, None));

            InstrumentMeta {
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
            }
        })
        .collect()
}
