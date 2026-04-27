//! Paper Exchange: Simulates order execution using real quotes.
//!
//! Wraps a real exchange for quote data but simulates fills locally.
//! Orders fill when the quote price crosses the order price.
//! Uses FillSimulator for shared fill matching logic.
//! Uses MarginLedger for accurate perp margin accounting.

use super::fill_simulator::{FillSimulator, PendingOrder};
use crate::simulation::MarginLedger;
use async_lock::RwLock;
use bot_core::{
    AccountState, AssetId, ClientOrderId, Exchange, ExchangeError, ExchangeId, ExchangeOrderId,
    Fill, InstrumentId, MarketIndex, OrderInput, PlaceOrderResult, Qty, Quote, TimeInForce,
};
use rust_decimal::Decimal;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

/// Newtype wrapper around Arc<dyn Exchange> to implement Exchange trait.
/// This allows PaperExchange to wrap any Arc<dyn Exchange> while satisfying orphan rules.
pub struct ArcExchange(pub Arc<dyn Exchange>);

impl ArcExchange {
    pub fn new(exchange: Arc<dyn Exchange>) -> Self {
        Self(exchange)
    }
}

#[async_trait::async_trait]
impl Exchange for ArcExchange {
    fn exchange_id(&self) -> &ExchangeId {
        self.0.exchange_id()
    }

    fn environment(&self) -> bot_core::Environment {
        self.0.environment()
    }

    async fn place_orders(
        &self,
        orders: &[OrderInput],
    ) -> Result<Vec<PlaceOrderResult>, ExchangeError> {
        self.0.place_orders(orders).await
    }

    async fn cancel_order(
        &self,
        instrument: &InstrumentId,
        market_index: &MarketIndex,
        client_id: &ClientOrderId,
        exchange_order_id: Option<&ExchangeOrderId>,
    ) -> Result<(), ExchangeError> {
        self.0
            .cancel_order(instrument, market_index, client_id, exchange_order_id)
            .await
    }

    async fn cancel_all_orders(
        &self,
        instrument: &InstrumentId,
        market_index: &MarketIndex,
    ) -> Result<u32, ExchangeError> {
        self.0.cancel_all_orders(instrument, market_index).await
    }

    async fn poll_user_fills(&self, cursor: Option<&str>) -> Result<Vec<Fill>, ExchangeError> {
        self.0.poll_user_fills(cursor).await
    }

    async fn poll_quotes(&self, instruments: &[InstrumentId]) -> Result<Vec<Quote>, ExchangeError> {
        self.0.poll_quotes(instruments).await
    }

    async fn poll_account_state(&self) -> Result<AccountState, ExchangeError> {
        self.0.poll_account_state().await
    }
}

// ============================================================================
// NoOpExchange - Minimal placeholder for standalone PaperExchange
// ============================================================================

/// Minimal exchange that does nothing - used internally by PaperExchange::new_standalone()
/// All quotes come from queue_quotes/inject_quote, not from this exchange.
pub struct NoOpExchange {
    exchange_id: ExchangeId,
    environment: bot_core::Environment,
}

impl NoOpExchange {
    pub fn new() -> Self {
        Self::with_config("paper-exchange", bot_core::Environment::Testnet)
    }

    /// Create with a custom exchange ID (for backtest mode matching production exchange)
    pub fn with_exchange_id(id: &str) -> Self {
        Self::with_config(id, bot_core::Environment::Testnet)
    }

    /// Create with custom exchange ID and environment (for full compatibility)
    pub fn with_config(id: &str, environment: bot_core::Environment) -> Self {
        Self {
            exchange_id: ExchangeId::new(id),
            environment,
        }
    }
}

impl Default for NoOpExchange {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Exchange for NoOpExchange {
    fn exchange_id(&self) -> &ExchangeId {
        &self.exchange_id
    }

    fn environment(&self) -> bot_core::Environment {
        self.environment
    }

    async fn place_orders(
        &self,
        _orders: &[OrderInput],
    ) -> Result<Vec<PlaceOrderResult>, ExchangeError> {
        // PaperExchange handles orders itself, this is never called
        Ok(vec![])
    }

    async fn cancel_order(
        &self,
        _instrument: &InstrumentId,
        _market_index: &MarketIndex,
        _client_id: &ClientOrderId,
        _exchange_order_id: Option<&ExchangeOrderId>,
    ) -> Result<(), ExchangeError> {
        Ok(())
    }

    async fn cancel_all_orders(
        &self,
        _instrument: &InstrumentId,
        _market_index: &MarketIndex,
    ) -> Result<u32, ExchangeError> {
        Ok(0)
    }

    async fn poll_user_fills(&self, _cursor: Option<&str>) -> Result<Vec<Fill>, ExchangeError> {
        Ok(vec![])
    }

    async fn poll_quotes(
        &self,
        _instruments: &[InstrumentId],
    ) -> Result<Vec<Quote>, ExchangeError> {
        // PaperExchange uses queue_quotes/inject_quote, not this
        Ok(vec![])
    }

    async fn poll_account_state(&self) -> Result<AccountState, ExchangeError> {
        Ok(AccountState {
            positions: vec![],
            account_value: None,
            unrealized_pnl: None,
        })
    }
}

/// Internal state for the paper exchange
struct PaperState {
    /// Fill simulator handles pending orders, balances, and fill matching
    simulator: FillSimulator,
    /// Margin ledger for accurate perp accounting (positions, margin, equity)
    margin_ledger: MarginLedger,
    /// Simulated fills waiting to be returned
    simulated_fills: Vec<Fill>,
    /// Current time (for fill timestamps)
    time_ms: i64,
    /// Last known quotes (for fill matching)
    last_quotes: HashMap<InstrumentId, Quote>,
    /// Injected quotes for simulation testing (bypasses real exchange)
    injected_quotes: HashMap<InstrumentId, Quote>,
    /// Whether to use injected quotes instead of real ones
    use_injected_quotes: bool,
    /// Queued quotes for batch backtesting (FIFO)
    quote_queue: VecDeque<Quote>,
}

/// Type alias for standalone PaperExchange (no inner exchange dependency)
pub type StandalonePaperExchange = PaperExchange<NoOpExchange>;

/// Create a standalone paper exchange for backtesting without any inner exchange.
/// Uses `queue_quotes()` or `inject_quote()` for price data.
///
/// # Example
/// ```ignore
/// let exchange = create_standalone_paper_exchange(balances);
/// exchange.queue_quotes(historical_quotes).await;
/// // Use exchange for backtesting
/// ```
pub fn create_standalone_paper_exchange(
    initial_balances: HashMap<AssetId, Decimal>,
) -> PaperExchange<NoOpExchange> {
    create_standalone_paper_exchange_with_id(
        initial_balances,
        "paper-exchange",
        bot_core::Environment::Testnet,
    )
}

/// Create a standalone paper exchange with a custom exchange ID and environment.
/// Use exchange_id = "hyperliquid" and environment = Mainnet for backtest mode compatibility.
pub fn create_standalone_paper_exchange_with_id(
    initial_balances: HashMap<AssetId, Decimal>,
    exchange_id: &str,
    environment: bot_core::Environment,
) -> PaperExchange<NoOpExchange> {
    PaperExchange::new(
        NoOpExchange::with_config(exchange_id, environment),
        initial_balances,
    )
}

/// Paper Exchange - wraps a real exchange for quotes, simulates fills locally.
///
/// Uses `FillSimulator` for shared fill matching logic.
///
/// # Usage
/// ```ignore
/// let real_exchange = HyperliquidExchange::new(config);
/// let paper = PaperExchange::new(real_exchange, initial_balances);
/// // Use paper as the exchange - strategies see no difference
/// ```
pub struct PaperExchange<E: Exchange> {
    /// The real exchange for quote data
    quote_source: E,
    /// Internal state
    state: Arc<RwLock<PaperState>>,
}

impl<E: Exchange> PaperExchange<E> {
    /// Create a new paper exchange wrapping a real quote source
    pub fn new(quote_source: E, initial_balances: HashMap<AssetId, Decimal>) -> Self {
        // Extract starting USDC for the margin ledger
        let starting_usdc = initial_balances
            .get(&AssetId::new("USDC"))
            .copied()
            .unwrap_or(Decimal::ZERO);

        Self {
            quote_source,
            state: Arc::new(RwLock::new(PaperState {
                simulator: FillSimulator::new(initial_balances),
                margin_ledger: MarginLedger::new(starting_usdc, Decimal::ZERO),
                simulated_fills: Vec::new(),
                time_ms: bot_core::now_ms(),
                last_quotes: HashMap::new(),
                injected_quotes: HashMap::new(),
                use_injected_quotes: false,
                quote_queue: VecDeque::new(),
            })),
        }
    }

    /// Set a balance directly (for testing)
    pub async fn set_balance(&self, asset: AssetId, amount: Decimal) {
        self.state
            .write()
            .await
            .simulator
            .set_balance(asset, amount);
    }

    /// Get current balance
    pub async fn balance(&self, asset: &AssetId) -> Decimal {
        self.state.read().await.simulator.balance(asset)
    }

    /// Get pending orders count
    pub async fn pending_orders_count(&self) -> usize {
        self.state.read().await.simulator.pending_orders_count()
    }

    /// Set fee rate for fill simulation (e.g., 0.0004 = 0.04%)
    /// For spot BUY orders, fee is deducted from base asset
    pub async fn set_fee_rate(&self, fee_rate: Decimal) {
        let mut state = self.state.write().await;
        state.simulator.set_fee_rate(fee_rate);
        state.margin_ledger.set_fee_rate(fee_rate);
    }

    /// Get current balances (for testing)
    pub async fn get_balances(&self) -> HashMap<AssetId, Decimal> {
        let state = self.state.read().await;
        let mut balances = HashMap::new();
        // Get USDC balance
        balances.insert(
            AssetId::new("USDC"),
            state.simulator.balance(&AssetId::new("USDC")),
        );
        balances.insert(
            AssetId::new("BTC"),
            state.simulator.balance(&AssetId::new("BTC")),
        );
        balances.insert(
            AssetId::new("ETH"),
            state.simulator.balance(&AssetId::new("ETH")),
        );
        balances
    }

    /// Get position for an instrument (for testing)
    /// Returns the net position qty (positive = long, negative = short)
    pub async fn get_position(&self, instrument: &InstrumentId) -> Decimal {
        let instrument_str = instrument.to_string();
        let state = self.state.read().await;

        // For PERP instruments, use margin ledger
        if instrument_str.ends_with("-PERP") {
            return state.margin_ledger.position_qty(instrument);
        }

        // For SPOT, position is the base asset balance
        let base_asset = if let Some(pos) = instrument_str.rfind('-') {
            AssetId::new(&instrument_str[..pos])
        } else {
            AssetId::new(&instrument_str)
        };
        state.simulator.balance(&base_asset)
    }

    /// Set leverage for an instrument in the margin ledger
    pub async fn set_instrument_leverage(
        &self,
        instrument: &InstrumentId,
        leverage: Decimal,
        max_leverage: Decimal,
    ) {
        self.state
            .write()
            .await
            .margin_ledger
            .set_leverage(instrument, leverage, max_leverage);
    }

    /// Get the margin ledger's free USDC (for testing)
    pub async fn free_usdc(&self) -> Decimal {
        self.state.read().await.margin_ledger.free_usdc()
    }

    // =========================================================================
    // Simulation Testing Controls
    // =========================================================================

    /// Enable simulation mode - quotes will come from injected_quotes instead of real exchange.
    /// This allows deterministic testing where you control the price feed.
    pub async fn enable_simulation_mode(&self) {
        self.state.write().await.use_injected_quotes = true;
    }

    /// Disable simulation mode - return to using real exchange quotes.
    pub async fn disable_simulation_mode(&self) {
        self.state.write().await.use_injected_quotes = false;
    }

    /// Check if simulation mode is enabled.
    pub async fn is_simulation_mode(&self) -> bool {
        self.state.read().await.use_injected_quotes
    }

    /// Inject a quote for simulation testing.
    /// When simulation mode is enabled, poll_quotes will return these instead of real quotes.
    pub async fn inject_quote(&self, instrument: InstrumentId, bid: Decimal, ask: Decimal) {
        let mut state = self.state.write().await;
        let quote = Quote {
            instrument: instrument.clone(),
            bid: bot_core::Price::new(bid),
            ask: bot_core::Price::new(ask),
            bid_size: Qty::new(Decimal::new(1000, 0)),
            ask_size: Qty::new(Decimal::new(1000, 0)),
            ts: state.time_ms,
        };
        state.injected_quotes.insert(instrument, quote);
    }

    // =========================================================================
    // Batch Backtesting Controls
    // =========================================================================

    /// Queue multiple quotes for batch backtesting (FIFO).
    /// When quotes are queued, poll_quotes will pop from this queue first.
    /// This enables fast-forward backtesting with realistic price-crossing fills.
    pub async fn queue_quotes(&self, quotes: Vec<Quote>) {
        let mut state = self.state.write().await;
        state.quote_queue.extend(quotes);
    }

    /// Check if there are still queued quotes remaining.
    /// Use this to detect when backtesting is complete.
    pub async fn has_queued_quotes(&self) -> bool {
        !self.state.read().await.quote_queue.is_empty()
    }

    /// Get the number of queued quotes remaining.
    pub async fn queue_len(&self) -> usize {
        self.state.read().await.quote_queue.len()
    }

    /// Clear all queued quotes.
    pub async fn clear_quote_queue(&self) {
        self.state.write().await.quote_queue.clear();
    }

    /// Set simulated time (for deterministic testing).
    pub async fn set_time(&self, time_ms: i64) {
        self.state.write().await.time_ms = time_ms;
    }

    /// Advance simulated time by delta milliseconds.
    pub async fn advance_time(&self, delta_ms: i64) {
        self.state.write().await.time_ms += delta_ms;
    }

    /// Get current simulated time.
    pub async fn current_time(&self) -> i64 {
        self.state.read().await.time_ms
    }

    /// Get all simulated fills (for testing).
    pub async fn fills(&self) -> Vec<Fill> {
        self.state.read().await.simulated_fills.clone()
    }

    /// Check if any pending orders can be filled based on current quotes
    async fn check_fills(&self) {
        let mut state = self.state.write().await;
        // Clone to avoid borrow conflict
        let quotes = state.last_quotes.clone();
        let time_ms = state.time_ms;
        let simulated = state.simulator.check_fills(&quotes, time_ms);

        for sim_fill in simulated {
            // Apply margin accounting for PERP fills
            let inst_str = sim_fill.fill.instrument.to_string();
            if inst_str.ends_with("-PERP") {
                state.margin_ledger.apply_perp_fill(
                    &sim_fill.fill.instrument,
                    sim_fill.fill.side,
                    sim_fill.fill.price.0,
                    sim_fill.fill.qty.0,
                    sim_fill.fill.fee.amount,
                );
            }
            state.simulated_fills.push(sim_fill.fill);
        }
    }

    /// Check if balance is sufficient for order.
    /// For PERP instruments: uses margin-aware check (notional / leverage).
    /// For SPOT: selling requires the base asset, buying requires USDC.
    fn check_balance_for_order(
        simulator: &FillSimulator,
        margin_ledger: &MarginLedger,
        order: &OrderInput,
    ) -> Result<(), String> {
        let instrument_str = order.instrument.to_string();
        let is_perp = instrument_str.ends_with("-PERP");

        if is_perp {
            // PERP: delegate to margin-aware check
            margin_ledger.check_margin_for_perp_order(
                &order.instrument,
                order.side,
                order.price.0,
                order.qty.0,
                order.reduce_only,
            )
        } else {
            // SPOT: use original balance check logic
            let quote_asset = AssetId::new("USDC");

            if order.side == bot_core::OrderSide::Buy {
                let required = order.price.0 * order.qty.0;
                let available = simulator.balance(&quote_asset);
                if required > available {
                    return Err(format!(
                        "Insufficient balance: need {} USDC, have {}",
                        required, available
                    ));
                }
            } else {
                let base_asset = if let Some(pos) = instrument_str.rfind('-') {
                    AssetId::new(&instrument_str[..pos])
                } else {
                    AssetId::new(&instrument_str)
                };
                let available = simulator.balance(&base_asset);
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
}

#[async_trait::async_trait]
impl<E: Exchange + Send + Sync> Exchange for PaperExchange<E> {
    fn exchange_id(&self) -> &ExchangeId {
        self.quote_source.exchange_id()
    }

    fn environment(&self) -> bot_core::Environment {
        self.quote_source.environment()
    }

    async fn place_orders(
        &self,
        orders: &[OrderInput],
    ) -> Result<Vec<PlaceOrderResult>, ExchangeError> {
        #[cfg(feature = "wasm")]
        web_sys::console::log_1(
            &format!(
                "[WASM Runner] place_orders called with {} orders",
                orders.len()
            )
            .into(),
        );
        tracing::debug!(
            "[WASM Runner] place_orders called with {} orders",
            orders.len()
        );
        let mut state = self.state.write().await;
        let mut results = Vec::new();

        for order in orders {
            #[cfg(feature = "wasm")]
            web_sys::console::log_1(
                &format!(
                    "[WASM Runner] Processing order: {:?} {} {} @ {} (instrument={})",
                    order.client_id, order.side, order.qty, order.price, order.instrument
                )
                .into(),
            );
            tracing::debug!(
                "[WASM Runner] Processing order: {:?} {} {} @ {}",
                order.client_id,
                order.side,
                order.qty,
                order.price
            );

            // Check balance using margin-aware logic
            if let Err(reason) =
                Self::check_balance_for_order(&state.simulator, &state.margin_ledger, order)
            {
                #[cfg(feature = "wasm")]
                web_sys::console::log_1(
                    &format!("[WASM Runner] Order REJECTED: {}", reason).into(),
                );
                tracing::debug!("[WASM Runner] Order rejected: {}", reason);
                results.push(PlaceOrderResult::Rejected { reason });
                continue;
            }

            let exchange_order_id = state.simulator.next_exchange_order_id("paper");

            // For IOC orders, check if we can fill immediately
            if order.tif == TimeInForce::Ioc {
                if let Some(quote) = state.last_quotes.get(&order.instrument) {
                    let can_fill = match order.side {
                        bot_core::OrderSide::Buy => quote.ask.0 <= order.price.0,
                        bot_core::OrderSide::Sell => quote.bid.0 >= order.price.0,
                    };

                    if can_fill {
                        let fill_price = match order.side {
                            bot_core::OrderSide::Buy => quote.ask,
                            bot_core::OrderSide::Sell => quote.bid,
                        };

                        let fill = Fill {
                            trade_id: bot_core::TradeId::new(format!(
                                "paper_{}",
                                exchange_order_id.0
                            )),
                            client_id: Some(order.client_id.clone()),
                            exchange_order_id: Some(exchange_order_id.clone()),
                            instrument: order.instrument.clone(),
                            side: order.side,
                            price: fill_price,
                            qty: order.qty.clone(),
                            fee: bot_core::Fee::new(Decimal::ZERO, AssetId::new("USDC")),
                            ts: state.time_ms,
                        };

                        // Apply fill to simulator's balances directly
                        let quote_asset = AssetId::new("USDC");
                        let instrument_str = fill.instrument.to_string();
                        let base_asset = if let Some(pos) = instrument_str.rfind('-') {
                            AssetId::new(&instrument_str[..pos])
                        } else {
                            AssetId::new(&instrument_str)
                        };
                        let notional = fill.price.0 * fill.qty.0;

                        let is_perp = instrument_str.ends_with("-PERP");

                        if is_perp {
                            // PERP: use margin ledger for proper accounting
                            state.margin_ledger.apply_perp_fill(
                                &fill.instrument,
                                fill.side,
                                fill.price.0,
                                fill.qty.0,
                                fill.fee.amount,
                            );
                        } else {
                            // SPOT: use original balance update logic
                            match fill.side {
                                bot_core::OrderSide::Buy => {
                                    let bal = state.simulator.balance(&quote_asset) - notional;
                                    state.simulator.set_balance(quote_asset, bal);
                                    let bal = state.simulator.balance(&base_asset) + fill.qty.0;
                                    state.simulator.set_balance(base_asset, bal);
                                }
                                bot_core::OrderSide::Sell => {
                                    let bal = state.simulator.balance(&quote_asset) + notional;
                                    state.simulator.set_balance(quote_asset, bal);
                                    let bal = state.simulator.balance(&base_asset) - fill.qty.0;
                                    state.simulator.set_balance(base_asset, bal);
                                }
                            }
                        }
                        state.simulated_fills.push(fill.clone());

                        results.push(PlaceOrderResult::Accepted {
                            exchange_order_id: Some(exchange_order_id),
                            filled_qty: Some(fill.qty),
                            avg_fill_px: Some(fill.price),
                        });
                    } else {
                        // IOC cannot fill immediately
                        results.push(PlaceOrderResult::Rejected {
                            reason: "IOC order cannot fill at current price".into(),
                        });
                    }
                } else {
                    // No quote available
                    results.push(PlaceOrderResult::Rejected {
                        reason: "No quote available for instrument".into(),
                    });
                }
            } else {
                // Non-IOC orders go to pending via FillSimulator
                let created_at = state.time_ms; // Copy before mutable borrow
                #[cfg(feature = "wasm")]
                web_sys::console::log_1(
                    &format!(
                        "[WASM Runner] Adding to simulator pending: {} {} @ {} (TIF={:?})",
                        order.side, order.qty, order.price, order.tif
                    )
                    .into(),
                );
                state.simulator.add_pending_order(PendingOrder {
                    client_id: order.client_id.clone(),
                    exchange_order_id: exchange_order_id.clone(),
                    instrument: order.instrument.clone(),
                    side: order.side,
                    price: order.price.clone(),
                    qty: order.qty.clone(),
                    remaining_qty: order.qty.clone(),
                    created_at,
                });
                #[cfg(feature = "wasm")]
                web_sys::console::log_1(
                    &format!(
                        "[WASM Runner] After add: pending_count={}",
                        state.simulator.pending_orders_count()
                    )
                    .into(),
                );

                results.push(PlaceOrderResult::Accepted {
                    exchange_order_id: Some(exchange_order_id),
                    filled_qty: None,
                    avg_fill_px: None,
                });
            }
        }

        Ok(results)
    }

    async fn cancel_order(
        &self,
        _instrument: &InstrumentId,
        _market_index: &MarketIndex,
        client_id: &ClientOrderId,
        _exchange_order_id: Option<&ExchangeOrderId>,
    ) -> Result<(), ExchangeError> {
        #[cfg(feature = "wasm")]
        web_sys::console::log_1(
            &format!("[WASM Runner] cancel_order called: {:?}", client_id).into(),
        );
        let mut state = self.state.write().await;
        state.simulator.remove_order(client_id);
        Ok(())
    }

    async fn cancel_all_orders(
        &self,
        instrument: &InstrumentId,
        _market_index: &MarketIndex,
    ) -> Result<u32, ExchangeError> {
        #[cfg(feature = "wasm")]
        web_sys::console::log_1(
            &format!(
                "[WASM Runner] cancel_all_orders called: instrument={}",
                instrument
            )
            .into(),
        );
        let mut state = self.state.write().await;
        let removed = state.simulator.remove_orders_for_instrument(instrument);
        #[cfg(feature = "wasm")]
        web_sys::console::log_1(
            &format!(
                "[WASM Runner] cancel_all_orders removed {} orders",
                removed.len()
            )
            .into(),
        );
        Ok(removed.len() as u32)
    }

    async fn poll_user_fills(&self, _cursor: Option<&str>) -> Result<Vec<Fill>, ExchangeError> {
        // Check if any pending orders can now fill
        self.check_fills().await;

        // Drain simulated fills
        let mut state = self.state.write().await;
        let fills = std::mem::take(&mut state.simulated_fills);
        Ok(fills)
    }

    async fn poll_quotes(&self, instruments: &[InstrumentId]) -> Result<Vec<Quote>, ExchangeError> {
        let mut state = self.state.write().await;

        // Quote source priority:
        // 1. quote_queue (batch backtest mode) - pop next quote
        // 2. injected_quotes (simulation mode) - use latest injected
        // 3. real exchange (live/paper trading) - fetch from underlying
        let quotes = if !state.quote_queue.is_empty() {
            // Batch backtest mode: pop next quote from queue
            if let Some(quote) = state.quote_queue.pop_front() {
                // Update time to match the quote timestamp
                state.time_ms = quote.ts;
                vec![quote]
            } else {
                vec![]
            }
        } else if state.use_injected_quotes {
            // Simulation mode: use injected quotes
            instruments
                .iter()
                .filter_map(|inst| state.injected_quotes.get(inst).cloned())
                .collect()
        } else {
            // Normal mode: get real quotes from the underlying exchange
            // Note: we need to drop the lock before the await
            drop(state);
            let quotes = self.quote_source.poll_quotes(instruments).await?;
            state = self.state.write().await;
            state.time_ms = bot_core::now_ms();
            quotes
        };

        // Store for fill matching
        for quote in &quotes {
            state
                .last_quotes
                .insert(quote.instrument.clone(), quote.clone());
        }

        // Check fills immediately after updating quotes
        // This is important because runner calls poll_user_fills BEFORE poll_quotes
        let quotes_copy = state.last_quotes.clone();
        let time_ms = state.time_ms;
        let simulated = state.simulator.check_fills(&quotes_copy, time_ms);
        for sim_fill in simulated {
            // Apply margin accounting for PERP fills
            let inst_str = sim_fill.fill.instrument.to_string();
            if inst_str.ends_with("-PERP") {
                state.margin_ledger.apply_perp_fill(
                    &sim_fill.fill.instrument,
                    sim_fill.fill.side,
                    sim_fill.fill.price.0,
                    sim_fill.fill.qty.0,
                    sim_fill.fill.fee.amount,
                );
            }
            state.simulated_fills.push(sim_fill.fill);
        }

        // Check for liquidations at current mark prices
        let marks: HashMap<InstrumentId, Decimal> = state
            .last_quotes
            .iter()
            .map(|(id, q)| (id.clone(), q.mid().0))
            .collect();
        let liquidated = state.margin_ledger.check_liquidations(&marks);
        for instrument in liquidated {
            if let Some(mark) = marks.get(&instrument) {
                state.margin_ledger.liquidate(&instrument, *mark);
            }
        }

        Ok(quotes)
    }

    async fn poll_account_state(&self) -> Result<AccountState, ExchangeError> {
        let state = self.state.read().await;

        // Build mark prices from last known quotes
        let marks: HashMap<InstrumentId, Decimal> = state
            .last_quotes
            .iter()
            .map(|(id, q)| (id.clone(), q.mid().0))
            .collect();

        // Return real equity including margin positions and unrealized PnL
        let equity = state.margin_ledger.equity(&marks);
        let unrealized = state.margin_ledger.total_unrealized_pnl(&marks);
        let positions = state.margin_ledger.position_snapshots(&marks);

        Ok(AccountState {
            positions,
            account_value: Some(equity),
            unrealized_pnl: Some(unrealized),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bot_core::{OrderSide, Price};

    // Create a minimal mock for testing PaperExchange
    struct MinimalMockExchange {
        exchange_id: ExchangeId,
        quotes: Arc<RwLock<Vec<Quote>>>,
    }

    impl MinimalMockExchange {
        fn new() -> Self {
            Self {
                exchange_id: ExchangeId::new("minimal-mock"),
                quotes: Arc::new(RwLock::new(Vec::new())),
            }
        }

        async fn set_quote(&self, quote: Quote) {
            self.quotes.write().await.push(quote);
        }
    }

    #[async_trait::async_trait]
    impl Exchange for MinimalMockExchange {
        fn exchange_id(&self) -> &ExchangeId {
            &self.exchange_id
        }

        fn environment(&self) -> bot_core::Environment {
            bot_core::Environment::Testnet
        }

        async fn place_orders(
            &self,
            _orders: &[OrderInput],
        ) -> Result<Vec<PlaceOrderResult>, ExchangeError> {
            unimplemented!("MinimalMockExchange doesn't support orders")
        }

        async fn cancel_order(
            &self,
            _instrument: &InstrumentId,
            _market_index: &MarketIndex,
            _client_id: &ClientOrderId,
            _exchange_order_id: Option<&ExchangeOrderId>,
        ) -> Result<(), ExchangeError> {
            Ok(())
        }

        async fn cancel_all_orders(
            &self,
            _instrument: &InstrumentId,
            _market_index: &MarketIndex,
        ) -> Result<u32, ExchangeError> {
            Ok(0)
        }

        async fn poll_user_fills(&self, _cursor: Option<&str>) -> Result<Vec<Fill>, ExchangeError> {
            Ok(vec![])
        }

        async fn poll_quotes(
            &self,
            _instruments: &[InstrumentId],
        ) -> Result<Vec<Quote>, ExchangeError> {
            let quotes = self.quotes.read().await;
            Ok(quotes.clone())
        }

        async fn poll_account_state(&self) -> Result<AccountState, ExchangeError> {
            Ok(AccountState {
                positions: vec![],
                account_value: None,
                unrealized_pnl: None,
            })
        }
    }

    #[tokio::test]
    async fn test_paper_exchange_uses_fill_simulator() {
        let mut balances = HashMap::new();
        balances.insert(AssetId::new("USDC"), Decimal::new(10000, 0));

        let mock = MinimalMockExchange::new();
        let paper = PaperExchange::new(mock, balances);

        // Check USDC balance
        let balance = paper.balance(&AssetId::new("USDC")).await;
        assert_eq!(balance, Decimal::new(10000, 0));
    }

    #[tokio::test]
    async fn test_paper_exchange_accepts_perp_short() {
        let mut balances = HashMap::new();
        balances.insert(AssetId::new("USDC"), Decimal::new(100000, 0)); // 100k margin

        let mock = MinimalMockExchange::new();

        // Set a quote first
        mock.set_quote(Quote {
            instrument: InstrumentId::new("BTC-PERP"),
            bid: Price::new(Decimal::new(50000, 0)),
            ask: Price::new(Decimal::new(50001, 0)),
            bid_size: Qty::new(Decimal::new(10, 0)),
            ask_size: Qty::new(Decimal::new(10, 0)),
            ts: 0,
        })
        .await;

        let paper = PaperExchange::new(mock, balances);

        // Poll quotes to populate last_quotes
        paper
            .poll_quotes(&[InstrumentId::new("BTC-PERP")])
            .await
            .unwrap();

        // Place a SELL (short) order on PERP - should work with only margin
        let orders = vec![OrderInput {
            client_id: ClientOrderId::new("0xtest12345678901234567890123456"),
            instrument: InstrumentId::new("BTC-PERP"),
            market_index: MarketIndex::new(0),
            side: OrderSide::Sell,
            price: Price::new(Decimal::new(51000, 0)),
            qty: Qty::new(Decimal::new(1, 0)),
            tif: bot_core::TimeInForce::Gtc,
            post_only: false,
            reduce_only: false,
        }];

        let results = paper.place_orders(&orders).await.unwrap();

        match &results[0] {
            PlaceOrderResult::Accepted { .. } => { /* expected */ }
            PlaceOrderResult::Rejected { reason } => {
                panic!(
                    "PERP short should be accepted with margin, got rejection: {}",
                    reason
                );
            }
        }
    }
}
