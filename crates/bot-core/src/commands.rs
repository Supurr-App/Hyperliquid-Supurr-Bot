//! Strategy commands sent to the engine.
//!
//! Commands are the write-side API for strategies. A strategy never calls an
//! exchange directly; it emits commands through [`StrategyContext`], and the
//! engine turns those commands into exchange writes and later execution events.
//!
//! # Example
//!
//! ```
//! use bot_core::{
//!     Command, Environment, ExchangeId, ExchangeInstance, InstrumentId, OrderSide, PlaceOrder,
//!     Price, Qty,
//! };
//! use rust_decimal::Decimal;
//!
//! let exchange = ExchangeInstance::new(ExchangeId::new("hyperliquid"), Environment::Testnet);
//! let order = PlaceOrder::limit(
//!     exchange,
//!     InstrumentId::new("BTC-PERP"),
//!     OrderSide::Buy,
//!     Price::new(Decimal::new(65_000, 0)),
//!     Qty::new(Decimal::new(1, 3)),
//! );
//!
//! let command = Command::PlaceOrder(order);
//! assert_eq!(command.instrument().unwrap().as_str(), "BTC-PERP");
//! ```
//!
//! [`StrategyContext`]: crate::StrategyContext

use crate::types::*;
use serde::{Deserialize, Serialize};

/// Commands that strategies can emit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Command {
    /// Submit one limit order.
    PlaceOrder(PlaceOrder),
    /// Submit several limit orders as one logical batch.
    PlaceOrders(Vec<PlaceOrder>),
    /// Cancel one tracked order by client order ID.
    CancelOrder(CancelOrder),
    /// Cancel all tracked orders, optionally scoped to one instrument.
    CancelAll(CancelAll),
    /// Request strategy stop with a reason
    StopStrategy(StopStrategy),
}

impl Command {
    /// Get the client order ID if applicable (returns first one for batch)
    pub fn client_id(&self) -> Option<&ClientOrderId> {
        match self {
            Command::PlaceOrder(c) => Some(&c.client_id),
            Command::PlaceOrders(orders) => orders.first().map(|o| &o.client_id),
            Command::CancelOrder(c) => Some(&c.client_id),
            Command::CancelAll(_) => None,
            Command::StopStrategy(_) => None,
        }
    }

    /// Get the instrument if applicable (returns first one for batch)
    pub fn instrument(&self) -> Option<&InstrumentId> {
        match self {
            Command::PlaceOrder(c) => Some(&c.instrument),
            Command::PlaceOrders(orders) => orders.first().map(|o| &o.instrument),
            Command::CancelOrder(_) => None,
            Command::CancelAll(c) => c.instrument.as_ref(),
            Command::StopStrategy(_) => None,
        }
    }
}

/// Place a new order
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaceOrder {
    /// Client-generated unique order ID
    pub client_id: ClientOrderId,
    /// Target exchange instance
    pub exchange: ExchangeInstance,
    /// Instrument to trade
    pub instrument: InstrumentId,
    /// Buy or Sell
    pub side: OrderSide,
    /// Limit price
    pub price: Price,
    /// Quantity
    pub qty: Qty,
    /// Time in force (GTC, IOC, FOK)
    pub tif: TimeInForce,
    /// Post-only (maker only)
    pub post_only: bool,
    /// Reduce-only (close position only)
    pub reduce_only: bool,
}

impl PlaceOrder {
    /// Create a new PlaceOrder command with defaults
    pub fn limit(
        exchange: ExchangeInstance,
        instrument: InstrumentId,
        side: OrderSide,
        price: Price,
        qty: Qty,
    ) -> Self {
        Self {
            client_id: ClientOrderId::generate(),
            exchange,
            instrument,
            side,
            price,
            qty,
            tif: TimeInForce::Gtc,
            post_only: false,
            reduce_only: false,
        }
    }

    /// Set time in force
    pub fn with_tif(mut self, tif: TimeInForce) -> Self {
        self.tif = tif;
        self
    }

    /// Set post-only
    pub fn post_only(mut self) -> Self {
        self.post_only = true;
        self
    }

    /// Set reduce-only
    pub fn reduce_only(mut self) -> Self {
        self.reduce_only = true;
        self
    }

    /// Set a specific client order ID
    pub fn with_client_id(mut self, client_id: ClientOrderId) -> Self {
        self.client_id = client_id;
        self
    }
}

/// Cancel an existing order
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelOrder {
    /// Target exchange instance
    pub exchange: ExchangeInstance,
    /// Client order ID to cancel
    pub client_id: ClientOrderId,
}

impl CancelOrder {
    /// Create a command that cancels one order by client order ID.
    ///
    /// # Example
    ///
    /// ```
    /// use bot_core::{CancelOrder, ClientOrderId, Environment, ExchangeId, ExchangeInstance};
    ///
    /// let exchange = ExchangeInstance::new(ExchangeId::new("hyperliquid"), Environment::Mainnet);
    /// let cancel = CancelOrder::new(exchange, ClientOrderId::new("client-1"));
    ///
    /// assert_eq!(cancel.client_id.as_str(), "client-1");
    /// ```
    pub fn new(exchange: ExchangeInstance, client_id: ClientOrderId) -> Self {
        Self {
            exchange,
            client_id,
        }
    }
}

/// Cancel all orders (optionally for a specific instrument)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelAll {
    /// Target exchange instance
    pub exchange: ExchangeInstance,
    /// Optionally limit to a specific instrument
    pub instrument: Option<InstrumentId>,
}

impl CancelAll {
    /// Create a command that cancels every open order on an exchange instance.
    ///
    /// Use [`CancelAll::for_instrument`] when the cancellation should be
    /// limited to one instrument.
    pub fn new(exchange: ExchangeInstance) -> Self {
        Self {
            exchange,
            instrument: None,
        }
    }

    /// Create a command that cancels all open orders for one instrument.
    ///
    /// # Example
    ///
    /// ```
    /// use bot_core::{CancelAll, Environment, ExchangeId, ExchangeInstance, InstrumentId};
    ///
    /// let exchange = ExchangeInstance::new(ExchangeId::new("hyperliquid"), Environment::Testnet);
    /// let cancel = CancelAll::for_instrument(exchange, InstrumentId::new("ETH-PERP"));
    ///
    /// assert_eq!(cancel.instrument.unwrap().as_str(), "ETH-PERP");
    /// ```
    pub fn for_instrument(exchange: ExchangeInstance, instrument: InstrumentId) -> Self {
        Self {
            exchange,
            instrument: Some(instrument),
        }
    }
}

/// Stop strategy command - requests the engine to stop a strategy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StopStrategy {
    /// The strategy requesting to stop
    pub strategy_id: StrategyId,
    /// Reason for stopping
    pub reason: String,
}

impl StopStrategy {
    /// Create a command requesting strategy shutdown.
    ///
    /// The engine decides when the stop callback runs; this command only records
    /// the strategy and reason.
    pub fn new(strategy_id: StrategyId, reason: impl Into<String>) -> Self {
        Self {
            strategy_id,
            reason: reason.into(),
        }
    }
}
