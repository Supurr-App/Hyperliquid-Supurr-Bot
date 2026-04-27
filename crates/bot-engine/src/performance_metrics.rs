//! Shared performance metrics for backtests and upstream sync payloads.

use bot_core::{now_ms, InstrumentId, OrderFilledEvent, OrderSide, Position};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

const YEAR_MS: f64 = 365.0 * 24.0 * 60.0 * 60.0 * 1000.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestEquityPoint {
    pub ts_ms: i64,
    pub equity: String,
    pub net_pnl: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestClosedTrade {
    pub entry_ts_ms: i64,
    pub exit_ts_ms: i64,
    pub side: String,
    pub qty: String,
    pub entry_price: String,
    pub exit_price: String,
    pub gross_pnl: String,
    pub fees: String,
    pub net_pnl: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceBenchmark {
    pub start_ts_ms: Option<i64>,
    pub end_ts_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    pub quote_count: usize,
    pub starting_balance_usdc: Option<String>,
    pub ending_balance_usdc: Option<String>,
    pub instrument: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceMetrics {
    pub period_return_pct: Option<f64>,
    pub apr_pct: Option<f64>,
    pub sharpe: Option<f64>,
    pub max_drawdown_pct: Option<f64>,
    pub max_drawdown_usdc: String,
    pub win_rate_pct: Option<f64>,
    pub closed_trade_count: usize,
    pub winning_trade_count: usize,
    pub losing_trade_count: usize,
    pub fill_count: usize,
    pub total_fees: String,
    pub total_volume: String,
    pub net_pnl: String,
    pub fee_drag_pct: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceMetricsSnapshot {
    pub schema_version: u32,
    pub mode: String,
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_started_at_ms: Option<i64>,
    pub metrics: PerformanceMetrics,
    pub benchmark: PerformanceBenchmark,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_equity: Option<BacktestEquityPoint>,
}

#[derive(Debug, Clone)]
pub struct PerformanceTracker {
    mode: String,
    scope: String,
    run_started_at_ms: Option<i64>,
    starting_balance_usdc: Option<Decimal>,
    instrument: Option<InstrumentId>,
    equity_curve: Vec<BacktestEquityPoint>,
    quote_count: usize,
}

impl PerformanceTracker {
    pub fn new(
        mode: impl Into<String>,
        starting_balance_usdc: Option<Decimal>,
        instrument: Option<InstrumentId>,
    ) -> Self {
        let mode = mode.into();
        let is_live_like = matches!(mode.as_str(), "live" | "paper");
        Self {
            scope: if is_live_like {
                "current_run".to_string()
            } else {
                "backtest_window".to_string()
            },
            run_started_at_ms: is_live_like.then(now_ms),
            mode,
            starting_balance_usdc,
            instrument,
            equity_curve: Vec::new(),
            quote_count: 0,
        }
    }

    pub fn set_instrument(&mut self, instrument: InstrumentId) {
        if self.instrument.is_none() {
            self.instrument = Some(instrument);
        }
    }

    pub fn record_equity_point(&mut self, ts_ms: i64, price: Option<Decimal>, net_pnl: Decimal) {
        self.quote_count += 1;

        let Some(starting_balance) = self.starting_balance_usdc else {
            return;
        };

        self.equity_curve.push(BacktestEquityPoint {
            ts_ms,
            equity: (starting_balance + net_pnl).to_string(),
            net_pnl: net_pnl.to_string(),
            price: price.map(|p| p.to_string()),
        });
    }

    pub fn snapshot(
        &self,
        fills: &[OrderFilledEvent],
        positions: &[Position],
    ) -> PerformanceMetricsSnapshot {
        let total_volume = total_volume(fills);
        let total_fees = positions.iter().map(|p| p.total_fees).sum::<Decimal>();
        let net_pnl = positions.iter().map(Position::current_pnl).sum::<Decimal>();
        let closed_trades = build_closed_trades(fills);
        let benchmark = self.benchmark(net_pnl);
        let metrics = calculate_metrics(
            self.starting_balance_usdc,
            &self.equity_curve,
            &closed_trades,
            fills.len(),
            total_volume,
            total_fees,
            net_pnl,
        );

        PerformanceMetricsSnapshot {
            schema_version: 1,
            mode: self.mode.clone(),
            scope: self.scope.clone(),
            run_started_at_ms: self.run_started_at_ms,
            metrics,
            benchmark,
            latest_equity: self.equity_curve.last().cloned(),
        }
    }

    pub fn equity_curve(&self) -> Vec<BacktestEquityPoint> {
        self.equity_curve.clone()
    }

    pub fn closed_trades(&self, fills: &[OrderFilledEvent]) -> Vec<BacktestClosedTrade> {
        build_closed_trades(fills)
    }

    pub fn metrics(
        &self,
        fills: &[OrderFilledEvent],
        positions: &[Position],
    ) -> PerformanceMetrics {
        self.snapshot(fills, positions).metrics
    }

    pub fn benchmark(&self, net_pnl: Decimal) -> PerformanceBenchmark {
        let start_ts_ms = self.equity_curve.first().map(|p| p.ts_ms);
        let end_ts_ms = self.equity_curve.last().map(|p| p.ts_ms);
        let duration_ms = match (start_ts_ms, end_ts_ms) {
            (Some(start), Some(end)) if end >= start => Some(end - start),
            _ => None,
        };
        let ending_balance_usdc = self
            .starting_balance_usdc
            .map(|balance| (balance + net_pnl).to_string());

        PerformanceBenchmark {
            start_ts_ms,
            end_ts_ms,
            duration_ms,
            quote_count: self.quote_count,
            starting_balance_usdc: self.starting_balance_usdc.map(|v| v.to_string()),
            ending_balance_usdc,
            instrument: self.instrument.as_ref().map(|i| i.0.clone()),
        }
    }
}

pub fn total_volume(fills: &[OrderFilledEvent]) -> Decimal {
    fills.iter().map(|fill| fill.qty.0 * fill.price.0).sum()
}

pub fn build_closed_trades(fills: &[OrderFilledEvent]) -> Vec<BacktestClosedTrade> {
    let mut ordered = fills.to_vec();
    ordered.sort_by_key(|fill| fill.ts);

    let mut position_qty = Decimal::ZERO;
    let mut avg_entry = Decimal::ZERO;
    let mut open_ts_ms: Option<i64> = None;
    let mut open_fee_pool = Decimal::ZERO;
    let mut closed_trades = Vec::new();

    for fill in ordered {
        let fill_sign = match fill.side {
            OrderSide::Buy => Decimal::ONE,
            OrderSide::Sell => -Decimal::ONE,
        };
        let mut remaining_qty = fill.qty.0;
        let mut remaining_fee = fill.fee.amount;

        while remaining_qty > Decimal::ZERO {
            let same_side = (position_qty > Decimal::ZERO && fill_sign > Decimal::ZERO)
                || (position_qty < Decimal::ZERO && fill_sign < Decimal::ZERO);
            if position_qty == Decimal::ZERO || same_side {
                let new_abs_qty = position_qty.abs() + remaining_qty;
                avg_entry = if new_abs_qty > Decimal::ZERO {
                    ((avg_entry * position_qty.abs()) + (fill.price.0 * remaining_qty))
                        / new_abs_qty
                } else {
                    Decimal::ZERO
                };
                position_qty += fill_sign * remaining_qty;
                open_fee_pool += remaining_fee;
                open_ts_ms.get_or_insert(fill.ts);
                break;
            }

            let position_abs = position_qty.abs();
            let closing_qty = remaining_qty.min(position_abs);
            let fee_ratio = if remaining_qty > Decimal::ZERO {
                closing_qty / remaining_qty
            } else {
                Decimal::ZERO
            };
            let close_fee = remaining_fee * fee_ratio;
            let open_fee = if position_abs > Decimal::ZERO {
                open_fee_pool * (closing_qty / position_abs)
            } else {
                Decimal::ZERO
            };

            let gross_pnl = if position_qty > Decimal::ZERO {
                (fill.price.0 - avg_entry) * closing_qty
            } else {
                (avg_entry - fill.price.0) * closing_qty
            };
            let fees = open_fee + close_fee;
            let side = if position_qty > Decimal::ZERO {
                "LONG"
            } else {
                "SHORT"
            };

            closed_trades.push(BacktestClosedTrade {
                entry_ts_ms: open_ts_ms.unwrap_or(fill.ts),
                exit_ts_ms: fill.ts,
                side: side.to_string(),
                qty: closing_qty.to_string(),
                entry_price: avg_entry.to_string(),
                exit_price: fill.price.0.to_string(),
                gross_pnl: gross_pnl.to_string(),
                fees: fees.to_string(),
                net_pnl: (gross_pnl - fees).to_string(),
            });

            position_qty += fill_sign * closing_qty;
            open_fee_pool -= open_fee;
            remaining_qty -= closing_qty;
            remaining_fee -= close_fee;

            if position_qty == Decimal::ZERO {
                avg_entry = Decimal::ZERO;
                open_fee_pool = Decimal::ZERO;
                open_ts_ms = None;
            }
        }
    }

    closed_trades
}

fn calculate_metrics(
    starting_balance_usdc: Option<Decimal>,
    equity_curve: &[BacktestEquityPoint],
    closed_trades: &[BacktestClosedTrade],
    fill_count: usize,
    total_volume: Decimal,
    total_fees: Decimal,
    net_pnl: Decimal,
) -> PerformanceMetrics {
    let winning_trade_count = closed_trades
        .iter()
        .filter(|trade| decimal_from_str(&trade.net_pnl) > Decimal::ZERO)
        .count();
    let losing_trade_count = closed_trades
        .iter()
        .filter(|trade| decimal_from_str(&trade.net_pnl) < Decimal::ZERO)
        .count();
    let closed_trade_count = closed_trades.len();
    let win_rate_pct = if closed_trade_count > 0 {
        Some((winning_trade_count as f64 / closed_trade_count as f64) * 100.0)
    } else {
        None
    };

    let period_return_pct = starting_balance_usdc.and_then(|balance| {
        if balance > Decimal::ZERO {
            Some((decimal_to_f64(net_pnl / balance)) * 100.0)
        } else {
            None
        }
    });
    let duration_ms = match (equity_curve.first(), equity_curve.last()) {
        (Some(first), Some(last)) if last.ts_ms > first.ts_ms => {
            Some((last.ts_ms - first.ts_ms) as f64)
        }
        _ => None,
    };
    let apr_pct = match (period_return_pct, duration_ms) {
        (Some(return_pct), Some(duration)) if duration > 0.0 => {
            Some((return_pct / 100.0) * (YEAR_MS / duration) * 100.0)
        }
        _ => None,
    };
    let (max_drawdown_usdc, max_drawdown_pct) = max_drawdown(equity_curve);

    PerformanceMetrics {
        period_return_pct,
        apr_pct,
        sharpe: sharpe(equity_curve),
        max_drawdown_pct,
        max_drawdown_usdc: max_drawdown_usdc.to_string(),
        win_rate_pct,
        closed_trade_count,
        winning_trade_count,
        losing_trade_count,
        fill_count,
        total_fees: total_fees.to_string(),
        total_volume: total_volume.to_string(),
        net_pnl: net_pnl.to_string(),
        fee_drag_pct: starting_balance_usdc.and_then(|balance| {
            if balance > Decimal::ZERO {
                Some(decimal_to_f64(total_fees / balance) * 100.0)
            } else {
                None
            }
        }),
    }
}

fn max_drawdown(equity_curve: &[BacktestEquityPoint]) -> (Decimal, Option<f64>) {
    let mut peak: Option<Decimal> = None;
    let mut max_dd = Decimal::ZERO;
    let mut max_dd_pct: Option<f64> = None;

    for point in equity_curve {
        let equity = decimal_from_str(&point.equity);
        peak = Some(match peak {
            Some(existing) if existing > equity => existing,
            _ => equity,
        });

        if let Some(current_peak) = peak {
            let drawdown = current_peak - equity;
            if drawdown > max_dd {
                max_dd = drawdown;
                max_dd_pct = if current_peak > Decimal::ZERO {
                    Some(decimal_to_f64(drawdown / current_peak) * 100.0)
                } else {
                    None
                };
            }
        }
    }

    (max_dd, max_dd_pct)
}

fn sharpe(equity_curve: &[BacktestEquityPoint]) -> Option<f64> {
    if equity_curve.len() < 3 {
        return None;
    }

    let mut returns = Vec::new();
    for pair in equity_curve.windows(2) {
        let prev = decimal_to_f64(decimal_from_str(&pair[0].equity));
        let current = decimal_to_f64(decimal_from_str(&pair[1].equity));
        if prev == 0.0 {
            continue;
        }
        returns.push((current - prev) / prev);
    }

    if returns.len() < 2 {
        return None;
    }

    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance =
        returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (returns.len() - 1) as f64;
    let std_dev = variance.sqrt();
    if std_dev == 0.0 {
        return None;
    }

    let first_ts = equity_curve.first()?.ts_ms;
    let last_ts = equity_curve.last()?.ts_ms;
    if last_ts <= first_ts {
        return None;
    }
    let average_sample_ms = (last_ts - first_ts) as f64 / returns.len() as f64;
    if average_sample_ms <= 0.0 {
        return None;
    }

    Some((mean / std_dev) * (YEAR_MS / average_sample_ms).sqrt())
}

fn decimal_to_f64(value: Decimal) -> f64 {
    value.to_string().parse::<f64>().unwrap_or(0.0)
}

fn decimal_from_str(value: &str) -> Decimal {
    value.parse::<Decimal>().unwrap_or(Decimal::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bot_core::{AssetId, ClientOrderId, ExchangeId, Fee, Price, Qty, TradeId};
    use rust_decimal_macros::dec;

    fn point(ts_ms: i64, equity: Decimal) -> BacktestEquityPoint {
        BacktestEquityPoint {
            ts_ms,
            equity: equity.to_string(),
            net_pnl: (equity - dec!(1000)).to_string(),
            price: None,
        }
    }

    fn assert_close(actual: Option<f64>, expected: f64) {
        let actual = actual.expect("metric should be present");
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }

    fn assert_decimal_str(actual: &str, expected: Decimal) {
        assert_eq!(decimal_from_str(actual), expected);
    }

    fn fill(
        id: &str,
        side: OrderSide,
        price: Decimal,
        qty: Decimal,
        fee: Decimal,
        ts: i64,
    ) -> OrderFilledEvent {
        OrderFilledEvent {
            exchange: ExchangeId::new("hyperliquid"),
            trade_id: TradeId::new(id),
            client_id: ClientOrderId::new(format!("client-{}", id)),
            instrument: InstrumentId::new("BTC-PERP"),
            side,
            price: Price::new(price),
            qty: Qty::new(qty),
            net_qty: Qty::new(qty),
            fee: Fee::new(fee, AssetId::new("USDC")),
            ts,
        }
    }

    #[test]
    fn closed_trades_include_allocated_open_and_close_fees() {
        let fills = vec![
            fill("1", OrderSide::Buy, dec!(100), dec!(1), dec!(0.10), 1),
            fill("2", OrderSide::Sell, dec!(110), dec!(1), dec!(0.10), 2),
        ];

        let closed = build_closed_trades(&fills);

        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].gross_pnl, "10");
        assert_eq!(closed[0].fees, "0.20");
        assert_eq!(closed[0].net_pnl, "9.80");
    }

    #[test]
    fn closed_trades_handle_partial_close_and_reversal() {
        let fills = vec![
            fill("1", OrderSide::Buy, dec!(100), dec!(2), dec!(0.20), 1),
            fill("2", OrderSide::Sell, dec!(110), dec!(3), dec!(0.30), 2),
            fill("3", OrderSide::Buy, dec!(90), dec!(1), dec!(0.10), 3),
        ];

        let closed = build_closed_trades(&fills);

        assert_eq!(closed.len(), 2);
        assert_eq!(closed[0].side, "LONG");
        assert_decimal_str(&closed[0].qty, dec!(2));
        assert_decimal_str(&closed[0].gross_pnl, dec!(20));
        assert_decimal_str(&closed[0].fees, dec!(0.40));
        assert_decimal_str(&closed[0].net_pnl, dec!(19.60));
        assert_eq!(closed[1].side, "SHORT");
        assert_decimal_str(&closed[1].qty, dec!(1));
        assert_decimal_str(&closed[1].gross_pnl, dec!(20));
        assert_decimal_str(&closed[1].fees, dec!(0.20));
        assert_decimal_str(&closed[1].net_pnl, dec!(19.80));
    }

    #[test]
    fn metrics_match_known_apr_drawdown_fee_and_volume_values() {
        let year_ms = YEAR_MS as i64;
        let equity_curve = vec![
            point(0, dec!(1000)),
            point(year_ms / 4, dec!(900)),
            point(year_ms / 2, dec!(1100)),
        ];

        let metrics = calculate_metrics(
            Some(dec!(1000)),
            &equity_curve,
            &[],
            2,
            dec!(2500),
            dec!(5),
            dec!(100),
        );

        assert_close(metrics.period_return_pct, 10.0);
        assert_close(metrics.apr_pct, 20.0);
        assert_eq!(metrics.max_drawdown_usdc, "100");
        assert_close(metrics.max_drawdown_pct, 10.0);
        assert_eq!(metrics.total_volume, "2500");
        assert_eq!(metrics.total_fees, "5");
        assert_eq!(metrics.net_pnl, "100");
        assert_close(metrics.fee_drag_pct, 0.5);
    }

    #[test]
    fn sharpe_matches_known_sample_return_formula() {
        let equity_curve = vec![
            point(0, dec!(1000)),
            point(86_400_000, dec!(1010)),
            point(172_800_000, dec!(1000)),
            point(259_200_000, dec!(1020)),
        ];

        let metrics = calculate_metrics(
            Some(dec!(1000)),
            &equity_curve,
            &[],
            0,
            Decimal::ZERO,
            Decimal::ZERO,
            dec!(20),
        );

        let returns = [10.0 / 1000.0, -10.0 / 1010.0, 20.0 / 1000.0];
        let mean = returns.iter().sum::<f64>() / returns.len() as f64;
        let variance =
            returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (returns.len() - 1) as f64;
        let expected = (mean / variance.sqrt()) * 365.0_f64.sqrt();

        assert_close(metrics.sharpe, expected);
    }

    #[test]
    fn metrics_return_nulls_when_data_is_insufficient() {
        let tracker =
            PerformanceTracker::new("backtest", Some(dec!(1000)), Some(InstrumentId::new("BTC")));
        let metrics = tracker.metrics(&[], &[]);

        assert_eq!(metrics.period_return_pct, Some(0.0));
        assert_eq!(metrics.apr_pct, None);
        assert_eq!(metrics.sharpe, None);
        assert_eq!(metrics.win_rate_pct, None);
    }

    #[test]
    fn drawdown_and_win_rate_are_computed_from_curve_and_closed_trades() {
        let mut tracker =
            PerformanceTracker::new("backtest", Some(dec!(1000)), Some(InstrumentId::new("BTC")));
        tracker.record_equity_point(1, Some(dec!(100)), dec!(0));
        tracker.record_equity_point(2, Some(dec!(95)), dec!(-100));
        tracker.record_equity_point(3, Some(dec!(110)), dec!(100));

        let fills = vec![
            fill("1", OrderSide::Buy, dec!(100), dec!(1), dec!(0), 1),
            fill("2", OrderSide::Sell, dec!(110), dec!(1), dec!(0), 2),
        ];
        let metrics = tracker.metrics(&fills, &[]);

        assert_eq!(metrics.closed_trade_count, 1);
        assert_eq!(metrics.win_rate_pct, Some(100.0));
        assert_eq!(metrics.max_drawdown_usdc, "100");
        assert_eq!(metrics.max_drawdown_pct, Some(10.0));
        assert!(metrics.apr_pct.is_some());
    }
}
