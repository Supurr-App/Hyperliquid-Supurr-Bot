//! Order manager: tracks order state and transitions.

use bot_core::{
    now_ms, ClientOrderId, ExchangeOrderId, InstrumentId, LiveOrder, OrderSide, OrderStatus, Price,
    Qty, TradeId,
};
use std::collections::{HashMap, HashSet};

/// Manages order state and fill deduplication.
pub struct OrderManager {
    /// All tracked orders by client_id
    orders: HashMap<ClientOrderId, LiveOrder>,

    /// Map exchange_order_id -> client_id (for fills that don't include cloid)
    exchange_id_map: HashMap<ExchangeOrderId, ClientOrderId>,

    /// Seen trade IDs for deduplication (bounded LRU-style)
    seen_trades: HashSet<String>,
    seen_trades_order: Vec<String>,
    max_seen_trades: usize,
}

impl OrderManager {
    pub fn new() -> Self {
        Self {
            orders: HashMap::new(),
            exchange_id_map: HashMap::new(),
            seen_trades: HashSet::new(),
            seen_trades_order: Vec::new(),
            max_seen_trades: 10000,
        }
    }

    /// Get an order by client_id
    pub fn get(&self, client_id: &ClientOrderId) -> Option<&LiveOrder> {
        self.orders.get(client_id)
    }

    /// Get a mutable order by client_id
    pub fn get_mut(&mut self, client_id: &ClientOrderId) -> Option<&mut LiveOrder> {
        self.orders.get_mut(client_id)
    }

    /// Check if we're tracking an order
    pub fn contains(&self, client_id: &ClientOrderId) -> bool {
        self.orders.contains_key(client_id)
    }

    /// Create a new order (when PlaceOrder command is emitted)
    pub fn create_order(
        &mut self,
        client_id: ClientOrderId,
        instrument: InstrumentId,
        side: OrderSide,
        price: Price,
        qty: Qty,
    ) -> &LiveOrder {
        let order = LiveOrder {
            client_id: client_id.clone(),
            exchange_order_id: None,
            instrument,
            side,
            price,
            requested_qty: qty,
            filled_qty: Qty::new(rust_decimal::Decimal::ZERO),
            avg_fill_px: None,
            status: OrderStatus::New,
            ts_created: now_ms(),
            ts_last_update: now_ms(),
        };
        self.orders.insert(client_id.clone(), order);
        self.orders.get(&client_id).unwrap()
    }

    /// Mark order as accepted
    pub fn accept_order(
        &mut self,
        client_id: &ClientOrderId,
        exchange_order_id: Option<ExchangeOrderId>,
    ) -> bool {
        if let Some(order) = self.orders.get_mut(client_id) {
            order.status = OrderStatus::Accepted;
            order.ts_last_update = now_ms();
            if let Some(eid) = exchange_order_id {
                order.exchange_order_id = Some(eid.clone());
                self.exchange_id_map.insert(eid, client_id.clone());
            }
            true
        } else {
            false
        }
    }

    /// Mark order as rejected and remove it
    pub fn reject_order(&mut self, client_id: &ClientOrderId) -> Option<LiveOrder> {
        if let Some(mut order) = self.orders.remove(client_id) {
            order.status = OrderStatus::Rejected;
            order.ts_last_update = now_ms();
            if let Some(ref eid) = order.exchange_order_id {
                self.exchange_id_map.remove(eid);
            }
            Some(order)
        } else {
            None
        }
    }

    /// Apply a fill to an order
    /// Returns true if this is a new fill (not a duplicate)
    pub fn apply_fill(
        &mut self,
        client_id: &ClientOrderId,
        trade_id: &TradeId,
        fill_qty: Qty,
        fill_px: Price,
    ) -> bool {
        // Dedupe check
        if self.seen_trades.contains(&trade_id.0) {
            return false;
        }

        // Add to seen trades (with LRU eviction)
        self.seen_trades.insert(trade_id.0.clone());
        self.seen_trades_order.push(trade_id.0.clone());
        if self.seen_trades_order.len() > self.max_seen_trades {
            if let Some(old) = self.seen_trades_order.first().cloned() {
                self.seen_trades.remove(&old);
                self.seen_trades_order.remove(0);
            }
        }

        // Apply to order
        if let Some(order) = self.orders.get_mut(client_id) {
            // Update filled qty
            order.filled_qty += fill_qty;

            // Update average fill price (weighted)
            let old_notional = order.avg_fill_px.map(|p| p.0).unwrap_or_default()
                * (order.filled_qty.0 - fill_qty.0);
            let new_notional = fill_px.0 * fill_qty.0;
            let total_qty = order.filled_qty.0;
            if total_qty > rust_decimal::Decimal::ZERO {
                order.avg_fill_px = Some(Price((old_notional + new_notional) / total_qty));
            }

            // Update status
            if order.filled_qty >= order.requested_qty {
                order.status = OrderStatus::Filled;
            } else {
                order.status = OrderStatus::PartiallyFilled;
            }

            order.ts_last_update = now_ms();
            true
        } else {
            // Unknown order - might be from before our tracking started
            false
        }
    }

    /// Mark order as canceled and remove it
    pub fn cancel_order(&mut self, client_id: &ClientOrderId) -> Option<LiveOrder> {
        if let Some(mut order) = self.orders.remove(client_id) {
            order.status = OrderStatus::Canceled;
            order.ts_last_update = now_ms();
            if let Some(ref eid) = order.exchange_order_id {
                self.exchange_id_map.remove(eid);
            }
            Some(order)
        } else {
            None
        }
    }

    /// Check if an order is complete (fully filled)
    pub fn is_complete(&self, client_id: &ClientOrderId) -> bool {
        self.orders
            .get(client_id)
            .map(|o| o.is_complete())
            .unwrap_or(false)
    }

    /// Remove completed/terminal orders
    pub fn remove_terminal(&mut self, client_id: &ClientOrderId) -> Option<LiveOrder> {
        if let Some(order) = self.orders.get(client_id) {
            if order.status.is_terminal() {
                let order = self.orders.remove(client_id)?;
                if let Some(ref eid) = order.exchange_order_id {
                    self.exchange_id_map.remove(eid);
                }
                return Some(order);
            }
        }
        None
    }

    /// Look up client_id from exchange_order_id
    pub fn client_id_from_exchange_id(
        &self,
        exchange_id: &ExchangeOrderId,
    ) -> Option<&ClientOrderId> {
        self.exchange_id_map.get(exchange_id)
    }

    /// Check if a trade_id has been seen (for external dedupe)
    pub fn is_trade_seen(&self, trade_id: &TradeId) -> bool {
        self.seen_trades.contains(&trade_id.0)
    }

    /// Get the number of orders currently tracked
    pub fn order_count(&self) -> usize {
        self.orders.len()
    }
}

impl Default for OrderManager {
    fn default() -> Self {
        Self::new()
    }
}
