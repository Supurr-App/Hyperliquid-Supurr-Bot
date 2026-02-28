//! Mock Exchange for deterministic testing.
//!
//! Provides a stateful simulation of an exchange without real API calls.
//! Supports behavioral controls ("knobs") to test failure modes.

use async_lock::RwLock;
use bot_core::{
    AccountState, AssetId, ClientOrderId, Environment, Exchange, ExchangeError, ExchangeId,
    ExchangeOrderId, Fill, InstrumentId, OrderInput, OrderSide, PlaceOrderResult, PositionSnapshot,
    Price, Qty, Quote, TimeInForce, TradeId,
};
use rust_decimal::Decimal;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

/// Behavioral control for order execution
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderFailMode {
    /// Orders always succeed and fill immediately
    AlwaysSucceed,
    /// Orders fail if insufficient balance (spot) or margin (perp)
    FailOnInsufficientBalance,
    /// IOC orders always reject (no immediate liquidity)
    IocAlwaysReject,
}

impl Default for OrderFailMode {
    fn default() -> Self {
        Self::AlwaysSucceed
    }
}

/// Behavioral controls ("knobs") for testing
#[derive(Debug, Clone)]
pub struct MockKnobs {
    pub order_fail_mode: OrderFailMode,
    pub exchange_health: bot_core::ExchangeHealth,
    pub should_timeout: bool,
    pub should_rate_limit: bool,
    /// Fill all orders immediately regardless of TIF (for backtesting)
    pub fill_all_immediately: bool,
}

impl Default for MockKnobs {
    fn default() -> Self {
        Self {
            order_fail_mode: OrderFailMode::AlwaysSucceed,
            exchange_health: bot_core::ExchangeHealth::Active,
            should_timeout: false,
            should_rate_limit: false,
            fill_all_immediately: false,
        }
    }
}

/// Internal position tracking
#[derive(Debug, Clone)]
struct Position {
    qty: Decimal,
    avg_entry_px: Price,
    unrealized_pnl: Decimal,
}

/// Pending order for price-crossing fill simulation
#[derive(Debug, Clone)]
struct PendingOrder {
    oid: u64,
    order: OrderInput,
}

/// Internal mock state
struct MockState {
    // Account state
    balances: HashMap<AssetId, Decimal>,
    positions: HashMap<InstrumentId, Position>,
    account_value: Decimal,

    // Market data
    current_mids: HashMap<String, Decimal>,

    // Order tracking
    next_oid: u64,
    fills: Vec<Fill>,

    // Event recording
    placed_orders: Vec<OrderInput>,

    // Behavioral controls
    knobs: MockKnobs,

    // Time control
    time_ms: i64,

    // Queued quotes for backtesting (FIFO)
    quote_queue: VecDeque<Quote>,

    // Pending limit orders (for realistic fill simulation)
    pending_orders: HashMap<u64, PendingOrder>,
}

impl MockState {
    fn allocate_oid(&mut self) -> u64 {
        let oid = self.next_oid;
        self.next_oid += 1;
        oid
    }

    fn execute_immediate_fill(&mut self, order: &OrderInput, oid: u64) -> Fill {
        let trade_id = TradeId::new(format!("mock_{}", oid));

        let fill = Fill {
            trade_id: trade_id.clone(),
            client_id: Some(order.client_id.clone()),
            exchange_order_id: Some(ExchangeOrderId::new(oid.to_string())),
            instrument: order.instrument.clone(),
            side: order.side,
            price: order.price,
            qty: order.qty,
            fee: bot_core::Fee::new(Decimal::ZERO, AssetId::new("USDC")),
            ts: self.time_ms,
        };

        // Update balances based on fill
        self.update_balances_after_fill(&fill);

        fill
    }

    fn update_balances_after_fill(&mut self, fill: &Fill) {
        let quote_asset = AssetId::new("USDC");

        // Extract base asset from instrument (e.g., "ETH-SPOT" -> "ETH")
        let instrument_str = fill.instrument.to_string();
        let base_asset = if let Some(pos) = instrument_str.rfind('-') {
            AssetId::new(&instrument_str[..pos])
        } else {
            AssetId::new(&instrument_str)
        };

        let notional = fill.price.0 * fill.qty.0;

        match fill.side {
            OrderSide::Buy => {
                // Deduct quote, add base
                *self.balances.entry(quote_asset).or_default() -= notional;
                *self.balances.entry(base_asset).or_default() += fill.qty.0;
            }
            OrderSide::Sell => {
                // Add quote, deduct base
                *self.balances.entry(quote_asset).or_default() += notional;
                *self.balances.entry(base_asset).or_default() -= fill.qty.0;
            }
        }
    }

    fn check_balance(&self, order: &OrderInput) -> Result<(), String> {
        let quote_asset = AssetId::new("USDC");

        if order.side == OrderSide::Buy {
            // Check if we have enough quote currency
            let required = order.price.0 * order.qty.0;
            let available = self.balances.get(&quote_asset).copied().unwrap_or_default();

            if required > available {
                return Err(format!(
                    "Insufficient balance: need {} USDC, have {}",
                    required, available
                ));
            }
        } else {
            // Check if we have enough base asset
            let instrument_str = order.instrument.to_string();
            let base_asset = if let Some(pos) = instrument_str.rfind('-') {
                AssetId::new(&instrument_str[..pos])
            } else {
                AssetId::new(&instrument_str)
            };

            let available = self.balances.get(&base_asset).copied().unwrap_or_default();

            if order.qty.0 > available {
                return Err(format!(
                    "Insufficient balance: need {} {}, have {}",
                    order.qty.0, base_asset, available
                ));
            }
        }

        Ok(())
    }
}

/// Mock Exchange - stateful simulation for testing
pub struct MockExchange {
    exchange_id: ExchangeId,
    environment: Environment,
    inner: Arc<RwLock<MockState>>,
}

impl MockExchange {
    /// Create a new mock exchange with initial balances
    pub fn new_with_balances(balances: HashMap<AssetId, Decimal>) -> Self {
        Self {
            // Use "hyperliquid" to match production Market enum's exchange_id
            exchange_id: ExchangeId::new("hyperliquid"),
            environment: Environment::Testnet,
            inner: Arc::new(RwLock::new(MockState {
                balances,
                positions: HashMap::new(),
                account_value: Decimal::ZERO,
                current_mids: HashMap::new(),
                next_oid: 1000,
                fills: Vec::new(),
                placed_orders: Vec::new(),
                knobs: MockKnobs::default(),
                time_ms: 0,
                quote_queue: VecDeque::new(),
                pending_orders: HashMap::new(),
            })),
        }
    }

    /// Create a new mock exchange with default balances (for testing)
    pub fn new() -> Self {
        let mut balances = HashMap::new();
        balances.insert(AssetId::new("USDC"), Decimal::new(100000, 0)); // 100k USDC
        balances.insert(AssetId::new("ETH"), Decimal::new(10, 0)); // 10 ETH
        Self::new_with_balances(balances)
    }

    // === CONTROL METHODS (Test Knobs) ===

    pub async fn set_fail_mode(&self, mode: OrderFailMode) {
        self.inner.write().await.knobs.order_fail_mode = mode;
    }

    pub async fn set_exchange_health(&self, health: bot_core::ExchangeHealth) {
        self.inner.write().await.knobs.exchange_health = health;
    }

    pub async fn set_should_timeout(&self, should_timeout: bool) {
        self.inner.write().await.knobs.should_timeout = should_timeout;
    }

    /// Enable immediate fill mode for all orders (for backtesting)
    pub async fn set_fill_all_immediately(&self, fill: bool) {
        self.inner.write().await.knobs.fill_all_immediately = fill;
    }

    pub async fn set_mid(&self, coin: &str, price: Decimal) {
        self.inner
            .write()
            .await
            .current_mids
            .insert(coin.to_string(), price);
    }

    pub async fn set_time(&self, time_ms: i64) {
        self.inner.write().await.time_ms = time_ms;
    }

    pub async fn set_balance(&self, asset: AssetId, amount: Decimal) {
        self.inner.write().await.balances.insert(asset, amount);
    }

    // === VERIFICATION METHODS ===

    pub async fn placed_orders(&self) -> Vec<OrderInput> {
        self.inner.read().await.placed_orders.clone()
    }

    pub async fn balance(&self, asset: &AssetId) -> Decimal {
        self.inner
            .read()
            .await
            .balances
            .get(asset)
            .copied()
            .unwrap_or_default()
    }

    pub async fn fills(&self) -> Vec<Fill> {
        self.inner.read().await.fills.clone()
    }

    // === BACKTESTING METHODS ===

    /// Queue historical quotes for backtesting. These will be returned
    /// by poll_quotes() in FIFO order, one per call.
    pub async fn queue_quotes(&self, quotes: Vec<Quote>) {
        let mut state = self.inner.write().await;
        state.quote_queue.extend(quotes);
    }

    /// Check if there are queued quotes remaining
    pub async fn has_queued_quotes(&self) -> bool {
        !self.inner.read().await.quote_queue.is_empty()
    }
}

impl Default for MockExchange {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Exchange for MockExchange {
    fn exchange_id(&self) -> &ExchangeId {
        &self.exchange_id
    }

    fn environment(&self) -> Environment {
        self.environment
    }

    async fn place_orders(
        &self,
        orders: &[OrderInput],
    ) -> Result<Vec<PlaceOrderResult>, ExchangeError> {
        let mut state = self.inner.write().await;

        // Record for verification
        state.placed_orders.extend_from_slice(orders);

        // Check behavioral knobs
        if state.knobs.exchange_health != bot_core::ExchangeHealth::Active {
            return Err(ExchangeError::Unavailable);
        }

        if state.knobs.should_timeout {
            return Err(ExchangeError::Network("Timeout".into()));
        }

        if state.knobs.should_rate_limit {
            return Err(ExchangeError::RateLimited);
        }

        let mut results = Vec::new();

        for order in orders {
            match state.knobs.order_fail_mode {
                OrderFailMode::AlwaysSucceed => {
                    let oid = state.allocate_oid();

                    // For IOC orders or fill_all_immediately mode, execute immediately
                    let (filled_qty, avg_fill_px) =
                        if order.tif == TimeInForce::Ioc || state.knobs.fill_all_immediately {
                            let fill = state.execute_immediate_fill(order, oid);
                            let qty = fill.qty;
                            let px = fill.price;
                            state.fills.push(fill);
                            (Some(qty), Some(px))
                        } else {
                            (None, None)
                        };

                    results.push(PlaceOrderResult::Accepted {
                        exchange_order_id: Some(ExchangeOrderId::new(oid.to_string())),
                        filled_qty,
                        avg_fill_px,
                    });
                }

                OrderFailMode::FailOnInsufficientBalance => {
                    // Check balance first
                    if let Err(reason) = state.check_balance(order) {
                        results.push(PlaceOrderResult::Rejected { reason });
                        continue;
                    }

                    // Otherwise succeed
                    let oid = state.allocate_oid();

                    let (filled_qty, avg_fill_px) = if order.tif == TimeInForce::Ioc {
                        let fill = state.execute_immediate_fill(order, oid);
                        let qty = fill.qty;
                        let px = fill.price;
                        state.fills.push(fill);
                        (Some(qty), Some(px))
                    } else {
                        (None, None)
                    };

                    results.push(PlaceOrderResult::Accepted {
                        exchange_order_id: Some(ExchangeOrderId::new(oid.to_string())),
                        filled_qty,
                        avg_fill_px,
                    });
                }

                OrderFailMode::IocAlwaysReject => {
                    if order.tif == TimeInForce::Ioc {
                        results.push(PlaceOrderResult::Rejected {
                            reason: "IOC order rejected: no immediate liquidity".into(),
                        });
                    } else {
                        let oid = state.allocate_oid();
                        results.push(PlaceOrderResult::Accepted {
                            exchange_order_id: Some(ExchangeOrderId::new(oid.to_string())),
                            filled_qty: None,
                            avg_fill_px: None,
                        });
                    }
                }
            }
        }

        Ok(results)
    }

    async fn cancel_order(
        &self,
        _instrument: &InstrumentId,
        _market_index: &bot_core::MarketIndex,
        _client_id: &ClientOrderId,
        _exchange_order_id: Option<&ExchangeOrderId>,
    ) -> Result<(), ExchangeError> {
        // Mock: always succeeds
        Ok(())
    }

    async fn cancel_all_orders(
        &self,
        _instrument: &InstrumentId,
        _market_index: &bot_core::MarketIndex,
    ) -> Result<u32, ExchangeError> {
        // Mock: no orders to cancel
        Ok(0)
    }

    async fn poll_user_fills(&self, _cursor: Option<&str>) -> Result<Vec<Fill>, ExchangeError> {
        let state = self.inner.read().await;
        Ok(state.fills.clone())
    }

    async fn poll_quotes(&self, instruments: &[InstrumentId]) -> Result<Vec<Quote>, ExchangeError> {
        let mut state = self.inner.write().await;

        // If we have queued quotes (backtesting mode), pop one from the queue
        if !state.quote_queue.is_empty() {
            if let Some(quote) = state.quote_queue.pop_front() {
                // Update time to match the quote timestamp
                state.time_ms = quote.ts;
                return Ok(vec![quote]);
            }
        }

        // Otherwise, generate quotes from current_mids (real-time mode)
        let mut quotes = Vec::new();
        for instrument in instruments {
            // Extract coin from instrument (e.g., "ETH-PERP" -> "ETH")
            let instrument_str = instrument.to_string();
            let coin = if let Some(pos) = instrument_str.rfind('-') {
                &instrument_str[..pos]
            } else {
                &instrument_str
            };

            if let Some(mid) = state.current_mids.get(coin) {
                // Simulate bid/ask spread (5 bps)
                let spread_bps = Decimal::new(5, 4); // 0.0005
                let bid = *mid * (Decimal::ONE - spread_bps);
                let ask = *mid * (Decimal::ONE + spread_bps);

                quotes.push(Quote {
                    instrument: instrument.clone(),
                    bid: Price::new(bid),
                    ask: Price::new(ask),
                    bid_size: Qty::new(Decimal::new(100, 0)),
                    ask_size: Qty::new(Decimal::new(100, 0)),
                    ts: state.time_ms,
                });
            }
        }

        Ok(quotes)
    }

    async fn poll_account_state(&self) -> Result<AccountState, ExchangeError> {
        let state = self.inner.read().await;

        let positions: Vec<PositionSnapshot> = state
            .positions
            .iter()
            .map(|(instrument, pos)| PositionSnapshot {
                instrument: instrument.clone(),
                qty: pos.qty,
                avg_entry_px: Some(pos.avg_entry_px),
                unrealized_pnl: Some(pos.unrealized_pnl),
            })
            .collect();

        Ok(AccountState {
            positions,
            account_value: Some(state.account_value),
            unrealized_pnl: Some(Decimal::ZERO),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_exchange_creation() {
        let mock = MockExchange::new();

        // Verify initial balances
        let usdc = mock.balance(&AssetId::new("USDC")).await;
        assert_eq!(usdc, Decimal::new(100000, 0));

        let eth = mock.balance(&AssetId::new("ETH")).await;
        assert_eq!(eth, Decimal::new(10, 0));
    }

    #[tokio::test]
    async fn test_place_order_always_succeed() {
        let mock = MockExchange::new();
        mock.set_mid("ETH", Decimal::new(3000, 0)).await;

        let order = OrderInput {
            instrument: InstrumentId::new("ETH-SPOT"),
            market_index: bot_core::MarketIndex::new(0),
            side: OrderSide::Buy,
            price: Price::new(Decimal::new(3000, 0)),
            qty: Qty::new(Decimal::new(1, 1)), // 0.1 ETH
            client_id: ClientOrderId::generate(),
            tif: TimeInForce::Ioc,
            post_only: false,
            reduce_only: false,
        };

        let results = mock.place_orders(&[order]).await.unwrap();
        assert_eq!(results.len(), 1);

        match &results[0] {
            PlaceOrderResult::Accepted { .. } => {}
            PlaceOrderResult::Rejected { reason } => {
                panic!("Order rejected: {}", reason);
            }
        }

        // Verify balance updated
        let usdc = mock.balance(&AssetId::new("USDC")).await;
        assert_eq!(usdc, Decimal::new(99700, 0)); // 100000 - 300

        let eth = mock.balance(&AssetId::new("ETH")).await;
        assert_eq!(eth, Decimal::new(101, 1)); // 10 + 0.1
    }

    #[tokio::test]
    async fn test_insufficient_balance() {
        let mock = MockExchange::new();
        mock.set_fail_mode(OrderFailMode::FailOnInsufficientBalance)
            .await;
        mock.set_balance(AssetId::new("USDC"), Decimal::new(10, 0))
            .await; // Only $10

        let order = OrderInput {
            instrument: InstrumentId::new("ETH-SPOT"),
            market_index: bot_core::MarketIndex::new(0),
            side: OrderSide::Buy,
            price: Price::new(Decimal::new(3000, 0)),
            qty: Qty::new(Decimal::new(1, 1)),
            client_id: ClientOrderId::generate(),
            tif: TimeInForce::Ioc,
            post_only: false,
            reduce_only: false,
        };

        let results = mock.place_orders(&[order]).await.unwrap();

        match &results[0] {
            PlaceOrderResult::Rejected { reason } => {
                assert!(reason.contains("Insufficient balance"));
            }
            PlaceOrderResult::Accepted { .. } => {
                panic!("Order should have been rejected");
            }
        }
    }
}
