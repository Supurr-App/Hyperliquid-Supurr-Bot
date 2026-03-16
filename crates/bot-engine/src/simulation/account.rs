//! Isolated margin accounting for simulated perpetual futures.
//!
//! Models per-instrument isolated margin positions:
//! - Tracks qty, avg entry price, leverage, reserved margin
//! - Computes unrealized PnL at any mark price
//! - Computes liquidation price using the Hyperliquid formula
//! - Determines required initial margin for new orders
//!
//! The `MarginLedger` manages all positions + free USDC, providing
//! a single source of truth for the simulated account.

use bot_core::{InstrumentId, OrderSide, PositionSnapshot, Price};
use rust_decimal::Decimal;
use std::collections::HashMap;

// =============================================================================
// IsolatedPosition — Per-instrument margin state
// =============================================================================

/// Per-instrument isolated margin position.
///
/// Tracks all state needed for accurate perp simulation:
/// position size, average entry, reserved margin, PnL, and fees.
#[derive(Debug, Clone)]
pub struct IsolatedPosition {
    /// Signed position quantity: positive = long, negative = short
    pub qty: Decimal,
    /// Weighted average entry price
    pub avg_entry_px: Decimal,
    /// Leverage for this position (e.g., 10x)
    pub leverage: Decimal,
    /// USDC locked as isolated margin for this position
    pub isolated_margin_reserved: Decimal,
    /// Maintenance margin rate = 1 / max_leverage
    /// Used for liquidation price calculation
    pub maintenance_margin_rate: Decimal,
    /// Accumulated realized PnL from partial/full closes
    pub realized_pnl: Decimal,
    /// Accumulated fees paid
    pub fee_paid: Decimal,
}

impl Default for IsolatedPosition {
    fn default() -> Self {
        Self {
            qty: Decimal::ZERO,
            avg_entry_px: Decimal::ZERO,
            leverage: Decimal::ONE,
            isolated_margin_reserved: Decimal::ZERO,
            maintenance_margin_rate: Decimal::ONE, // 1x = 100% margin rate
            realized_pnl: Decimal::ZERO,
            fee_paid: Decimal::ZERO,
        }
    }
}

impl IsolatedPosition {
    /// Unrealized PnL at given mark price.
    /// Long: profit when price goes up. Short: profit when price goes down.
    pub fn unrealized_pnl(&self, mark_price: Decimal) -> Decimal {
        if self.qty.is_zero() {
            return Decimal::ZERO;
        }
        self.qty * (mark_price - self.avg_entry_px)
    }

    /// Liquidation price for this isolated position (Hyperliquid formula).
    ///
    /// For a long position: liq_price = entry - margin_available / |qty| / (1 - mmr)
    /// For a short position: liq_price = entry + margin_available / |qty| / (1 + mmr)
    ///
    /// Returns None if position is flat.
    pub fn liquidation_price(&self) -> Option<Decimal> {
        if self.qty.is_zero() {
            return None;
        }

        let side_val = if self.qty > Decimal::ZERO {
            Decimal::ONE
        } else {
            -Decimal::ONE
        };
        let mmr = self.maintenance_margin_rate;
        let denominator = Decimal::ONE - mmr * side_val;

        if denominator.is_zero() {
            return None;
        }

        let margin_available = self.isolated_margin_reserved
            - self.qty.abs() * self.avg_entry_px * mmr;

        Some(
            self.avg_entry_px - side_val * margin_available / self.qty.abs() / denominator,
        )
    }

    /// Required initial margin for a new order at given notional and leverage.
    /// Includes a fee buffer for entry + exit (2x fee_rate).
    pub fn required_margin_for_order(
        notional: Decimal,
        leverage: Decimal,
        fee_rate: Decimal,
    ) -> Decimal {
        let margin = notional / leverage;
        let fee_buffer = notional * fee_rate * Decimal::TWO; // entry + exit fees
        margin + fee_buffer
    }

    /// Check if this position is liquidated at the given mark price.
    ///
    /// A long is liquidated when mark drops below liq_price.
    /// A short is liquidated when mark rises above liq_price.
    pub fn is_liquidated(&self, mark_price: Decimal) -> bool {
        if self.qty.is_zero() {
            return false;
        }

        if let Some(liq_price) = self.liquidation_price() {
            if self.qty > Decimal::ZERO {
                // Long: liquidated when mark <= liq_price
                mark_price <= liq_price
            } else {
                // Short: liquidated when mark >= liq_price
                mark_price >= liq_price
            }
        } else {
            false
        }
    }

    /// Check if an order on this instrument is reducing the current position.
    pub fn is_order_reducing(&self, order_side: OrderSide) -> bool {
        if self.qty.is_zero() {
            return false;
        }
        match order_side {
            OrderSide::Buy => self.qty < Decimal::ZERO,  // buying reduces a short
            OrderSide::Sell => self.qty > Decimal::ZERO,  // selling reduces a long
        }
    }
}

// =============================================================================
// MarginLedger — All positions + free USDC
// =============================================================================

/// Manages all isolated margin positions and free USDC balance.
///
/// This is the single source of truth for simulated account state,
/// replacing the scattered tracking in PaperExchange and FillSimulator.
#[derive(Debug)]
pub struct MarginLedger {
    /// Free USDC not locked in any margin position
    free_usdc: Decimal,
    /// Per-instrument isolated positions
    positions: HashMap<InstrumentId, IsolatedPosition>,
    /// Per-instrument leverage settings: (user_leverage, max_leverage)
    leverage_settings: HashMap<InstrumentId, (Decimal, Decimal)>,
    /// Default leverage for instruments without explicit settings
    default_leverage: Decimal,
    /// Fee rate (e.g. 0.0002 = 0.02%)
    fee_rate: Decimal,
}

impl MarginLedger {
    /// Create a new ledger with starting USDC balance.
    pub fn new(starting_balance: Decimal, fee_rate: Decimal) -> Self {
        Self {
            free_usdc: starting_balance,
            positions: HashMap::new(),
            leverage_settings: HashMap::new(),
            default_leverage: Decimal::ONE, // 1x = no leverage by default
            fee_rate,
        }
    }

    // =========================================================================
    // Read accessors
    // =========================================================================

    /// Free USDC available for new margin reservations.
    pub fn free_usdc(&self) -> Decimal {
        self.free_usdc
    }

    /// Set free USDC directly (for compatibility with FillSimulator spot logic).
    pub fn set_free_usdc(&mut self, amount: Decimal) {
        self.free_usdc = amount;
    }

    /// Adjust free_usdc by delta (positive = add, negative = subtract).
    pub fn adjust_free_usdc(&mut self, delta: Decimal) {
        self.free_usdc += delta;
    }

    /// Fee rate used for margin calculations.
    pub fn fee_rate(&self) -> Decimal {
        self.fee_rate
    }

    /// Set fee rate.
    pub fn set_fee_rate(&mut self, fee_rate: Decimal) {
        self.fee_rate = fee_rate;
    }

    /// Get position for an instrument (if any).
    pub fn position(&self, instrument: &InstrumentId) -> Option<&IsolatedPosition> {
        self.positions.get(instrument)
    }

    /// Get mutable position for an instrument.
    pub fn position_mut(&mut self, instrument: &InstrumentId) -> Option<&mut IsolatedPosition> {
        self.positions.get_mut(instrument)
    }

    /// Get or create a position for an instrument.
    fn position_or_default(&mut self, instrument: &InstrumentId) -> &mut IsolatedPosition {
        self.positions.entry(instrument.clone()).or_default()
    }

    /// Get leverage for an instrument.
    pub fn leverage_for(&self, instrument: &InstrumentId) -> Decimal {
        self.leverage_settings
            .get(instrument)
            .map(|(lev, _)| *lev)
            .unwrap_or(self.default_leverage)
    }

    /// Total reserved margin across all positions.
    pub fn total_reserved_margin(&self) -> Decimal {
        self.positions
            .values()
            .map(|p| p.isolated_margin_reserved)
            .sum()
    }

    /// Total unrealized PnL across all positions at given mark prices.
    pub fn total_unrealized_pnl(&self, marks: &HashMap<InstrumentId, Decimal>) -> Decimal {
        self.positions
            .iter()
            .map(|(inst, pos)| {
                marks
                    .get(inst)
                    .map(|mark| pos.unrealized_pnl(*mark))
                    .unwrap_or(Decimal::ZERO)
            })
            .sum()
    }

    /// Account equity = free_usdc + total_reserved_margin + total_unrealized_pnl
    pub fn equity(&self, marks: &HashMap<InstrumentId, Decimal>) -> Decimal {
        self.free_usdc + self.total_reserved_margin() + self.total_unrealized_pnl(marks)
    }

    /// Position quantity for an instrument (signed: +long, -short).
    pub fn position_qty(&self, instrument: &InstrumentId) -> Decimal {
        self.positions
            .get(instrument)
            .map(|p| p.qty)
            .unwrap_or(Decimal::ZERO)
    }

    // =========================================================================
    // Configuration
    // =========================================================================

    /// Set leverage for an instrument.
    /// `leverage` is the user's chosen leverage (e.g. 10x).
    /// `max_leverage` is the exchange maximum for this asset (determines MMR).
    pub fn set_leverage(
        &mut self,
        instrument: &InstrumentId,
        leverage: Decimal,
        max_leverage: Decimal,
    ) {
        self.leverage_settings
            .insert(instrument.clone(), (leverage, max_leverage));

        // Update existing position's maintenance margin rate if it exists
        if let Some(pos) = self.positions.get_mut(instrument) {
            pos.leverage = leverage;
            if max_leverage > Decimal::ZERO {
                pos.maintenance_margin_rate = Decimal::ONE / max_leverage;
            }
        }
    }

    /// Set default leverage for instruments without explicit settings.
    pub fn set_default_leverage(&mut self, leverage: Decimal) {
        self.default_leverage = leverage;
    }

    // =========================================================================
    // Order admission (A1)
    // =========================================================================

    /// Check if there's sufficient margin for a PERP order.
    ///
    /// - Reducing orders always pass (they release margin).
    /// - New/increasing orders need: notional / leverage + 2x fee buffer.
    /// - Spot orders are not handled here (use FillSimulator's check_balance).
    pub fn check_margin_for_perp_order(
        &self,
        instrument: &InstrumentId,
        side: OrderSide,
        price: Decimal,
        qty: Decimal,
        reduce_only: bool,
    ) -> Result<(), String> {
        let existing_pos = self.positions.get(instrument);

        // Reduce-only orders don't need margin
        if reduce_only {
            return Ok(());
        }

        // Is this reducing an existing position?
        let is_reducing = existing_pos.map_or(false, |pos| pos.is_order_reducing(side));
        if is_reducing {
            return Ok(());
        }

        // New or increasing position: compute required margin
        let notional = price * qty;
        let leverage = self.leverage_for(instrument);
        let required =
            IsolatedPosition::required_margin_for_order(notional, leverage, self.fee_rate);

        if required > self.free_usdc {
            return Err(format!(
                "Insufficient margin: need {} USDC (notional={}, leverage={}x, fee_rate={}), have {} free",
                required, notional, leverage, self.fee_rate, self.free_usdc
            ));
        }

        Ok(())
    }

    // =========================================================================
    // Fill settlement (A2)
    // =========================================================================

    /// Apply a PERP fill to the margin ledger.
    ///
    /// Handles all scenarios:
    /// - Opening new position: reserve margin, set entry price
    /// - Increasing existing position: reserve additional margin, update avg entry
    /// - Reducing existing position: release margin proportionally, realize PnL
    /// - Flipping position: close old + open new in opposite direction
    ///
    /// Returns the realized PnL from this fill (zero for new positions).
    pub fn apply_perp_fill(
        &mut self,
        instrument: &InstrumentId,
        side: OrderSide,
        fill_price: Decimal,
        fill_qty: Decimal,
        fee_amount: Decimal,
    ) -> Decimal {
        let leverage = self.leverage_for(instrument);
        let max_leverage = self
            .leverage_settings
            .get(instrument)
            .map(|(_, ml)| *ml)
            .unwrap_or(leverage);
        let mmr = if max_leverage > Decimal::ZERO {
            Decimal::ONE / max_leverage
        } else {
            Decimal::ONE
        };

        // Determine signed fill quantity
        let signed_fill_qty = match side {
            OrderSide::Buy => fill_qty,
            OrderSide::Sell => -fill_qty,
        };

        let pos = self.position_or_default(instrument);
        pos.leverage = leverage;
        pos.maintenance_margin_rate = mmr;

        // Always deduct fee from free USDC
        self.free_usdc -= fee_amount;
        if let Some(pos) = self.positions.get_mut(instrument) {
            pos.fee_paid += fee_amount;
        }

        let old_qty = self
            .positions
            .get(instrument)
            .map(|p| p.qty)
            .unwrap_or(Decimal::ZERO);

        let is_same_direction = (old_qty >= Decimal::ZERO && signed_fill_qty > Decimal::ZERO)
            || (old_qty <= Decimal::ZERO && signed_fill_qty < Decimal::ZERO);

        let is_flat = old_qty.is_zero();

        if is_flat || is_same_direction {
            // Opening or increasing position
            self.apply_increase(instrument, signed_fill_qty, fill_price, leverage)
        } else {
            // Reducing or flipping
            let closing_qty = signed_fill_qty.abs().min(old_qty.abs());
            let remaining_qty = signed_fill_qty.abs() - closing_qty;

            // 1. Close the portion
            let realized = self.apply_reduce(instrument, closing_qty, fill_price);

            // 2. If there's remaining qty, open in opposite direction
            if remaining_qty > Decimal::ZERO {
                let new_signed = if signed_fill_qty > Decimal::ZERO {
                    remaining_qty
                } else {
                    -remaining_qty
                };
                self.apply_increase(instrument, new_signed, fill_price, leverage);
            }

            realized
        }
    }

    /// Increase position (same direction or new).
    /// Reserves additional margin, updates weighted avg entry price.
    fn apply_increase(
        &mut self,
        instrument: &InstrumentId,
        signed_fill_qty: Decimal,
        fill_price: Decimal,
        leverage: Decimal,
    ) -> Decimal {
        let new_margin = (fill_price * signed_fill_qty.abs()) / leverage;

        // Reserve margin from free USDC
        self.free_usdc -= new_margin;

        let pos = self.position_or_default(instrument);

        // Update weighted average entry price
        let old_notional = pos.qty.abs() * pos.avg_entry_px;
        let new_notional = signed_fill_qty.abs() * fill_price;
        let total_qty = pos.qty.abs() + signed_fill_qty.abs();

        if total_qty > Decimal::ZERO {
            pos.avg_entry_px = (old_notional + new_notional) / total_qty;
        }

        pos.qty += signed_fill_qty;
        pos.isolated_margin_reserved += new_margin;

        Decimal::ZERO // No realized PnL on increase
    }

    /// Reduce position (opposite direction).
    /// Releases margin proportionally, realizes PnL.
    fn apply_reduce(
        &mut self,
        instrument: &InstrumentId,
        closing_qty: Decimal,
        fill_price: Decimal,
    ) -> Decimal {
        let pos = match self.positions.get_mut(instrument) {
            Some(p) => p,
            None => return Decimal::ZERO,
        };

        if pos.qty.is_zero() {
            return Decimal::ZERO;
        }

        let old_abs_qty = pos.qty.abs();
        let close_fraction = closing_qty / old_abs_qty;

        // Compute realized PnL for the closed portion
        let realized = if pos.qty > Decimal::ZERO {
            // Long: profit = (fill_price - entry) * closing_qty
            (fill_price - pos.avg_entry_px) * closing_qty
        } else {
            // Short: profit = (entry - fill_price) * closing_qty
            (pos.avg_entry_px - fill_price) * closing_qty
        };

        // Release proportional margin
        let released_margin = pos.isolated_margin_reserved * close_fraction;
        pos.isolated_margin_reserved -= released_margin;

        // Update position qty (reduce toward zero)
        if pos.qty > Decimal::ZERO {
            pos.qty -= closing_qty;
        } else {
            pos.qty += closing_qty;
        }

        // Return released margin + realized PnL to free USDC
        self.free_usdc += released_margin + realized;

        // Track realized PnL
        pos.realized_pnl += realized;

        // Clean up flat positions
        if pos.qty.is_zero() {
            pos.avg_entry_px = Decimal::ZERO;
            pos.isolated_margin_reserved = Decimal::ZERO;
        }

        realized
    }

    // =========================================================================
    // Liquidation checks (A4)
    // =========================================================================

    /// Check all positions for liquidation at current mark prices.
    /// Returns list of instruments that should be liquidated.
    pub fn check_liquidations(
        &self,
        marks: &HashMap<InstrumentId, Decimal>,
    ) -> Vec<InstrumentId> {
        let mut liquidated = Vec::new();

        for (instrument, pos) in &self.positions {
            if pos.qty.is_zero() {
                continue;
            }
            if let Some(mark) = marks.get(instrument) {
                if pos.is_liquidated(*mark) {
                    liquidated.push(instrument.clone());
                }
            }
        }

        liquidated
    }

    /// Force-liquidate a position at the given mark price.
    /// Closes the entire position, realizes the loss, releases remaining margin.
    pub fn liquidate(&mut self, instrument: &InstrumentId, mark_price: Decimal) {
        let pos = match self.positions.get(instrument) {
            Some(p) if !p.qty.is_zero() => p,
            _ => return,
        };

        let qty = pos.qty;
        let closing_qty = qty.abs();

        // Determine the side of the closing fill
        let close_side = if qty > Decimal::ZERO {
            OrderSide::Sell // close long
        } else {
            OrderSide::Buy // close short
        };

        tracing::warn!(
            instrument = %instrument,
            qty = %qty,
            entry = %pos.avg_entry_px,
            mark = %mark_price,
            "LIQUIDATION: force-closing position"
        );

        self.apply_perp_fill(instrument, close_side, mark_price, closing_qty, Decimal::ZERO);
    }

    // =========================================================================
    // Account state snapshots (A3)
    // =========================================================================

    /// Generate position snapshots for `poll_account_state()`.
    /// Returns non-flat positions in the format expected by `AccountState`.
    pub fn position_snapshots(
        &self,
        marks: &HashMap<InstrumentId, Decimal>,
    ) -> Vec<PositionSnapshot> {
        self.positions
            .iter()
            .filter(|(_, pos)| !pos.qty.is_zero())
            .map(|(instrument, pos)| {
                let unrealized = marks
                    .get(instrument)
                    .map(|mark| pos.unrealized_pnl(*mark));

                PositionSnapshot {
                    instrument: instrument.clone(),
                    qty: pos.qty,
                    avg_entry_px: if pos.avg_entry_px.is_zero() {
                        None
                    } else {
                        Some(Price::new(pos.avg_entry_px))
                    },
                    unrealized_pnl: unrealized,
                    liquidation_px: pos.liquidation_price(),
                }
            })
            .collect()
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn usdc(n: i64) -> Decimal {
        Decimal::new(n, 0)
    }

    fn btc_perp() -> InstrumentId {
        InstrumentId::new("BTC-PERP")
    }

    fn eth_perp() -> InstrumentId {
        InstrumentId::new("ETH-PERP")
    }

    // =========================================================================
    // IsolatedPosition tests
    // =========================================================================

    #[test]
    fn test_unrealized_pnl_long() {
        let pos = IsolatedPosition {
            qty: Decimal::ONE,
            avg_entry_px: usdc(50000),
            ..Default::default()
        };
        // Price went up: $1000 profit
        assert_eq!(pos.unrealized_pnl(usdc(51000)), usdc(1000));
        // Price went down: $1000 loss
        assert_eq!(pos.unrealized_pnl(usdc(49000)), -usdc(1000));
    }

    #[test]
    fn test_unrealized_pnl_short() {
        let pos = IsolatedPosition {
            qty: -Decimal::ONE,
            avg_entry_px: usdc(50000),
            ..Default::default()
        };
        // Price went down: $1000 profit (short wins)
        assert_eq!(pos.unrealized_pnl(usdc(49000)), usdc(1000));
        // Price went up: $1000 loss
        assert_eq!(pos.unrealized_pnl(usdc(51000)), -usdc(1000));
    }

    #[test]
    fn test_unrealized_pnl_flat() {
        let pos = IsolatedPosition::default();
        assert_eq!(pos.unrealized_pnl(usdc(99999)), Decimal::ZERO);
    }

    #[test]
    fn test_required_margin() {
        let margin = IsolatedPosition::required_margin_for_order(
            usdc(10000),          // $10k notional
            Decimal::new(10, 0),  // 10x leverage
            Decimal::new(2, 4),   // 0.02% fee
        );
        // margin = 10000/10 = 1000, fee_buffer = 10000 * 0.0002 * 2 = 4
        assert_eq!(margin, Decimal::new(1004, 0));
    }

    #[test]
    fn test_is_order_reducing() {
        let long_pos = IsolatedPosition {
            qty: Decimal::ONE,
            ..Default::default()
        };
        assert!(long_pos.is_order_reducing(OrderSide::Sell));
        assert!(!long_pos.is_order_reducing(OrderSide::Buy));

        let short_pos = IsolatedPosition {
            qty: -Decimal::ONE,
            ..Default::default()
        };
        assert!(short_pos.is_order_reducing(OrderSide::Buy));
        assert!(!short_pos.is_order_reducing(OrderSide::Sell));
    }

    // =========================================================================
    // MarginLedger tests
    // =========================================================================

    #[test]
    fn test_margin_admission_rejects_insufficient() {
        let ledger = MarginLedger::new(usdc(1000), Decimal::new(2, 4));
        // Set leverage to 10x
        // With $1000 at 10x, max notional should be around $10k

        let result = ledger.check_margin_for_perp_order(
            &btc_perp(),
            OrderSide::Buy,
            usdc(50000), // price
            Decimal::ONE, // 1 BTC = $50k notional
            false,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Insufficient margin"));
    }

    #[test]
    fn test_margin_admission_accepts_leveraged() {
        let mut ledger = MarginLedger::new(usdc(1000), Decimal::new(2, 4));
        ledger.set_leverage(&btc_perp(), Decimal::new(10, 0), Decimal::new(50, 0));

        // $1000 at 10x → can trade up to ~$10k notional
        // 0.1 BTC at $50000 = $5000 notional → margin needed = 500 + fees
        let result = ledger.check_margin_for_perp_order(
            &btc_perp(),
            OrderSide::Buy,
            usdc(50000),
            Decimal::new(1, 1), // 0.1 BTC
            false,
        );
        assert!(result.is_ok(), "Should accept: {:?}", result);
    }

    #[test]
    fn test_margin_admission_allows_reducing() {
        let mut ledger = MarginLedger::new(usdc(1000), Decimal::new(2, 4));
        ledger.set_leverage(&btc_perp(), Decimal::new(10, 0), Decimal::new(50, 0));

        // Open a long position first
        ledger.apply_perp_fill(
            &btc_perp(),
            OrderSide::Buy,
            usdc(50000),
            Decimal::new(1, 1),
            Decimal::ZERO,
        );

        // Now selling (reducing) should always pass, even with zero free USDC
        let result = ledger.check_margin_for_perp_order(
            &btc_perp(),
            OrderSide::Sell,
            usdc(50000),
            Decimal::new(1, 1),
            false,
        );
        assert!(result.is_ok(), "Reducing order should always pass");
    }

    #[test]
    fn test_fill_open_long() {
        let mut ledger = MarginLedger::new(usdc(10000), Decimal::new(2, 4));
        ledger.set_leverage(&btc_perp(), Decimal::new(10, 0), Decimal::new(50, 0));

        let realized = ledger.apply_perp_fill(
            &btc_perp(),
            OrderSide::Buy,
            usdc(50000),
            Decimal::new(1, 1), // 0.1 BTC
            usdc(1),            // $1 fee
        );

        assert_eq!(realized, Decimal::ZERO, "No realized PnL on open");

        let pos = ledger.position(&btc_perp()).unwrap();
        assert_eq!(pos.qty, Decimal::new(1, 1)); // 0.1 BTC long
        assert_eq!(pos.avg_entry_px, usdc(50000));
        // margin = 5000/10 = 500
        assert_eq!(pos.isolated_margin_reserved, usdc(500));
        // free = 10000 - 500 - 1(fee) = 9499
        assert_eq!(ledger.free_usdc(), usdc(9499));
    }

    #[test]
    fn test_fill_close_long_with_profit() {
        let mut ledger = MarginLedger::new(usdc(10000), Decimal::new(2, 4));
        ledger.set_leverage(&btc_perp(), Decimal::new(10, 0), Decimal::new(50, 0));

        // Open: buy 0.1 BTC at $50000
        ledger.apply_perp_fill(
            &btc_perp(),
            OrderSide::Buy,
            usdc(50000),
            Decimal::new(1, 1),
            Decimal::ZERO,
        );

        let free_after_open = ledger.free_usdc();

        // Close: sell 0.1 BTC at $51000 (profit = 0.1 * 1000 = $100)
        let realized = ledger.apply_perp_fill(
            &btc_perp(),
            OrderSide::Sell,
            usdc(51000),
            Decimal::new(1, 1),
            Decimal::ZERO,
        );

        assert_eq!(realized, usdc(100), "Realized PnL should be $100");

        let pos = ledger.position(&btc_perp()).unwrap();
        assert!(pos.qty.is_zero(), "Position should be flat");
        assert_eq!(pos.isolated_margin_reserved, Decimal::ZERO);

        // free = free_after_open + 500(margin_released) + 100(pnl)
        assert_eq!(ledger.free_usdc(), free_after_open + usdc(500) + usdc(100));
    }

    #[test]
    fn test_fill_close_long_with_loss() {
        let mut ledger = MarginLedger::new(usdc(10000), Decimal::ZERO);
        ledger.set_leverage(&btc_perp(), Decimal::new(10, 0), Decimal::new(50, 0));

        // Open: buy 0.1 BTC at $50000
        ledger.apply_perp_fill(
            &btc_perp(),
            OrderSide::Buy,
            usdc(50000),
            Decimal::new(1, 1),
            Decimal::ZERO,
        );

        // Close: sell 0.1 BTC at $49000 (loss = 0.1 * 1000 = -$100)
        let realized = ledger.apply_perp_fill(
            &btc_perp(),
            OrderSide::Sell,
            usdc(49000),
            Decimal::new(1, 1),
            Decimal::ZERO,
        );

        assert_eq!(realized, -usdc(100), "Realized PnL should be -$100");

        // free = 10000 - 500(open) + 500(released) + (-100)(loss) = 9900
        assert_eq!(ledger.free_usdc(), usdc(9900));
    }

    #[test]
    fn test_fill_short_and_close() {
        let mut ledger = MarginLedger::new(usdc(10000), Decimal::ZERO);
        ledger.set_leverage(&eth_perp(), Decimal::new(5, 0), Decimal::new(25, 0));

        // Open short: sell 1 ETH at $3000
        ledger.apply_perp_fill(
            &eth_perp(),
            OrderSide::Sell,
            usdc(3000),
            Decimal::ONE,
            Decimal::ZERO,
        );

        let pos = ledger.position(&eth_perp()).unwrap();
        assert_eq!(pos.qty, -Decimal::ONE); // short
        assert_eq!(pos.isolated_margin_reserved, usdc(600)); // 3000/5 = 600

        // Close short: buy 1 ETH at $2800 (profit = $200)
        let realized = ledger.apply_perp_fill(
            &eth_perp(),
            OrderSide::Buy,
            usdc(2800),
            Decimal::ONE,
            Decimal::ZERO,
        );

        assert_eq!(realized, usdc(200), "Short profit should be $200");
    }

    #[test]
    fn test_equity_with_unrealized() {
        let mut ledger = MarginLedger::new(usdc(10000), Decimal::ZERO);
        ledger.set_leverage(&btc_perp(), Decimal::new(10, 0), Decimal::new(50, 0));

        // Open: buy 0.1 BTC at $50000
        ledger.apply_perp_fill(
            &btc_perp(),
            OrderSide::Buy,
            usdc(50000),
            Decimal::new(1, 1),
            Decimal::ZERO,
        );

        // Mark at $51000 → unrealized = 0.1 * 1000 = $100
        let mut marks = HashMap::new();
        marks.insert(btc_perp(), usdc(51000));

        let equity = ledger.equity(&marks);
        // equity = free(9500) + reserved(500) + unrealized(100) = 10100
        assert_eq!(equity, usdc(10100));
    }

    #[test]
    fn test_position_snapshots() {
        let mut ledger = MarginLedger::new(usdc(10000), Decimal::ZERO);
        ledger.set_leverage(&btc_perp(), Decimal::new(10, 0), Decimal::new(50, 0));

        ledger.apply_perp_fill(
            &btc_perp(),
            OrderSide::Buy,
            usdc(50000),
            Decimal::new(1, 1),
            Decimal::ZERO,
        );

        let mut marks = HashMap::new();
        marks.insert(btc_perp(), usdc(51000));

        let snapshots = ledger.position_snapshots(&marks);
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].instrument, btc_perp());
        assert_eq!(snapshots[0].qty, Decimal::new(1, 1));
        assert_eq!(snapshots[0].unrealized_pnl, Some(usdc(100)));
    }

    #[test]
    fn test_liquidation_detection() {
        let mut ledger = MarginLedger::new(usdc(1000), Decimal::ZERO);
        ledger.set_leverage(&btc_perp(), Decimal::new(10, 0), Decimal::new(50, 0));

        // Open: buy 0.002 BTC at $50000 (notional = $100)
        // margin = 100/10 = $10
        ledger.apply_perp_fill(
            &btc_perp(),
            OrderSide::Buy,
            usdc(50000),
            Decimal::new(2, 3), // 0.002 BTC
            Decimal::ZERO,
        );

        let pos = ledger.position(&btc_perp()).unwrap();
        let liq_price = pos.liquidation_price();
        assert!(liq_price.is_some(), "Should have a liquidation price");

        // Well above liq price → not liquidated
        let mut marks = HashMap::new();
        marks.insert(btc_perp(), usdc(50000));
        assert!(ledger.check_liquidations(&marks).is_empty());

        // Drop price dramatically → should liquidate
        marks.insert(btc_perp(), usdc(1000));
        let liquidated = ledger.check_liquidations(&marks);
        assert!(!liquidated.is_empty(), "Should detect liquidation at $1000");
    }
}
