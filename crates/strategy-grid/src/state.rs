//! Grid strategy internal state.

use bot_core::{ClientOrderId, OrderSide, Price, Qty};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// State machine for a grid level.
///
/// Each level goes through this lifecycle:
/// ```text
/// IDLE → OPEN_PLACED → OPEN_FILLED → CLOSE_PLACED → IDLE (cycle repeats)
///   │                       │
///   └── order rejected ─────┴── order canceled → IDLE
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GridLevelState {
    /// No active order, ready to place open order
    Idle,
    /// Open order placed, waiting for fill
    OpenPlaced,
    /// Open order filled, ready to place close (take profit) order
    OpenFilled,
    /// Close order placed, waiting for fill
    ClosePlaced,
}

impl Default for GridLevelState {
    fn default() -> Self {
        Self::Idle
    }
}

/// Represents the kind of order at a grid level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderKind {
    /// Open order (entry into position)
    Open,
    /// Close order (take profit / exit position)
    Close,
}

/// A single grid level with its state and order tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridLevel {
    /// Level index (0-based, lower index = lower price for LONG mode)
    pub index: usize,

    /// Entry price for this level
    pub entry_price: Price,

    /// Take profit price for this level
    pub take_profit_price: Price,

    /// Order quantity for this level
    pub quantity: Qty,

    /// Order side for the open order (BUY for LONG, SELL for SHORT)
    pub side: OrderSide,

    /// Whether this level is active (can place orders)
    /// Some levels are deactivated (e.g., boundary levels, neutral center)
    pub is_active: bool,

    /// Current state of this level
    pub state: GridLevelState,

    /// Client order ID for the open order (if any)
    pub open_order_id: Option<ClientOrderId>,

    /// Client order ID for the close order (if any)
    pub close_order_id: Option<ClientOrderId>,

    /// Total filled quantity for the current open order
    /// Accumulates partial fills until order is complete
    pub filled_open_qty: Qty,
}

impl GridLevel {
    /// Create a new grid level.
    pub fn new(
        index: usize,
        entry_price: Price,
        take_profit_price: Price,
        quantity: Qty,
        side: OrderSide,
    ) -> Self {
        Self {
            index,
            entry_price,
            take_profit_price,
            quantity,
            side,
            is_active: true,
            state: GridLevelState::Idle,
            open_order_id: None,
            close_order_id: None,
            filled_open_qty: Qty::new(Decimal::ZERO),
        }
    }

    /// Reset level to IDLE state, clearing order tracking.
    pub fn reset(&mut self) {
        self.state = GridLevelState::Idle;
        self.open_order_id = None;
        self.close_order_id = None;
        self.filled_open_qty = Qty::new(Decimal::ZERO);
    }

    /// Check if this level can place an open order.
    pub fn can_place_open(&self) -> bool {
        self.is_active && self.state == GridLevelState::Idle && self.open_order_id.is_none()
    }

    /// Check if this level can place a close order.
    pub fn can_place_close(&self) -> bool {
        self.is_active
            && self.state == GridLevelState::OpenFilled
            && self.close_order_id.is_none()
            && self.filled_open_qty.0 > Decimal::ZERO
    }

    /// Transition to OPEN_PLACED state.
    pub fn set_open_placed(&mut self, order_id: ClientOrderId) {
        self.state = GridLevelState::OpenPlaced;
        self.open_order_id = Some(order_id);
    }

    /// Transition to OPEN_FILLED state.
    pub fn set_open_filled(&mut self) {
        self.state = GridLevelState::OpenFilled;
        self.open_order_id = None;
    }

    /// Transition to CLOSE_PLACED state.
    pub fn set_close_placed(&mut self, order_id: ClientOrderId) {
        self.state = GridLevelState::ClosePlaced;
        self.close_order_id = Some(order_id);
    }

    /// Get the close order side (opposite of open side).
    pub fn close_side(&self) -> OrderSide {
        self.side.opposite()
    }
}

/// Liquidation safety boundaries.
///
/// These are computed at grid initialization to ensure the grid
/// doesn't risk liquidation within the trading range.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LiquidationBoundaries {
    /// Minimum price before long liquidation (for LONG/NEUTRAL modes)
    pub min_safe_price: Option<Price>,

    /// Maximum price before short liquidation (for SHORT/NEUTRAL modes)
    pub max_safe_price: Option<Price>,
}

/// Full grid strategy state.
#[derive(Debug, Clone)]
pub struct GridState {
    // -------------------------------------------------------------------------
    // Price Tracking
    // -------------------------------------------------------------------------
    /// Current mid price from quotes
    pub mid_price: Option<Price>,

    /// Last logged timestamp (for periodic logging)
    pub last_log_ts: i64,

    // -------------------------------------------------------------------------
    // Grid Levels
    // -------------------------------------------------------------------------
    /// All grid levels
    pub levels: Vec<GridLevel>,

    /// Calculated grid step (price between adjacent levels)
    pub grid_step: Decimal,

    /// Quote amount per level
    pub quote_per_level: Decimal,

    // -------------------------------------------------------------------------
    // Order Registry
    // -------------------------------------------------------------------------
    /// Maps ClientOrderId -> (level_index, OrderKind)
    /// Used to look up which level an order belongs to
    pub order_registry: HashMap<String, (usize, OrderKind)>,

    // -------------------------------------------------------------------------
    // Safety Boundaries
    // -------------------------------------------------------------------------
    /// Liquidation safety boundaries
    pub liquidation_boundaries: LiquidationBoundaries,

    // -------------------------------------------------------------------------
    // Exit State
    // -------------------------------------------------------------------------
    /// Exit reason (if strategy should stop)
    pub exit_reason: Option<String>,

    // -------------------------------------------------------------------------
    // Initialization State
    // -------------------------------------------------------------------------
    /// Whether the grid has been initialized with levels
    pub is_initialized: bool,

    // -------------------------------------------------------------------------
    // Sync Optimization
    // -------------------------------------------------------------------------
    /// Set of level indices that need order sync (dirty tracking)
    /// More efficient than boolean flag - only sync specific levels that changed
    pub dirty_levels: HashSet<usize>,

    // -------------------------------------------------------------------------
    // Trailing Slide State
    // -------------------------------------------------------------------------
    /// Millisecond timestamp of last window slide (for cooldown enforcement)
    pub last_slide_ts: i64,
}

impl GridState {
    /// Create a new empty grid state.
    pub fn new() -> Self {
        Self {
            mid_price: None,
            last_log_ts: 0,
            levels: Vec::new(),
            grid_step: Decimal::ZERO,
            quote_per_level: Decimal::ZERO,
            order_registry: HashMap::new(),
            liquidation_boundaries: LiquidationBoundaries::default(),
            exit_reason: None,

            is_initialized: false,
            dirty_levels: HashSet::new(),
            last_slide_ts: 0,
        }
    }

    /// Mark a level as needing sync
    pub fn mark_dirty(&mut self, level_idx: usize) {
        self.dirty_levels.insert(level_idx);
    }

    /// Mark all active levels as dirty (for initial sync)
    pub fn mark_all_dirty(&mut self) {
        for level in &self.levels {
            if level.is_active {
                self.dirty_levels.insert(level.index);
            }
        }
    }

    /// Clear dirty tracking after sync
    pub fn clear_dirty(&mut self) {
        self.dirty_levels.clear();
    }

    /// Check if any levels need sync
    pub fn has_dirty_levels(&self) -> bool {
        !self.dirty_levels.is_empty()
    }

    /// Register an order in the registry.
    pub fn register_order(
        &mut self,
        client_id: &ClientOrderId,
        level_index: usize,
        kind: OrderKind,
    ) {
        self.order_registry
            .insert(client_id.0.clone(), (level_index, kind));
    }

    /// Unregister an order and return its (level_index, kind) if found.
    pub fn unregister_order(&mut self, client_id: &ClientOrderId) -> Option<(usize, OrderKind)> {
        self.order_registry.remove(&client_id.0)
    }

    /// Look up which level and order kind an order belongs to.
    pub fn order_info(&self, client_id: &ClientOrderId) -> Option<(usize, OrderKind)> {
        self.order_registry.get(&client_id.0).copied()
    }

    /// Get a mutable reference to a level by index.
    pub fn level_mut(&mut self, index: usize) -> Option<&mut GridLevel> {
        self.levels.get_mut(index)
    }

    /// Get an immutable reference to a level by index.
    pub fn level(&self, index: usize) -> Option<&GridLevel> {
        self.levels.get(index)
    }

    /// Reset all levels to IDLE state.
    pub fn reset_all_levels(&mut self) {
        for level in &mut self.levels {
            level.reset();
        }
        self.order_registry.clear();
    }

    /// Perform a synchronized re-indexing of all levels and supporting
    /// data structures after an array rotation (slide up or down).
    ///
    /// When levels are physically shifted in the Vec (e.g. `remove(0)` /
    /// `insert(0, …)`), every `level.index` and every entry in
    /// `order_registry` and `dirty_levels` that holds a `usize` position
    /// becomes stale.  This helper corrects them all in one pass.
    ///
    /// `offset`: `1` for slide-up (indices decreased), `-1` for slide-down
    /// (indices increased). Caller is responsible for appending/prepending
    /// the moved level *before* calling this.
    pub fn reindex_after_shift(&mut self, offset: i64) {
        // 1. Fix level.index on every element in the Vec
        for level in &mut self.levels {
            let new_idx = (level.index as i64 + offset) as usize;
            level.index = new_idx;
        }

        // 2. Rebuild order_registry with shifted indices
        let updated_registry: HashMap<String, (usize, OrderKind)> = self
            .order_registry
            .drain()
            .filter_map(|(client_id, (old_idx, kind))| {
                let new_idx = old_idx as i64 + offset;
                if new_idx < 0 {
                    // This order belonged to the level that was evicted; discard.
                    None
                } else {
                    Some((client_id, (new_idx as usize, kind)))
                }
            })
            .collect();
        self.order_registry = updated_registry;

        // 3. Rebuild dirty_levels with shifted indices
        let updated_dirty: HashSet<usize> = self
            .dirty_levels
            .drain()
            .filter_map(|old_idx| {
                let new_idx = old_idx as i64 + offset;
                if new_idx < 0 {
                    None // evicted level entry
                } else {
                    Some(new_idx as usize)
                }
            })
            .collect();
        self.dirty_levels = updated_dirty;
    }

    /// Count levels in each state (for logging).
    pub fn level_state_counts(&self) -> (usize, usize, usize, usize) {
        let mut idle = 0;
        let mut open_placed = 0;
        let mut open_filled = 0;
        let mut close_placed = 0;

        for level in &self.levels {
            if !level.is_active {
                continue;
            }
            match level.state {
                GridLevelState::Idle => idle += 1,
                GridLevelState::OpenPlaced => open_placed += 1,
                GridLevelState::OpenFilled => open_filled += 1,
                GridLevelState::ClosePlaced => close_placed += 1,
            }
        }

        (idle, open_placed, open_filled, close_placed)
    }

    /// Check if price is within liquidation safety boundaries.
    pub fn is_price_safe(&self, price: Price) -> bool {
        if let Some(min) = self.liquidation_boundaries.min_safe_price {
            if price.0 < min.0 {
                return false;
            }
        }
        if let Some(max) = self.liquidation_boundaries.max_safe_price {
            if price.0 > max.0 {
                return false;
            }
        }
        true
    }
}

impl Default for GridState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_level_state_transitions() {
        let mut level = GridLevel::new(
            0,
            Price::new(Decimal::new(80000, 0)),
            Price::new(Decimal::new(80500, 0)),
            Qty::new(Decimal::new(1, 3)),
            OrderSide::Buy,
        );

        assert!(level.can_place_open());
        assert!(!level.can_place_close());

        level.set_open_placed(ClientOrderId::new("test-1"));
        assert_eq!(level.state, GridLevelState::OpenPlaced);
        assert!(!level.can_place_open());

        level.filled_open_qty = Qty::new(Decimal::new(1, 3));
        level.set_open_filled();
        assert_eq!(level.state, GridLevelState::OpenFilled);
        assert!(level.can_place_close());

        level.set_close_placed(ClientOrderId::new("test-2"));
        assert_eq!(level.state, GridLevelState::ClosePlaced);
        assert!(!level.can_place_close());

        level.reset();
        assert_eq!(level.state, GridLevelState::Idle);
        assert!(level.can_place_open());
    }

    #[test]
    fn test_order_registry() {
        let mut state = GridState::new();
        let order_id = ClientOrderId::new("0x123");

        state.register_order(&order_id, 5, OrderKind::Open);
        assert_eq!(state.order_info(&order_id), Some((5, OrderKind::Open)));

        let info = state.unregister_order(&order_id);
        assert_eq!(info, Some((5, OrderKind::Open)));
        assert_eq!(state.order_info(&order_id), None);
    }

    #[test]
    fn test_liquidation_boundary_checks() {
        let mut state = GridState::new();
        state.liquidation_boundaries.min_safe_price = Some(Price::new(Decimal::new(75000, 0)));
        state.liquidation_boundaries.max_safe_price = Some(Price::new(Decimal::new(95000, 0)));

        assert!(state.is_price_safe(Price::new(Decimal::new(80000, 0))));
        assert!(!state.is_price_safe(Price::new(Decimal::new(70000, 0))));
        assert!(!state.is_price_safe(Price::new(Decimal::new(100000, 0))));
    }
}
