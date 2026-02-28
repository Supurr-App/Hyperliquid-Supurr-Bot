//! DCA (Dollar Cost Averaging) Trading Strategy
//!
//! Implements a DCA bot that builds positions by placing additional orders at
//! progressively lower/higher prices (depending on direction), averaging the
//! entry price, then taking profit when average price reaches a target.
//!
//! # How It Works
//!
//! 1. **Entry**: When price hits the trigger price, places a base order
//! 2. **DCA Orders**: If price continues moving against position, places DCA orders
//!    at pre-calculated deviation levels to average down/up
//! 3. **Take Profit**: When price recovers to TP level (based on average entry),
//!    closes the entire position for profit
//!
//! # Modes
//!
//! - **LONG**: Buy on each DCA trigger (price drops). Take profit above average entry.
//!   Ideal for bullish bias markets where you expect eventual recovery.
//!
//! - **SHORT**: Sell on each DCA trigger (price rises). Take profit below average entry.
//!   Ideal for bearish bias markets.
//!
//! # DCA Ladder Calculation
//!
//! For LONG direction:
//! ```text
//! trigger[0] = trigger_price (base order)
//! trigger[i] = trigger[i-1] × (1 - deviation_pct × multiplier^(i-1))
//! size[0] = base_order_size
//! size[i] = dca_order_size × size_multiplier^(i-1)
//! ```
//!
//! # Example
//!
//! ```ignore
//! use strategy_dca::{DCAStrategy, DCAConfig, DCADirection};
//! use bot_core::{StrategyId, InstrumentId, MarketIndex, ExchangeInstance, ExchangeId, Environment};
//! use rust_decimal::Decimal;
//!
//! let config = DCAConfig {
//!     strategy_id: StrategyId::new("btc-dca"),
//!     exchange: ExchangeInstance::new(ExchangeId::new("hyperliquid"), Environment::Mainnet),
//!     instrument: InstrumentId::new("BTC-PERP"),
//!     market_index: MarketIndex::new(0),
//!     direction: DCADirection::Long,
//!     trigger_price: Decimal::new(95000, 0),
//!     base_order_size: Decimal::new(1, 3),  // 0.001 BTC
//!     dca_order_size: Decimal::new(2, 3),   // 0.002 BTC
//!     max_dca_orders: 5,
//!     price_deviation_pct: Decimal::new(1, 0),  // 1%
//!     deviation_multiplier: Decimal::new(15, 1), // 1.5x
//!     size_multiplier: Decimal::new(2, 0),      // 2x
//!     take_profit_pct: Decimal::new(2, 0),      // 2%
//!     ..Default::default()
//! };
//!
//! let strategy = DCAStrategy::new(config);
//! ```

mod config;
mod state;
mod strategy;

pub use config::*;
pub use state::*;
pub use strategy::*;
