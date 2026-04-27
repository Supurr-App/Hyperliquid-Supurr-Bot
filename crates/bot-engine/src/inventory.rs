//! Inventory ledger: tracks balances and reservations.

use bot_core::{AssetId, Balance, ClientOrderId, Fee, OrderSide, Price, Qty, StrategyId};
use rust_decimal::Decimal;
use std::collections::HashMap;

/// Reservation for an order
#[derive(Debug, Clone)]
pub struct Reservation {
    pub asset: AssetId,
    pub amount: Decimal,
}

/// Inventory ledger managing balances and reservations.
pub struct InventoryLedger {
    /// Account-level balances per asset
    balances: HashMap<AssetId, AccountBalance>,

    /// Reservations by order
    order_reservations: HashMap<ClientOrderId, Vec<Reservation>>,

    /// Per-strategy allocations (optional budgeting)
    strategy_allocations: HashMap<StrategyId, StrategyAllocation>,

    /// Per-strategy reserved amounts
    strategy_reserved: HashMap<(StrategyId, AssetId), Decimal>,
}

#[derive(Debug, Clone, Default)]
struct AccountBalance {
    total: Decimal,
    reserved: Decimal,
}

impl AccountBalance {
    fn available(&self) -> Decimal {
        self.total - self.reserved
    }
}

/// Per-strategy allocation/budget
#[derive(Debug, Clone)]
pub struct StrategyAllocation {
    pub strategy_id: StrategyId,
    pub budgets: HashMap<AssetId, Decimal>,
}

impl InventoryLedger {
    pub fn new() -> Self {
        Self {
            balances: HashMap::new(),
            order_reservations: HashMap::new(),
            strategy_allocations: HashMap::new(),
            strategy_reserved: HashMap::new(),
        }
    }

    /// Set the total balance for an asset (from exchange polling)
    pub fn set_balance(&mut self, asset: &AssetId, total: Decimal) {
        let entry = self.balances.entry(asset.clone()).or_default();
        entry.total = total;
    }

    /// Get balance for an asset
    pub fn balance(&self, asset: &AssetId) -> Balance {
        if let Some(b) = self.balances.get(asset) {
            Balance {
                total: b.total,
                available: b.available(),
                reserved: b.reserved,
            }
        } else {
            Balance::zero()
        }
    }

    /// Reserve funds for an order
    /// Returns true if reservation succeeded, false if insufficient funds
    pub fn reserve(&mut self, order_id: &ClientOrderId, asset: &AssetId, amount: Decimal) -> bool {
        let entry = self.balances.entry(asset.clone()).or_default();

        if entry.available() < amount {
            return false;
        }

        entry.reserved += amount;

        let reservations = self.order_reservations.entry(order_id.clone()).or_default();
        reservations.push(Reservation {
            asset: asset.clone(),
            amount,
        });

        true
    }

    /// Release all reservations for an order
    pub fn release_order(&mut self, order_id: &ClientOrderId) {
        if let Some(reservations) = self.order_reservations.remove(order_id) {
            for res in reservations {
                if let Some(balance) = self.balances.get_mut(&res.asset) {
                    balance.reserved = (balance.reserved - res.amount).max(Decimal::ZERO);
                }
            }
        }
    }

    /// Apply a fill to inventory (spot-style accounting)
    pub fn apply_fill(
        &mut self,
        side: OrderSide,
        base_asset: &AssetId,
        quote_asset: &AssetId,
        qty: Qty,
        price: Price,
        fee: &Fee,
    ) {
        let notional = qty.0 * price.0;

        match side {
            OrderSide::Buy => {
                // BUY: base increases, quote decreases
                let base_entry = self.balances.entry(base_asset.clone()).or_default();
                let base_increase = if fee.asset == *base_asset {
                    qty.0 - fee.amount
                } else {
                    qty.0
                };
                base_entry.total += base_increase;

                let quote_entry = self.balances.entry(quote_asset.clone()).or_default();
                let quote_decrease = if fee.asset == *quote_asset {
                    notional + fee.amount
                } else {
                    notional
                };
                quote_entry.total = (quote_entry.total - quote_decrease).max(Decimal::ZERO);
            }
            OrderSide::Sell => {
                // SELL: base decreases, quote increases
                let base_entry = self.balances.entry(base_asset.clone()).or_default();
                let base_decrease = if fee.asset == *base_asset {
                    qty.0 + fee.amount
                } else {
                    qty.0
                };
                base_entry.total = (base_entry.total - base_decrease).max(Decimal::ZERO);

                let quote_entry = self.balances.entry(quote_asset.clone()).or_default();
                let quote_increase = if fee.asset == *quote_asset {
                    notional - fee.amount
                } else {
                    notional
                };
                quote_entry.total += quote_increase.max(Decimal::ZERO);
            }
        }
    }

    /// Partially release reservation proportional to a fill
    pub fn partial_release(
        &mut self,
        order_id: &ClientOrderId,
        asset: &AssetId,
        filled_fraction: Decimal,
    ) {
        if let Some(reservations) = self.order_reservations.get_mut(order_id) {
            for res in reservations.iter_mut() {
                if res.asset == *asset {
                    let release_amount = res.amount * filled_fraction;
                    res.amount -= release_amount;
                    if let Some(balance) = self.balances.get_mut(asset) {
                        balance.reserved = (balance.reserved - release_amount).max(Decimal::ZERO);
                    }
                }
            }
        }
    }

    /// Register a strategy allocation
    pub fn set_allocation(&mut self, allocation: StrategyAllocation) {
        self.strategy_allocations
            .insert(allocation.strategy_id.clone(), allocation);
    }

    /// Check if a strategy has budget for an amount
    pub fn check_strategy_budget(
        &self,
        strategy_id: &StrategyId,
        asset: &AssetId,
        amount: Decimal,
    ) -> bool {
        if let Some(allocation) = self.strategy_allocations.get(strategy_id) {
            if let Some(&budget) = allocation.budgets.get(asset) {
                let reserved = self
                    .strategy_reserved
                    .get(&(strategy_id.clone(), asset.clone()))
                    .copied()
                    .unwrap_or_default();
                return reserved + amount <= budget;
            }
        }
        // No allocation = no limit
        true
    }
}

impl Default for InventoryLedger {
    fn default() -> Self {
        Self::new()
    }
}
