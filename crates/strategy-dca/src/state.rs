//! DCA strategy internal state.

use crate::config::DCADirection;
use bot_core::{ClientOrderId, OrderSide, Price, Qty};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Phase of the DCA strategy lifecycle.
///
/// Simplified flow with limit orders placed upfront:
/// ```text
/// PLACING_ORDERS → ACTIVE → (on each fill: update TP) → COMPLETED
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DCAPhase {
    /// Initial phase: placing all DCA limit orders
    PlacingOrders,
    /// All orders placed, waiting for fills and managing TP
    Active,
    /// Take profit filled or stopped
    Completed,
    /// Cooldown period between cycles (when restart_on_complete is true)
    Cooldown,
}

impl Default for DCAPhase {
    fn default() -> Self {
        Self::PlacingOrders
    }
}

/// State of an individual DCA order in the ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DCAOrderState {
    /// Order not yet placed
    Pending,
    /// Order placed on exchange, waiting for fill
    Placed,
    /// Order filled (partially or fully)
    Filled,
}

impl Default for DCAOrderState {
    fn default() -> Self {
        Self::Pending
    }
}

/// Represents a single order in the DCA ladder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DCAOrder {
    /// Order index (0 = base order, 1..n = DCA orders)
    pub index: usize,

    /// Limit order price for this level
    pub limit_price: Price,

    /// Order size for this level
    pub order_size: Qty,

    /// Current state of this order
    pub state: DCAOrderState,

    /// Client order ID (when placed)
    pub client_order_id: Option<ClientOrderId>,

    /// Actual fill price (may differ slightly from limit)
    pub fill_price: Option<Price>,

    /// Filled quantity
    pub filled_qty: Qty,
}

impl DCAOrder {
    /// Create a new DCA order.
    pub fn new(index: usize, limit_price: Price, order_size: Qty) -> Self {
        Self {
            index,
            limit_price,
            order_size,
            state: DCAOrderState::Pending,
            client_order_id: None,
            fill_price: None,
            filled_qty: Qty::new(Decimal::ZERO),
        }
    }

    /// Check if this order needs to be placed.
    pub fn needs_placement(&self) -> bool {
        self.state == DCAOrderState::Pending
    }

    /// Check if this order is waiting for a fill.
    pub fn is_pending_fill(&self) -> bool {
        self.state == DCAOrderState::Placed
    }

    /// Mark as placed.
    pub fn set_placed(&mut self, client_id: ClientOrderId) {
        self.state = DCAOrderState::Placed;
        self.client_order_id = Some(client_id);
    }

    /// Mark as filled.
    pub fn set_filled(&mut self, price: Price, qty: Qty) {
        self.state = DCAOrderState::Filled;
        self.fill_price = Some(price);
        self.filled_qty = qty;
    }

    /// Reset to pending (e.g., after rejection).
    pub fn reset(&mut self) {
        self.state = DCAOrderState::Pending;
        self.client_order_id = None;
        self.fill_price = None;
        self.filled_qty = Qty::new(Decimal::ZERO);
    }
}

/// Deferred TP replacement to place after the previous TP is canceled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingTakeProfit {
    pub price: Price,
    pub qty: Qty,
}

/// Full DCA strategy state.
#[derive(Debug, Clone)]
pub struct DCAState {
    // -------------------------------------------------------------------------
    // Phase & Price Tracking
    // -------------------------------------------------------------------------
    /// Current phase in the DCA lifecycle
    pub phase: DCAPhase,

    /// Current mid price from quotes
    pub mid_price: Option<Price>,

    /// Direction being traded
    pub direction: DCADirection,

    /// Whether this is a spot market (buy-only DCA)
    pub is_spot: bool,

    /// Last logged timestamp (for periodic logging)
    pub last_log_ts: i64,

    // -------------------------------------------------------------------------
    // DCA Ladder
    // -------------------------------------------------------------------------
    /// Pre-computed DCA orders (index 0 = base order)
    pub orders: Vec<DCAOrder>,

    // -------------------------------------------------------------------------
    // Position Tracking
    // -------------------------------------------------------------------------
    /// Total filled quantity across all orders
    pub total_filled_qty: Qty,

    /// Total cost (sum of price × qty for each fill)
    /// Used to calculate average entry price
    pub total_cost: Decimal,

    /// Calculated average entry price
    pub average_entry_price: Option<Price>,

    /// Current take profit price (updated on each fill)
    pub take_profit_price: Option<Price>,

    // Note: Stop-loss is based on PnL from engine, not a price level

    // -------------------------------------------------------------------------
    // Order Registry
    // -------------------------------------------------------------------------
    /// Maps ClientOrderId -> order index for DCA orders
    pub order_registry: HashMap<String, usize>,

    /// Client order ID for current take profit order
    pub tp_order_id: Option<ClientOrderId>,

    /// TP order ID that has a cancel request in flight.
    pub tp_cancel_in_flight: Option<ClientOrderId>,

    /// Latest TP replacement spec waiting for cancel acknowledgment.
    pub pending_tp_replacement: Option<PendingTakeProfit>,

    /// Client order ID for stop loss order (if triggered)
    pub sl_order_id: Option<ClientOrderId>,

    // -------------------------------------------------------------------------
    // Exit State
    // -------------------------------------------------------------------------
    /// Exit reason (if strategy should stop)
    pub exit_reason: Option<String>,

    /// Whether the strategy has been initialized
    pub is_initialized: bool,

    // -------------------------------------------------------------------------
    // Cooldown State
    // -------------------------------------------------------------------------
    /// Unix timestamp (seconds) when cooldown ends and new cycle can start
    pub cooldown_until: Option<i64>,
}

impl DCAState {
    /// Create a new empty DCA state.
    pub fn new(direction: DCADirection, is_spot: bool) -> Self {
        Self {
            phase: DCAPhase::PlacingOrders,
            mid_price: None,
            direction,
            is_spot,
            last_log_ts: 0,

            orders: Vec::new(),

            total_filled_qty: Qty::new(Decimal::ZERO),
            total_cost: Decimal::ZERO,
            average_entry_price: None,
            take_profit_price: None,

            order_registry: HashMap::new(),
            tp_order_id: None,
            tp_cancel_in_flight: None,
            pending_tp_replacement: None,
            sl_order_id: None,

            exit_reason: None,
            is_initialized: false,
            cooldown_until: None,
        }
    }

    /// Register a DCA order in the registry.
    pub fn register_order(&mut self, client_id: &ClientOrderId, order_index: usize) {
        self.order_registry.insert(client_id.0.clone(), order_index);
    }

    /// Unregister an order and return its index if found.
    pub fn unregister_order(&mut self, client_id: &ClientOrderId) -> Option<usize> {
        self.order_registry.remove(&client_id.0)
    }

    /// Look up which order index a client ID belongs to.
    pub fn order_index(&self, client_id: &ClientOrderId) -> Option<usize> {
        self.order_registry.get(&client_id.0).copied()
    }

    /// Get a mutable reference to an order by index.
    pub fn order_mut(&mut self, index: usize) -> Option<&mut DCAOrder> {
        self.orders.get_mut(index)
    }

    /// Get an immutable reference to an order by index.
    pub fn order(&self, index: usize) -> Option<&DCAOrder> {
        self.orders.get(index)
    }

    /// Update average entry price after a fill.
    pub fn update_average_entry(&mut self, fill_price: Price, fill_qty: Qty) {
        self.total_filled_qty += fill_qty;
        self.total_cost += fill_price.0 * fill_qty.0;

        if self.total_filled_qty.0 > Decimal::ZERO {
            self.average_entry_price = Some(Price::new(self.total_cost / self.total_filled_qty.0));
        }
    }

    /// Calculate take profit price based on average entry.
    pub fn calculate_tp_price(&self, tp_pct: Decimal) -> Option<Price> {
        let avg = self.average_entry_price?;
        let hundred = Decimal::new(100, 0);

        // For spot: always sell TP (above entry)
        // For perps: based on direction
        let tp = if self.is_spot {
            avg.0 * (Decimal::ONE + tp_pct / hundred)
        } else {
            match self.direction {
                DCADirection::Long => avg.0 * (Decimal::ONE + tp_pct / hundred),
                DCADirection::Short => avg.0 * (Decimal::ONE - tp_pct / hundred),
            }
        };

        Some(Price::new(tp))
    }

    /// Calculate stop loss price based on average entry.
    pub fn calculate_sl_price(&self, sl_pct: Decimal) -> Option<Price> {
        let avg = self.average_entry_price?;
        let hundred = Decimal::new(100, 0);

        // sl_pct is negative (e.g., -10 for 10% loss)
        let sl = if self.is_spot {
            // For spot: SL is below entry
            avg.0 * (Decimal::ONE + sl_pct / hundred)
        } else {
            match self.direction {
                DCADirection::Long => avg.0 * (Decimal::ONE + sl_pct / hundred),
                DCADirection::Short => avg.0 * (Decimal::ONE - sl_pct / hundred),
            }
        };

        Some(Price::new(sl))
    }

    /// Update TP price.
    /// Note: SL is now PnL-based (checked in strategy via ctx.position().unrealized_pnl)
    pub fn update_tp_price(&mut self, tp_pct: Decimal) {
        self.take_profit_price = self.calculate_tp_price(tp_pct);
    }

    /// Get the order side for DCA orders (opening position).
    pub fn open_side(&self) -> OrderSide {
        if self.is_spot {
            // Spot: always buy
            OrderSide::Buy
        } else {
            match self.direction {
                DCADirection::Long => OrderSide::Buy,
                DCADirection::Short => OrderSide::Sell,
            }
        }
    }

    /// Get the order side for TP/SL (closing position).
    pub fn close_side(&self) -> OrderSide {
        if self.is_spot {
            // Spot: always sell to exit
            OrderSide::Sell
        } else {
            match self.direction {
                DCADirection::Long => OrderSide::Sell,
                DCADirection::Short => OrderSide::Buy,
            }
        }
    }

    // Note: Stop-loss is now PnL-based, checked in strategy using ctx.position().unrealized_pnl

    /// Check if we have any filled orders.
    pub fn has_position(&self) -> bool {
        self.total_filled_qty.0 > Decimal::ZERO
    }

    /// Count filled orders.
    pub fn filled_orders_count(&self) -> usize {
        self.orders
            .iter()
            .filter(|o| o.state == DCAOrderState::Filled)
            .count()
    }

    /// Count pending orders (not yet placed or filled).
    pub fn pending_orders_count(&self) -> usize {
        self.orders
            .iter()
            .filter(|o| o.state == DCAOrderState::Pending)
            .count()
    }

    /// Check if all orders are placed.
    pub fn all_orders_placed(&self) -> bool {
        self.orders
            .iter()
            .all(|o| o.state != DCAOrderState::Pending)
    }

    /// Check if all orders are filled.
    pub fn all_orders_filled(&self) -> bool {
        self.orders.iter().all(|o| o.state == DCAOrderState::Filled)
    }

    /// Clear all TP order tracking, including deferred replacement state.
    pub fn clear_take_profit_tracking(&mut self) {
        self.tp_order_id = None;
        self.tp_cancel_in_flight = None;
        self.pending_tp_replacement = None;
    }

    /// Reset state for a new cycle.
    pub fn reset_for_new_cycle(&mut self) {
        self.phase = DCAPhase::PlacingOrders;
        self.total_filled_qty = Qty::new(Decimal::ZERO);
        self.total_cost = Decimal::ZERO;
        self.average_entry_price = None;
        self.take_profit_price = None;
        self.order_registry.clear();
        self.clear_take_profit_tracking();
        self.sl_order_id = None;

        for order in &mut self.orders {
            order.reset();
        }
    }
}

impl Default for DCAState {
    fn default() -> Self {
        Self::new(DCADirection::Long, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_average_entry_calculation() {
        let mut state = DCAState::new(DCADirection::Long, false);

        // First fill: 0.01 BTC @ $95,000
        state.update_average_entry(
            Price::new(Decimal::new(95000, 0)),
            Qty::new(Decimal::new(1, 2)),
        );
        assert_eq!(
            state.average_entry_price,
            Some(Price::new(Decimal::new(95000, 0)))
        );

        // Second fill: 0.02 BTC @ $93,000
        state.update_average_entry(
            Price::new(Decimal::new(93000, 0)),
            Qty::new(Decimal::new(2, 2)),
        );
        // Average = (95000 * 0.01 + 93000 * 0.02) / 0.03
        let avg = state.average_entry_price.unwrap().0;
        assert!(avg > Decimal::new(93000, 0));
        assert!(avg < Decimal::new(95000, 0));
    }

    #[test]
    fn test_tp_calculation_long() {
        let mut state = DCAState::new(DCADirection::Long, false);
        state.average_entry_price = Some(Price::new(Decimal::new(100000, 0)));

        let tp = state.calculate_tp_price(Decimal::new(2, 0)).unwrap();
        // TP should be 2% above = 102,000
        assert_eq!(tp, Price::new(Decimal::new(102000, 0)));
    }

    #[test]
    fn test_tp_calculation_short() {
        let mut state = DCAState::new(DCADirection::Short, false);
        state.average_entry_price = Some(Price::new(Decimal::new(100000, 0)));

        let tp = state.calculate_tp_price(Decimal::new(2, 0)).unwrap();
        // TP should be 2% below = 98,000
        assert_eq!(tp, Price::new(Decimal::new(98000, 0)));
    }

    #[test]
    fn test_spot_always_buy_sell() {
        let state = DCAState::new(DCADirection::Long, true); // Spot

        // Spot: always buy for entry, sell for exit
        assert_eq!(state.open_side(), OrderSide::Buy);
        assert_eq!(state.close_side(), OrderSide::Sell);
    }

    #[test]
    fn test_perp_short_direction() {
        let state = DCAState::new(DCADirection::Short, false); // Perp

        // Short perp: sell for entry, buy for exit
        assert_eq!(state.open_side(), OrderSide::Sell);
        assert_eq!(state.close_side(), OrderSide::Buy);
    }
}
