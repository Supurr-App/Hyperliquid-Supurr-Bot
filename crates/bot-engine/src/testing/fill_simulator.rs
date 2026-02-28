//! Fill simulator: Shared logic for simulating order fills.
//!
//! Used by both MockExchange (for test assertions) and PaperExchange (for paper trading).
//!
//! ## Performance Optimization
//!
//! Uses sorted VecDeques (by price) for O(1) front removal when filling orders.
//! This reduces complexity from O(quotes × orders) to O(quotes × fillable_orders).
//! For grid strategies with 100 levels, this provides ~10-15x speedup.

use bot_core::{
    AssetId, ClientOrderId, ExchangeOrderId, Fee, Fill, InstrumentId, OrderSide, Price, Qty, Quote,
    TradeId,
};
use rust_decimal::Decimal;
use std::collections::{HashMap, VecDeque};

/// A pending order waiting to be filled
#[derive(Debug, Clone)]
pub struct PendingOrder {
    pub client_id: ClientOrderId,
    pub exchange_order_id: ExchangeOrderId,
    pub instrument: InstrumentId,
    pub side: OrderSide,
    pub price: Price,
    pub qty: Qty,
    pub remaining_qty: Qty,
    pub created_at: i64,
}

/// Result of a simulated fill
#[derive(Debug, Clone)]
pub struct SimulatedFill {
    pub fill: Fill,
    pub order_fully_filled: bool,
}

/// Key for grouping orders by instrument and side
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct OrderGroupKey {
    instrument: InstrumentId,
    side: OrderSide,
}

/// Simulates order fills based on quote price crossing.
///
/// Shared logic used by both MockExchange and PaperExchange.
///
/// ## Optimization
///
/// Orders are stored in sorted VecDeques by price (per instrument+side).
/// - BUY orders: sorted descending by price (highest first)
/// - SELL orders: sorted ascending by price (lowest first)
///
/// Using VecDeque allows O(1) pop_front for filling orders, avoiding O(n) shifts.
pub struct FillSimulator {
    /// Orders grouped by (instrument, side), sorted by price for fast lookups
    /// BUY orders: sorted descending (highest price first - most aggressive)
    /// SELL orders: sorted ascending (lowest price first - most aggressive)
    order_groups: HashMap<OrderGroupKey, VecDeque<PendingOrder>>,
    next_oid: u64,
    balances: HashMap<AssetId, Decimal>,
    /// Fee rate to apply to fills (e.g., 0.0004 = 0.04%)
    /// For spot BUY orders, fee is deducted from base asset (received asset)
    /// For spot SELL orders, fee is deducted from quote asset
    fee_rate: Decimal,
}

impl FillSimulator {
    pub fn new(initial_balances: HashMap<AssetId, Decimal>) -> Self {
        Self {
            order_groups: HashMap::new(),
            next_oid: 1000,
            balances: initial_balances,
            fee_rate: Decimal::ZERO, // Default: no fees
        }
    }

    /// Create a new fill simulator with a specific fee rate
    pub fn new_with_fee(initial_balances: HashMap<AssetId, Decimal>, fee_rate: Decimal) -> Self {
        Self {
            order_groups: HashMap::new(),
            next_oid: 1000,
            balances: initial_balances,
            fee_rate,
        }
    }

    /// Set the fee rate (0.0004 = 0.04%)
    pub fn set_fee_rate(&mut self, fee_rate: Decimal) {
        self.fee_rate = fee_rate;
    }

    /// Calculate fee for a fill
    /// For spot: BUY = fee in base asset, SELL = fee in quote asset
    fn calculate_fee(
        &self,
        instrument: &InstrumentId,
        side: OrderSide,
        qty: Qty,
        price: Price,
    ) -> Fee {
        Self::calculate_fee_static(self.fee_rate, instrument, side, qty, price)
    }

    /// Static version of calculate_fee (for use when self is already borrowed)
    fn calculate_fee_static(
        fee_rate: Decimal,
        instrument: &InstrumentId,
        side: OrderSide,
        qty: Qty,
        price: Price,
    ) -> Fee {
        if fee_rate == Decimal::ZERO {
            return Fee::new(Decimal::ZERO, AssetId::new("USDC"));
        }

        let instrument_str = instrument.to_string();
        let is_spot = instrument_str.ends_with("-SPOT");

        if is_spot {
            let base_asset = if let Some(pos) = instrument_str.rfind('-') {
                AssetId::new(&instrument_str[..pos])
            } else {
                AssetId::new(&instrument_str)
            };

            match side {
                OrderSide::Buy => {
                    // Spot BUY: fee deducted from received base asset
                    let fee_amount = qty.0 * fee_rate;
                    Fee::new(fee_amount, base_asset)
                }
                OrderSide::Sell => {
                    // Spot SELL: fee deducted from received quote (USDC)
                    let notional = qty.0 * price.0;
                    let fee_amount = notional * fee_rate;
                    Fee::new(fee_amount, AssetId::new("USDC"))
                }
            }
        } else {
            // PERP: fee always in USDC
            let notional = qty.0 * price.0;
            let fee_amount = notional * fee_rate;
            Fee::new(fee_amount, AssetId::new("USDC"))
        }
    }

    /// Generate next exchange order ID
    pub fn next_exchange_order_id(&mut self, prefix: &str) -> ExchangeOrderId {
        let oid = self.next_oid;
        self.next_oid += 1;
        ExchangeOrderId::new(format!("{}_{}", prefix, oid))
    }

    /// Add a pending order (maintains sorted order for O(log n) lookups)
    pub fn add_pending_order(&mut self, order: PendingOrder) {
        let key = OrderGroupKey {
            instrument: order.instrument.clone(),
            side: order.side,
        };

        let orders = self.order_groups.entry(key).or_default();

        // Binary search insert to maintain sorted order
        // BUY orders: sorted descending by price (highest = most aggressive first)
        // SELL orders: sorted ascending by price (lowest = most aggressive first)
        // VecDeque: convert to slice for partition_point, then insert
        let insert_pos = {
            let slice = orders.make_contiguous();
            match order.side {
                OrderSide::Buy => {
                    // Descending: find first position where existing price < new price
                    slice.partition_point(|o| o.price.0 > order.price.0)
                }
                OrderSide::Sell => {
                    // Ascending: find first position where existing price > new price
                    slice.partition_point(|o| o.price.0 < order.price.0)
                }
            }
        };

        orders.insert(insert_pos, order);
    }

    /// Remove pending order by client ID
    pub fn remove_order(&mut self, client_id: &ClientOrderId) -> Option<PendingOrder> {
        for orders in self.order_groups.values_mut() {
            if let Some(pos) = orders.iter().position(|o| &o.client_id == client_id) {
                return orders.remove(pos); // VecDeque::remove returns Option<T>
            }
        }
        None
    }

    /// Remove all pending orders for an instrument
    pub fn remove_orders_for_instrument(&mut self, instrument: &InstrumentId) -> Vec<PendingOrder> {
        let mut removed = Vec::new();

        // Remove from both Buy and Sell groups
        for side in [OrderSide::Buy, OrderSide::Sell] {
            let key = OrderGroupKey {
                instrument: instrument.clone(),
                side,
            };
            if let Some(orders) = self.order_groups.remove(&key) {
                removed.extend(orders);
            }
        }

        removed
    }

    /// Get current balance
    pub fn balance(&self, asset: &AssetId) -> Decimal {
        self.balances.get(asset).copied().unwrap_or_default()
    }

    /// Set balance directly
    pub fn set_balance(&mut self, asset: AssetId, amount: Decimal) {
        self.balances.insert(asset, amount);
    }

    /// Check pending orders against quotes and generate fills.
    ///
    /// ## Optimization
    ///
    /// Orders are grouped by (instrument, side) and sorted by price.
    /// For each quote, we use binary search to find orders that could fill:
    /// - BUY orders fill when ask <= order.price (scan from highest price down)
    /// - SELL orders fill when bid >= order.price (scan from lowest price up)
    ///
    /// This reduces complexity from O(orders) to O(fillable_orders).
    pub fn check_fills(
        &mut self,
        quotes: &HashMap<InstrumentId, Quote>,
        time_ms: i64,
    ) -> Vec<SimulatedFill> {
        let mut fills = Vec::new();

        // Extract fee_rate before mutably borrowing order_groups
        let fee_rate = self.fee_rate;

        // Process each quote
        for (instrument, quote) in quotes {
            // Check BUY orders: fill when ask <= order.price
            // Orders are sorted descending by price (highest first)
            let buy_key = OrderGroupKey {
                instrument: instrument.clone(),
                side: OrderSide::Buy,
            };

            if let Some(orders) = self.order_groups.get_mut(&buy_key) {
                // Find the partition point: orders with price >= ask can fill
                // Since sorted descending, all orders from index 0 to partition_point can fill
                let ask = quote.ask.0;

                // Drain fillable orders (from front since they're highest price first)
                // Using front() for comparison and pop_front() for O(1) removal
                while orders.front().map(|o| o.price.0 >= ask).unwrap_or(false) {
                    let order = orders.pop_front().unwrap();
                    let fee = Self::calculate_fee_static(
                        fee_rate,
                        &order.instrument,
                        order.side,
                        order.remaining_qty.clone(),
                        quote.ask.clone(),
                    );

                    let fill = Fill {
                        trade_id: TradeId::new(format!("sim_{}", order.exchange_order_id.0)),
                        client_id: Some(order.client_id.clone()),
                        exchange_order_id: Some(order.exchange_order_id.clone()),
                        instrument: order.instrument.clone(),
                        side: order.side,
                        price: order.price.clone(),
                        qty: order.remaining_qty.clone(),
                        fee,
                        ts: time_ms,
                    };

                    fills.push(SimulatedFill {
                        fill,
                        order_fully_filled: true,
                    });
                }
            }

            // Check SELL orders: fill when bid >= order.price
            // Orders are sorted ascending by price (lowest first)
            let sell_key = OrderGroupKey {
                instrument: instrument.clone(),
                side: OrderSide::Sell,
            };

            if let Some(orders) = self.order_groups.get_mut(&sell_key) {
                let bid = quote.bid.0;

                // Drain fillable orders (from front since they're lowest price first)
                // Using front() for comparison and pop_front() for O(1) removal
                while orders.front().map(|o| o.price.0 <= bid).unwrap_or(false) {
                    let order = orders.pop_front().unwrap();
                    let fee = Self::calculate_fee_static(
                        fee_rate,
                        &order.instrument,
                        order.side,
                        order.remaining_qty.clone(),
                        quote.bid.clone(),
                    );

                    let fill = Fill {
                        trade_id: TradeId::new(format!("sim_{}", order.exchange_order_id.0)),
                        client_id: Some(order.client_id.clone()),
                        exchange_order_id: Some(order.exchange_order_id.clone()),
                        instrument: order.instrument.clone(),
                        side: order.side,
                        price: order.price.clone(),
                        qty: order.remaining_qty.clone(),
                        fee,
                        ts: time_ms,
                    };

                    fills.push(SimulatedFill {
                        fill,
                        order_fully_filled: true,
                    });
                }
            }
        }

        // Apply balance updates
        for sim_fill in &fills {
            self.apply_fill_to_balances(&sim_fill.fill);
        }

        fills
    }

    /// Apply a fill to balances (buy: deduct quote, add base; sell: vice versa)
    /// Also deducts fees from the appropriate asset
    /// NOTE: For PERP instruments, we skip balance updates - Engine's position tracker is source of truth
    fn apply_fill_to_balances(&mut self, fill: &Fill) {
        let instrument_str = fill.instrument.to_string();

        // For PERP instruments, don't modify balances
        // The Engine's PositionTracker handles PnL tracking
        // This avoids phantom base asset balances (e.g., fake BTC holdings)
        if instrument_str.ends_with("-PERP") {
            return;
        }

        // SPOT logic below - unchanged
        let quote_asset = AssetId::new("USDC");

        // Extract base asset from instrument (e.g., "ETH-USD" -> "ETH")
        let base_asset = if let Some(pos) = instrument_str.rfind('-') {
            AssetId::new(&instrument_str[..pos])
        } else {
            AssetId::new(&instrument_str)
        };

        let notional = fill.price.0 * fill.qty.0;

        match fill.side {
            OrderSide::Buy => {
                *self.balances.entry(quote_asset.clone()).or_default() -= notional;
                // Add base asset, but deduct fee if fee is in base asset
                let received = if fill.fee.asset == base_asset {
                    fill.qty.0 - fill.fee.amount // Fee deducted from received
                } else {
                    fill.qty.0
                };
                *self.balances.entry(base_asset).or_default() += received;
                // If fee is in quote asset, deduct from quote
                if fill.fee.asset == quote_asset {
                    *self.balances.entry(quote_asset).or_default() -= fill.fee.amount;
                }
            }
            OrderSide::Sell => {
                // Add notional to quote, but deduct fee if fee is in quote
                let received = if fill.fee.asset == quote_asset {
                    notional - fill.fee.amount
                } else {
                    notional
                };
                *self.balances.entry(quote_asset).or_default() += received;
                *self.balances.entry(base_asset.clone()).or_default() -= fill.qty.0;
                // If fee is in base asset (unusual for sell), deduct from base
                if fill.fee.asset == base_asset {
                    *self.balances.entry(base_asset).or_default() -= fill.fee.amount;
                }
            }
        }
    }

    /// Check if balance is sufficient for an order
    pub fn check_balance(
        &self,
        instrument: &InstrumentId,
        side: OrderSide,
        price: Decimal,
        qty: Decimal,
    ) -> Result<(), String> {
        let quote_asset = AssetId::new("USDC");

        if side == OrderSide::Buy {
            let required = price * qty;
            let available = self.balance(&quote_asset);
            if required > available {
                return Err(format!(
                    "Insufficient balance: need {} USDC, have {}",
                    required, available
                ));
            }
        } else {
            let instrument_str = instrument.to_string();
            let base_asset = if let Some(pos) = instrument_str.rfind('-') {
                AssetId::new(&instrument_str[..pos])
            } else {
                AssetId::new(&instrument_str)
            };
            let available = self.balance(&base_asset);
            if qty > available {
                return Err(format!(
                    "Insufficient balance: need {} {}, have {}",
                    qty, base_asset, available
                ));
            }
        }
        Ok(())
    }

    /// Get pending orders count (sum across all groups)
    pub fn pending_orders_count(&self) -> usize {
        self.order_groups.values().map(|v| v.len()).sum()
    }

    /// Get all pending orders (for inspection) - flattened from all groups
    pub fn pending_orders(&self) -> Vec<&PendingOrder> {
        self.order_groups.values().flat_map(|v| v.iter()).collect()
    }
}

impl Default for FillSimulator {
    fn default() -> Self {
        Self::new(HashMap::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_quote(instrument: &str, bid: i64, ask: i64) -> Quote {
        Quote {
            instrument: InstrumentId::new(instrument),
            bid: Price::new(Decimal::new(bid, 0)),
            ask: Price::new(Decimal::new(ask, 0)),
            bid_size: Qty::new(Decimal::new(10, 0)),
            ask_size: Qty::new(Decimal::new(10, 0)),
            ts: 0,
        }
    }

    #[test]
    fn test_buy_order_fills_when_ask_crosses() {
        let mut balances = HashMap::new();
        balances.insert(AssetId::new("USDC"), Decimal::new(100000, 0));

        let mut sim = FillSimulator::new(balances);

        // Place buy order at 50000
        sim.add_pending_order(PendingOrder {
            client_id: ClientOrderId::new("order1"),
            exchange_order_id: ExchangeOrderId::new("ex1"),
            instrument: InstrumentId::new("BTC-PERP"),
            side: OrderSide::Buy,
            price: Price::new(Decimal::new(50000, 0)),
            qty: Qty::new(Decimal::new(1, 0)),
            remaining_qty: Qty::new(Decimal::new(1, 0)),
            created_at: 0,
        });

        // Quote with ask at 49999 (below order price) - should fill
        let mut quotes = HashMap::new();
        quotes.insert(
            InstrumentId::new("BTC-PERP"),
            make_quote("BTC-PERP", 49998, 49999),
        );

        let fills = sim.check_fills(&quotes, 1000);
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].fill.price.0, Decimal::new(49999, 0));
        assert_eq!(sim.pending_orders_count(), 0);
    }

    #[test]
    fn test_sell_order_fills_when_bid_crosses() {
        let mut balances = HashMap::new();
        balances.insert(AssetId::new("BTC"), Decimal::new(10, 0));

        let mut sim = FillSimulator::new(balances);

        // Place sell order at 50000
        sim.add_pending_order(PendingOrder {
            client_id: ClientOrderId::new("order1"),
            exchange_order_id: ExchangeOrderId::new("ex1"),
            instrument: InstrumentId::new("BTC-PERP"),
            side: OrderSide::Sell,
            price: Price::new(Decimal::new(50000, 0)),
            qty: Qty::new(Decimal::new(1, 0)),
            remaining_qty: Qty::new(Decimal::new(1, 0)),
            created_at: 0,
        });

        // Quote with bid at 50001 (above order price) - should fill
        let mut quotes = HashMap::new();
        quotes.insert(
            InstrumentId::new("BTC-PERP"),
            make_quote("BTC-PERP", 50001, 50002),
        );

        let fills = sim.check_fills(&quotes, 1000);
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].fill.price.0, Decimal::new(50001, 0));
    }
}
