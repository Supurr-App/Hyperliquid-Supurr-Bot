//! Market maker internal state.

use bot_core::{ClientOrderId, OrderSide, Price, Qty};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Order state machine for each side (BUY/SELL)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderState {
    /// No order, ready to place
    Idle,
    /// Order submitted, waiting for acceptance
    Placed,
    /// Order accepted and live on exchange
    Active,
    /// Cancel requested, waiting for confirmation
    CancelPending,
}

impl Default for OrderState {
    fn default() -> Self {
        Self::Idle
    }
}

/// State for one side of the market maker (BUY or SELL)
#[derive(Debug, Clone, Default)]
pub struct SideState {
    /// Current state machine state
    pub state: OrderState,
    /// Client order ID (if any)
    pub order_id: Option<ClientOrderId>,
    /// Price of the current/pending order
    pub price: Option<Price>,
    /// Size of the current/pending order
    pub size: Option<Qty>,
}

impl SideState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset to IDLE state
    pub fn reset(&mut self) {
        self.state = OrderState::Idle;
        self.order_id = None;
        self.price = None;
        self.size = None;
    }

    /// Transition to PLACED state
    pub fn set_placed(&mut self, order_id: ClientOrderId, price: Price, size: Qty) {
        self.state = OrderState::Placed;
        self.order_id = Some(order_id);
        self.price = Some(price);
        self.size = Some(size);
    }

    /// Transition to ACTIVE state
    pub fn set_active(&mut self) {
        if self.state == OrderState::Placed {
            self.state = OrderState::Active;
        }
    }

    /// Transition to CANCEL_PENDING state
    pub fn set_cancel_pending(&mut self) {
        if self.state == OrderState::Active {
            self.state = OrderState::CancelPending;
        }
    }

    /// Check if we can place a new order
    pub fn can_place(&self) -> bool {
        self.state == OrderState::Idle
    }

    /// Check if there's a pending cancel
    pub fn is_cancel_pending(&self) -> bool {
        self.state == OrderState::CancelPending
    }
}

/// Inventory metrics computed from position
#[derive(Debug, Clone, Default)]
pub struct InventoryMetrics {
    /// Current position quantity (signed)
    pub current_qty: Decimal,
    /// Position as percentage of max (-1 to +1)
    pub position_pct: Decimal,
    /// Normalized inventory ratio (0 to 1)
    pub inventory_ratio: Decimal,
    /// Imbalance from target (-1 to +1)
    pub imbalance: Decimal,
}

/// Skew adjustments computed from inventory
#[derive(Debug, Clone)]
pub struct SkewAdjustments {
    /// Price skew (applied to both bid and ask)
    pub price_skew: Decimal,
    /// Buy size multiplier
    pub buy_size_mult: Decimal,
    /// Sell size multiplier
    pub sell_size_mult: Decimal,
}

impl Default for SkewAdjustments {
    fn default() -> Self {
        Self {
            price_skew: Decimal::ZERO,
            buy_size_mult: Decimal::ONE,
            sell_size_mult: Decimal::ONE,
        }
    }
}

/// Full market maker state
#[derive(Debug, Clone)]
pub struct MarketMakerState {
    // Price tracking
    pub mid_price: Option<Price>,
    pub last_refresh_price: Option<Price>,
    pub last_log_ts: i64,

    // Position tracking
    pub current_position: Decimal,

    // Order tracking with state machine
    pub buy_side: SideState,
    pub sell_side: SideState,

    // Order registry for lookups: ClientOrderId -> OrderSide
    pub order_registry: HashMap<String, OrderSide>,

    // Computed values (cached for logging)
    pub inventory_metrics: InventoryMetrics,
    pub skew_adjustments: SkewAdjustments,

    // Exit state
    pub exit_reason: Option<String>,
    pub current_pnl: Option<Decimal>,
}

impl MarketMakerState {
    pub fn new() -> Self {
        Self {
            mid_price: None,
            last_refresh_price: None,
            last_log_ts: 0,

            current_position: Decimal::ZERO,

            buy_side: SideState::new(),
            sell_side: SideState::new(),

            order_registry: HashMap::new(),

            inventory_metrics: InventoryMetrics::default(),
            skew_adjustments: SkewAdjustments::default(),

            exit_reason: None,
            current_pnl: None,
        }
    }

    /// Reset all order state
    pub fn reset_all_orders(&mut self) {
        self.buy_side.reset();
        self.sell_side.reset();
        self.order_registry.clear();
    }

    /// Register an order in the registry
    pub fn register_order(&mut self, client_id: &ClientOrderId, side: OrderSide) {
        self.order_registry.insert(client_id.0.clone(), side);
    }

    /// Unregister an order and return its side
    pub fn unregister_order(&mut self, client_id: &ClientOrderId) -> Option<OrderSide> {
        self.order_registry.remove(&client_id.0)
    }

    /// Look up which side an order belongs to
    pub fn order_side(&self, client_id: &ClientOrderId) -> Option<OrderSide> {
        self.order_registry.get(&client_id.0).copied()
    }

    /// Get mutable reference to the appropriate side state
    pub fn side_mut(&mut self, side: OrderSide) -> &mut SideState {
        match side {
            OrderSide::Buy => &mut self.buy_side,
            OrderSide::Sell => &mut self.sell_side,
        }
    }

    /// Get immutable reference to the appropriate side state
    pub fn side(&self, side: OrderSide) -> &SideState {
        match side {
            OrderSide::Buy => &self.buy_side,
            OrderSide::Sell => &self.sell_side,
        }
    }
}

impl Default for MarketMakerState {
    fn default() -> Self {
        Self::new()
    }
}





