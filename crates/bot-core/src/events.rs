//! Events: canonical events emitted by the engine to strategies.

use crate::types::*;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// All events that strategies can receive
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    // Market data
    Quote(QuoteEvent),
    FundingRate(FundingRateEvent),

    // Execution (order lifecycle)
    OrderAccepted(OrderAcceptedEvent),
    OrderRejected(OrderRejectedEvent),
    OrderFilled(OrderFilledEvent),
    OrderCompleted(OrderCompletedEvent),
    OrderCanceled(OrderCanceledEvent),

    // System
    ExchangeStateChanged(ExchangeStateChangedEvent),
}

impl Event {
    /// Get the timestamp of this event
    pub fn ts(&self) -> i64 {
        match self {
            Event::Quote(e) => e.ts,
            Event::FundingRate(e) => e.ts,
            Event::OrderAccepted(e) => e.ts,
            Event::OrderRejected(e) => e.ts,
            Event::OrderFilled(e) => e.ts,
            Event::OrderCompleted(e) => e.ts,
            Event::OrderCanceled(e) => e.ts,
            Event::ExchangeStateChanged(e) => e.ts,
        }
    }

    /// Get the instrument ID if applicable
    pub fn instrument(&self) -> Option<&InstrumentId> {
        match self {
            Event::Quote(e) => Some(&e.instrument),
            Event::FundingRate(e) => Some(&e.instrument),
            Event::OrderAccepted(e) => Some(&e.instrument),
            Event::OrderRejected(e) => Some(&e.instrument),
            Event::OrderFilled(e) => Some(&e.instrument),
            Event::OrderCompleted(e) => Some(&e.instrument),
            Event::OrderCanceled(e) => Some(&e.instrument),
            Event::ExchangeStateChanged(_) => None,
        }
    }
}

// =============================================================================
// Market Events
// =============================================================================

/// Quote update (bid/ask)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteEvent {
    pub exchange: ExchangeId,
    pub instrument: InstrumentId,
    pub bid: Price,
    pub ask: Price,
    pub ts: i64,
}

impl QuoteEvent {
    pub fn mid(&self) -> Price {
        Price((self.bid.0 + self.ask.0) / Decimal::TWO)
    }
}

/// Funding rate changed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FundingRateEvent {
    pub exchange: ExchangeId,
    pub instrument: InstrumentId,
    pub rate: Decimal,
    pub ts: i64,
}

// =============================================================================
// Execution Events
// =============================================================================

/// Order accepted by exchange
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderAcceptedEvent {
    pub exchange: ExchangeId,
    pub instrument: InstrumentId,
    pub client_id: ClientOrderId,
    pub exchange_order_id: Option<ExchangeOrderId>,
    pub ts: i64,
}

/// Order rejected by exchange (or locally by engine)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderRejectedEvent {
    pub exchange: ExchangeId,
    pub instrument: InstrumentId,
    pub client_id: ClientOrderId,
    pub reason: String,
    pub ts: i64,
}

/// Order filled (partial or full) - derived from userFills
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderFilledEvent {
    pub exchange: ExchangeId,
    pub instrument: InstrumentId,
    pub client_id: ClientOrderId,
    pub trade_id: TradeId,
    pub side: OrderSide,
    pub price: Price,
    /// Gross quantity filled (as reported by exchange)
    pub qty: Qty,
    /// Net quantity received/spent after fee deduction.
    /// For spot BUY: qty - fee (if fee is in base asset)
    /// For spot SELL: qty (fee is in quote asset)
    /// For perps: same as qty (fees don't affect position size)
    pub net_qty: Qty,
    pub fee: Fee,
    pub ts: i64,
}

/// Order completed (terminal: fully filled)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderCompletedEvent {
    pub exchange: ExchangeId,
    pub instrument: InstrumentId,
    pub client_id: ClientOrderId,
    pub filled_qty: Qty,
    pub avg_fill_px: Option<Price>,
    pub ts: i64,
}

/// Order canceled
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderCanceledEvent {
    pub exchange: ExchangeId,
    pub instrument: InstrumentId,
    pub client_id: ClientOrderId,
    pub reason: Option<String>,
    pub ts: i64,
}

// =============================================================================
// System Events
// =============================================================================

/// Exchange state changed (Active <-> Halted)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeStateChangedEvent {
    pub exchange: ExchangeId,
    pub old_state: ExchangeHealth,
    pub new_state: ExchangeHealth,
    pub reason: String,
    pub ts: i64,
}
