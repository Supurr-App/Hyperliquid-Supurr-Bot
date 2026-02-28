//! Liquidation price calculation tests
//!
//! Test cases for verifying liquidation boundary calculations match Hyperliquid's formula.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Hyperliquid liquidation price formula:
///
/// liq_price = entry - side_val * margin_available / position_size / (1 - l * side_val)
///
/// where:
///   l = 1 / maintenance_leverage (MMR)
///   side_val = 1 for long, -1 for short
///   margin_available = isolated_margin - maintenance_margin
///   isolated_margin = notional / leverage
///   maintenance_margin = notional * l
///   position_size = notional / entry_price
fn calculate_liquidation_price(
    entry_price: Decimal,
    maintenance_leverage: Decimal,
    leverage: Decimal,
    notional: Decimal,
    is_long: bool,
) -> Decimal {
    let one = Decimal::ONE;
    let side_val = if is_long { one } else { -one };

    // l = maintenance margin rate = 1 / maintenance_leverage
    let l = one / maintenance_leverage;

    // Initial margin rate = 1 / leverage
    let imr = one / leverage;

    // Position size in asset units
    let position_size = notional / entry_price;

    // Isolated margin = initial margin = notional / leverage
    let isolated_margin = notional * imr;

    // Maintenance margin required = notional * l
    let maintenance_margin = notional * l;

    // margin_available = isolated_margin - maintenance_margin
    let margin_available = isolated_margin - maintenance_margin;

    // Denominator = 1 - l * side_val
    let denominator = one - l * side_val;

    // liq_price = entry - side_val * margin_available / position_size / denominator
    entry_price - side_val * margin_available / position_size / denominator
}

#[cfg(test)]
mod tests {
    use super::*;

    /// User's config:
    /// {
    ///   "start_price": 230.278,
    ///   "end_price": 254.0,
    ///   "total_amount_quote": 15.0,
    ///   "leverage": 10,
    ///   "max_leverage": 10,
    ///   "grid_levels": 3,
    ///   "is_directional": true,
    ///   "side": 1  // LONG
    /// }
    ///
    /// Calculations:
    /// - maintenance_leverage = max_leverage * 2 = 20
    /// - effective_investment = 15.0 * 0.9998 = 14.997 (with fee buffer)
    /// - notional_budget = 14.997 * 10 = 149.97
    /// - quote_per_level = 149.97 / (3-1) = 74.985
    ///
    /// For LONG mode:
    /// - Grid calculates liq_price using end_price (254.0) as entry
    /// - This is checked against start_price (230.278)
    #[test]
    fn test_user_config_long_liquidation() {
        let entry_price = dec!(254.0); // end_price for LONG
        let maintenance_leverage = dec!(20); // max_leverage * 2
        let leverage = dec!(10);

        // Calculations:
        // effective_investment = 15.0 * 0.9998 = 14.997
        // notional_budget = 14.997 * 10 = 149.97
        // quote_per_level = 149.97 / 2 = 74.985
        let notional_per_level = dec!(74.985);

        let liq_price = calculate_liquidation_price(
            entry_price,
            maintenance_leverage,
            leverage,
            notional_per_level,
            true, // is_long
        );

        let start_price = dec!(230.278);

        println!("=== LONG Grid Liquidation Test ===");
        println!("Entry price (end_price): {}", entry_price);
        println!("Start price (grid_start): {}", start_price);
        println!("Leverage: {}", leverage);
        println!("Maintenance leverage: {}", maintenance_leverage);
        println!("Notional per level: {}", notional_per_level);
        println!("Calculated liquidation price: {}", liq_price);
        println!();

        // Step-by-step calculation verification:
        let l = dec!(1) / maintenance_leverage; // 0.05
        let imr = dec!(1) / leverage; // 0.1
        let position_size = notional_per_level / entry_price;
        let isolated_margin = notional_per_level * imr;
        let maintenance_margin = notional_per_level * l;
        let margin_available = isolated_margin - maintenance_margin;
        let denominator = dec!(1) - l; // For long: 1 - l * 1 = 1 - 0.05 = 0.95

        println!("--- Step by step ---");
        println!("l (MMR) = 1/{} = {}", maintenance_leverage, l);
        println!("IMR = 1/{} = {}", leverage, imr);
        println!(
            "position_size = {}/{} = {}",
            notional_per_level, entry_price, position_size
        );
        println!(
            "isolated_margin = {} * {} = {}",
            notional_per_level, imr, isolated_margin
        );
        println!(
            "maintenance_margin = {} * {} = {}",
            notional_per_level, l, maintenance_margin
        );
        println!(
            "margin_available = {} - {} = {}",
            isolated_margin, maintenance_margin, margin_available
        );
        println!("denominator = 1 - {} = {}", l, denominator);
        println!(
            "price_drop = {} / {} / {} = {}",
            margin_available,
            position_size,
            denominator,
            margin_available / position_size / denominator
        );
        println!(
            "liq_price = {} - {} = {}",
            entry_price,
            margin_available / position_size / denominator,
            liq_price
        );

        // Verify: For grid to be safe, liq_price must be BELOW start_price
        // If liq_price > start_price, the grid is unsafe
        assert!(
            liq_price < entry_price,
            "Liq price should be below entry for LONG"
        );

        // The key test: is liq_price below start_price?
        if liq_price > start_price {
            println!(
                "\n⚠️ GRID UNSAFE: liq_price {} > start_price {}",
                liq_price, start_price
            );
        } else {
            println!(
                "\n✅ GRID SAFE: liq_price {} < start_price {}",
                liq_price, start_price
            );
        }
    }

    #[test]
    fn test_short_grid_liquidation() {
        // For SHORT: entry at start_price, liq_price should be above end_price
        let entry_price = dec!(230.278); // start_price for SHORT
        let maintenance_leverage = dec!(20);
        let leverage = dec!(10);
        let notional_per_level = dec!(74.985);

        let liq_price = calculate_liquidation_price(
            entry_price,
            maintenance_leverage,
            leverage,
            notional_per_level,
            false, // is_short
        );

        let end_price = dec!(254.0);

        println!("=== SHORT Grid Liquidation Test ===");
        println!("Entry price (start_price): {}", entry_price);
        println!("End price (grid_end): {}", end_price);
        println!("Calculated liquidation price: {}", liq_price);

        // For SHORT, liq_price should be ABOVE entry (price goes up = liquidation)
        assert!(
            liq_price > entry_price,
            "Liq price should be above entry for SHORT"
        );

        // The key test: is liq_price above end_price?
        if liq_price < end_price {
            println!(
                "\n⚠️ GRID UNSAFE: liq_price {} < end_price {}",
                liq_price, end_price
            );
        } else {
            println!(
                "\n✅ GRID SAFE: liq_price {} > end_price {}",
                liq_price, end_price
            );
        }
    }

    #[test]
    fn test_neutral_grid_liquidation() {
        // NEUTRAL mode: both long and short boundaries
        let maintenance_leverage = dec!(20);
        let leverage = dec!(10);
        let notional_per_level = dec!(74.985);
        let start_price = dec!(230.278);
        let end_price = dec!(254.0);

        // For long side (entries below mid)
        let long_entry = start_price; // first_buy_price
        let long_liq = calculate_liquidation_price(
            long_entry,
            maintenance_leverage,
            leverage,
            notional_per_level,
            true,
        );

        // For short side (entries above mid)
        let short_entry = end_price; // first_sell_price
        let short_liq = calculate_liquidation_price(
            short_entry,
            maintenance_leverage,
            leverage,
            notional_per_level,
            false,
        );

        println!("=== NEUTRAL Grid Liquidation Test ===");
        println!("Long entry: {} -> Liq: {}", long_entry, long_liq);
        println!("Short entry: {} -> Liq: {}", short_entry, short_liq);
        println!("Grid range: {} to {}", start_price, end_price);

        // Long liq should be below start_price
        if long_liq > start_price {
            println!(
                "\n⚠️ LONG SIDE UNSAFE: liq_price {} > start_price {}",
                long_liq, start_price
            );
        } else {
            println!(
                "\n✅ LONG SIDE SAFE: liq_price {} < start_price {}",
                long_liq, start_price
            );
        }

        // Short liq should be above end_price
        if short_liq < end_price {
            println!(
                "⚠️ SHORT SIDE UNSAFE: liq_price {} < end_price {}",
                short_liq, end_price
            );
        } else {
            println!(
                "✅ SHORT SIDE SAFE: liq_price {} > end_price {}",
                short_liq, end_price
            );
        }
    }

    /// Compare with simple formula: liq = entry * (1 - 1/leverage)
    #[test]
    fn test_simple_vs_hyperliquid_formula() {
        let entry = dec!(100.0);
        let leverage = dec!(10);
        let maintenance_leverage = dec!(20);
        let notional = dec!(1000.0);

        // Simple formula (commonly used approximation)
        let simple_liq = entry * (dec!(1) - dec!(1) / leverage); // 90.0

        // Hyperliquid formula
        let hl_liq =
            calculate_liquidation_price(entry, maintenance_leverage, leverage, notional, true);

        println!("Entry: {}", entry);
        println!("Simple formula liq: {}", simple_liq);
        println!("Hyperliquid formula liq: {}", hl_liq);
        println!("Difference: {}", (simple_liq - hl_liq).abs());

        // They won't be exactly equal because HL uses maintenance leverage
        // HL's formula accounts for the gap between initial and maintenance margin
    }
}
