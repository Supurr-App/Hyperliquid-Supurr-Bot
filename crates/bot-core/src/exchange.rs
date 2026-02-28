//! Exchange adapter trait.

use crate::types::*;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Input parameters for placing an order
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderInput {
    pub instrument: InstrumentId,
    pub market_index: MarketIndex,
    pub client_id: ClientOrderId,
    pub side: OrderSide,
    pub price: Price,
    pub qty: Qty,
    pub tif: TimeInForce,
    pub post_only: bool,
    pub reduce_only: bool,
}

/// Errors from exchange operations
#[derive(Debug, Error, Clone)]
pub enum ExchangeError {
    #[error("HTTP error: {0}")]
    Http(String),

    #[error("Network error: {0}")]
    Network(String),

    #[error("Rate limited (429)")]
    RateLimited,

    #[error("Exchange unavailable (502/503)")]
    Unavailable,

    #[error("Request timeout")]
    Timeout,

    #[error("Order rejected: {0}")]
    Rejected(String),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("Signing error: {0}")]
    Signing(String),

    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("Unknown error: {0}")]
    Unknown(String),
}

impl ExchangeError {
    /// Is this error transient (should retry)?
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::RateLimited | Self::Unavailable | Self::Timeout | Self::Network(_)
        )
    }

    /// Is this a 502 (exchange halted)?
    pub fn is_502(&self) -> bool {
        matches!(self, Self::Unavailable)
    }
}

/// Result of placing an order
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PlaceOrderResult {
    /// Order accepted by exchange
    Accepted {
        exchange_order_id: Option<ExchangeOrderId>,
        /// For IOC orders that are immediately filled, includes fill info
        filled_qty: Option<Qty>,
        avg_fill_px: Option<Price>,
    },
    /// Order rejected by exchange
    Rejected { reason: String },
}

/// A fill from userFills
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    /// Trade ID (from exchange, or derived stable key)
    pub trade_id: TradeId,
    /// Client order ID (if returned by exchange)
    pub client_id: Option<ClientOrderId>,
    /// Exchange order ID
    pub exchange_order_id: Option<ExchangeOrderId>,
    /// Instrument
    pub instrument: InstrumentId,
    /// Side
    pub side: OrderSide,
    /// Fill price
    pub price: Price,
    /// Fill quantity
    pub qty: Qty,
    /// Fee
    pub fee: Fee,
    /// Exchange timestamp (millis)
    pub ts: i64,
}

/// Account balance from exchange
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountBalance {
    pub asset: AssetId,
    pub total: Decimal,
    pub available: Decimal,
}

/// Position from exchange
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangePosition {
    pub instrument: InstrumentId,
    pub qty: Decimal, // signed
    pub avg_entry_px: Option<Price>,
    pub unrealized_pnl: Option<Decimal>,
}

/// Exchange adapter trait - what each exchange must implement.
///
/// All methods are async and return Results to handle network errors.
/// The engine calls these methods and translates results into canonical events.
#[async_trait::async_trait]
pub trait Exchange: Send + Sync + 'static {
    /// Get the exchange ID
    fn exchange_id(&self) -> &ExchangeId;

    /// Get the environment (mainnet/testnet)
    fn environment(&self) -> Environment;

    /// Get the exchange instance key
    fn instance(&self) -> ExchangeInstance {
        ExchangeInstance::new(self.exchange_id().clone(), self.environment())
    }

    /// Initialize the exchange connection and perform startup validations.
    /// Called by the runner before entering the main loop.
    /// Implementations can validate: connectivity, vault ownership, account balance, etc.
    async fn init(&self) -> Result<(), ExchangeError> {
        Ok(()) // default no-op for paper/mock exchanges
    }

    // -------------------------------------------------------------------------
    // Write operations (trading)
    // -------------------------------------------------------------------------

    /// Place multiple limit orders in a single batch API call.
    /// Returns a Vec of results, one for each order in the same order as the input.
    async fn place_orders(
        &self,
        orders: &[OrderInput],
    ) -> Result<Vec<PlaceOrderResult>, ExchangeError>;

    /// Cancel an order by client_id or exchange_order_id.
    async fn cancel_order(
        &self,
        instrument: &InstrumentId,
        market_index: &MarketIndex,
        client_id: &ClientOrderId,
        exchange_order_id: Option<&ExchangeOrderId>,
    ) -> Result<(), ExchangeError>;

    /// Cancel all orders for an instrument.
    /// Returns number of orders canceled.
    async fn cancel_all_orders(
        &self,
        instrument: &InstrumentId,
        market_index: &MarketIndex,
    ) -> Result<u32, ExchangeError>;

    // -------------------------------------------------------------------------
    // Read operations (polling)
    // -------------------------------------------------------------------------

    /// Poll user fills since cursor.
    /// This is the primary execution event source.
    /// cursor is opaque string (implementation-specific).
    async fn poll_user_fills(&self, cursor: Option<&str>) -> Result<Vec<Fill>, ExchangeError>;

    /// Poll current quotes/prices.
    async fn poll_quotes(
        &self,
        instruments: &[InstrumentId],
    ) -> Result<Vec<crate::types::Quote>, ExchangeError>;

    /// Poll account state (absolute).
    /// Used for snapshot-based synchronization.
    async fn poll_account_state(&self) -> Result<AccountState, ExchangeError>;
}
