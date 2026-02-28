//! Spot-Perp Arbitrage Strategy
//!
//! Exploits price differences between spot and perpetual markets.
//! When perp trades at a premium to spot, buy spot and short perp.
//! When spread converges, close both positions for profit.
//!
//! # Example
//!
//! ```ignore
//! use strategy_arbitrage::{ArbitrageStrategy, ArbitrageConfig};
//! use bot_core::{StrategyId, Market, HyperliquidMarket, Environment};
//! use rust_decimal_macros::dec;
//!
//! let config = ArbitrageConfig {
//!     strategy_id: StrategyId::new("eth-arb"),
//!     spot_market: Market::Hyperliquid(HyperliquidMarket::Spot {
//!         base: "ETH".into(), quote: "USDC".into(), index: 10002, instrument_meta: None,
//!     }),
//!     perp_market: Market::Hyperliquid(HyperliquidMarket::Perp {
//!         base: "ETH".into(), quote: "USDC".into(), index: 1, instrument_meta: None,
//!     }),
//!     environment: Environment::Mainnet,
//!     order_amount: dec!(0.5),
//!     min_opening_spread_pct: dec!(0.003),  // 0.3%
//!     min_closing_spread_pct: dec!(-0.001), // -0.1%
//!     perp_leverage: dec!(1),
//!     spot_slippage_buffer_pct: dec!(0.001),
//!     perp_slippage_buffer_pct: dec!(0.001),
//! };
//!
//! let strategy = ArbitrageStrategy::new(config);
//! ```

mod config;
mod strategy;

pub use config::*;
pub use strategy::*;
