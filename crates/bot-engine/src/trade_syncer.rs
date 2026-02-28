//! Trade syncer: syncs fills to upstream API for PnL tracking.
//!
//! This module provides async HTTP syncing of fills to an external API
//! (like the AlgoBot API) for centralized PnL calculation and persistence.

use bot_core::{now_ms, Fill, InstrumentId};
use reqwest::Client;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::Duration;

// Re-use SyncError from account_syncer to avoid duplication
pub use crate::account_syncer::SyncError;

/// Configuration for trade syncing
#[derive(Debug, Clone)]
pub struct TradeSyncerConfig {
    /// Bot ID for upstream API
    pub bot_id: String,
    /// Upstream API base URL (e.g., "https://api.example.com/bot-api")
    pub upstream_url: String,
    /// Sync interval in milliseconds (default: 10000)
    pub sync_interval_ms: u64,
    /// HTTP timeout in seconds
    pub timeout_secs: u64,
    /// Maximum retries for failed syncs
    pub max_retries: u32,
    /// Initial retry delay in milliseconds
    pub retry_delay_ms: u64,
    /// Instruments to filter fills (only sync fills for these instruments)
    /// Empty = sync all fills (no filter)
    pub instruments: Vec<InstrumentId>,
}

impl Default for TradeSyncerConfig {
    fn default() -> Self {
        Self {
            bot_id: String::new(),
            upstream_url: String::new(),
            sync_interval_ms: 10_000,
            timeout_secs: 10,
            max_retries: 3,
            retry_delay_ms: 1000,
            instruments: Vec::new(),
        }
    }
}

/// Trade format for upstream API (matches botRoutes.py Trade schema)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamTrade {
    pub trade_id: String,
    pub client_order_id: String,
    pub venue_order_id: String,
    pub instrument_id: String,
    pub side: String,
    pub order_type: String,
    pub qty: String,
    pub price: String,
    pub quote_notional: String,
    pub fee: String,
    pub fee_currency: String,
    pub liquidity: String,
    pub ts_event: i64,
}

/// Request payload for sync API
#[derive(Debug, Clone, Serialize)]
pub struct SyncRequest {
    pub trades: Vec<UpstreamTrade>,
    pub ts: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_price: Option<String>,
    pub stop_bot: bool,
    pub stop_reason: String,
}

/// Response from sync API
#[derive(Debug, Clone, Deserialize)]
pub struct SyncResponse {
    pub synced: SyncedInfo,
    #[serde(default)]
    pub pnl: f64,
}

/// Info about last synced trade
#[derive(Debug, Clone, Deserialize)]
pub struct SyncedInfo {
    pub trade_id: String,
    pub ts: i64,
}

/// Response from sync API
#[derive(Debug, Clone)]
pub struct SyncResult {
    pub success: bool,
    pub pnl: Option<f64>,
    pub last_synced_trade_id: Option<String>,
    pub trades_synced: usize,
}

/// Trade syncer - handles syncing fills to upstream API
pub struct TradeSyncer {
    config: TradeSyncerConfig,
    client: Client,
    /// Trade IDs that have been successfully synced
    synced_trade_ids: HashSet<String>,
    /// Accumulated fills waiting to be synced
    pending_fills: Vec<Fill>,
    /// Last successful sync timestamp
    last_sync_ts: i64,
    /// Last sync PnL
    last_pnl: Option<f64>,
    /// Timestamp when the syncer was created (only sync fills after this time)
    start_timestamp: i64,
}

impl TradeSyncer {
    /// Create a new trade syncer with the given configuration
    pub fn new(config: TradeSyncerConfig) -> Result<Self, SyncError> {
        if config.bot_id.is_empty() {
            return Err(SyncError::Config("bot_id is required".to_string()));
        }
        if config.upstream_url.is_empty() {
            return Err(SyncError::Config("upstream_url is required".to_string()));
        }

        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| SyncError::Http(e.to_string()))?;

        let start_timestamp = now_ms();
        tracing::info!(
            "[TradeSyncer] Initialized with start_timestamp={} - only fills after this time will be synced",
            start_timestamp
        );

        Ok(Self {
            config,
            client,
            synced_trade_ids: HashSet::new(),
            pending_fills: Vec::new(),
            last_sync_ts: 0,
            last_pnl: None,
            start_timestamp,
        })
    }

    /// Add a fill to the pending queue (will be synced on next sync call)
    pub fn add_fill(&mut self, fill: Fill) {
        // Skip fills that happened before the syncer started (historical fills)
        if fill.ts < self.start_timestamp {
            tracing::debug!(
                "Skipping historical fill: {} (ts={} < start_ts={})",
                fill.trade_id,
                fill.ts,
                self.start_timestamp
            );
            return;
        }

        // Skip if already synced
        if self.synced_trade_ids.contains(&fill.trade_id.0) {
            tracing::debug!("Skipping already-synced fill: {}", fill.trade_id);
            return;
        }

        // Filter by instruments if configured
        if !self.config.instruments.is_empty()
            && !self.config.instruments.contains(&fill.instrument)
        {
            tracing::debug!(
                "Skipping fill for untracked instrument: {} (tracking: {:?})",
                fill.instrument,
                self.config.instruments
            );
            return;
        }

        tracing::info!(
            "[TradeSyncer] Adding fill to pending queue: {} (ts={})",
            fill.trade_id,
            fill.ts
        );
        self.pending_fills.push(fill);
    }

    /// Check if it's time to sync (based on interval)
    pub fn should_sync(&self) -> bool {
        let now = now_ms();
        now - self.last_sync_ts >= self.config.sync_interval_ms as i64
    }

    /// Get the last known PnL
    pub fn last_pnl(&self) -> Option<f64> {
        self.last_pnl
    }

    /// Get the number of pending fills
    pub fn pending_count(&self) -> usize {
        self.pending_fills.len()
    }

    /// Sync pending fills to upstream API
    ///
    /// Returns the PnL from the upstream API on success
    pub async fn sync(
        &mut self,
        current_price: Option<Decimal>,
        stop_bot: bool,
        stop_reason: &str,
    ) -> Result<SyncResult, SyncError> {
        let now = now_ms();

        // Convert pending fills to upstream format
        let trades: Vec<UpstreamTrade> = self
            .pending_fills
            .iter()
            .filter(|f| !self.synced_trade_ids.contains(&f.trade_id.0))
            .map(|f| self.fill_to_trade(f))
            .collect();

        let trades_count = trades.len();

        tracing::info!(
            "[TradeSyncer] Syncing {} trades to upstream (pending={}, synced={})",
            trades_count,
            self.pending_fills.len(),
            self.synced_trade_ids.len()
        );

        let request = SyncRequest {
            trades,
            ts: now / 1000, // seconds
            current_price: current_price.map(|p| p.to_string()),
            stop_bot,
            stop_reason: stop_reason.to_string(),
        };

        // Execute with retry
        let response = self.execute_with_retry(&request).await?;

        // Mark all trades as synced
        for fill in &self.pending_fills {
            self.synced_trade_ids.insert(fill.trade_id.0.clone());
        }

        // Clear pending fills
        self.pending_fills.clear();

        // Update state
        self.last_sync_ts = now;
        self.last_pnl = Some(response.pnl);

        tracing::info!(
            "[TradeSyncer] Sync successful: pnl={:.4}, last_trade_id={}",
            response.pnl,
            response.synced.trade_id
        );

        Ok(SyncResult {
            success: true,
            pnl: Some(response.pnl),
            last_synced_trade_id: Some(response.synced.trade_id),
            trades_synced: trades_count,
        })
    }

    /// Execute sync request with retry logic
    async fn execute_with_retry(&self, request: &SyncRequest) -> Result<SyncResponse, SyncError> {
        let url = format!(
            "{}/sync/{}",
            self.config.upstream_url.trim_end_matches('/'),
            self.config.bot_id
        );

        let mut last_error: Option<SyncError> = None;
        let mut delay_ms = self.config.retry_delay_ms;

        for attempt in 1..=self.config.max_retries {
            tracing::debug!(
                "[TradeSyncer] Sync attempt {}/{} to {}",
                attempt,
                self.config.max_retries,
                url
            );

            match self.execute_request(&url, request).await {
                Ok(response) => return Ok(response),
                Err(e) => {
                    tracing::warn!(
                        "[TradeSyncer] Sync attempt {}/{} failed: {}",
                        attempt,
                        self.config.max_retries,
                        e
                    );
                    last_error = Some(e);

                    if attempt < self.config.max_retries {
                        tracing::debug!("[TradeSyncer] Retrying in {}ms...", delay_ms);
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        delay_ms *= 2; // exponential backoff
                    }
                }
            }
        }

        Err(last_error.unwrap_or(SyncError::MaxRetries))
    }

    /// Execute a single sync request
    async fn execute_request(
        &self,
        url: &str,
        request: &SyncRequest,
    ) -> Result<SyncResponse, SyncError> {
        let response = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .json(request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(SyncError::Api {
                status: status.as_u16(),
                body,
            });
        }

        let sync_response: SyncResponse = response
            .json()
            .await
            .map_err(|e| SyncError::Parse(e.to_string()))?;

        Ok(sync_response)
    }

    /// Convert a Fill to upstream trade format
    fn fill_to_trade(&self, fill: &Fill) -> UpstreamTrade {
        // Determine side string
        let side = format!("{}", fill.side);

        // Calculate quote notional
        let quote_notional = fill.price.0 * fill.qty.0;

        UpstreamTrade {
            trade_id: fill.trade_id.0.clone(),
            client_order_id: fill
                .client_id
                .as_ref()
                .map(|c| c.0.clone())
                .unwrap_or_default(),
            venue_order_id: fill
                .exchange_order_id
                .as_ref()
                .map(|e| e.0.clone())
                .unwrap_or_default(),
            instrument_id: fill.instrument.0.clone(),
            side,
            order_type: "LIMIT".to_string(),
            qty: fill.qty.0.to_string(),
            price: fill.price.0.to_string(),
            quote_notional: quote_notional.to_string(),
            fee: fill.fee.amount.to_string(),
            fee_currency: fill.fee.asset.0.clone(),
            liquidity: "UNKNOWN".to_string(), // Fill doesn't have maker/taker info
            ts_event: fill.ts,
        }
    }

    /// Perform final sync on shutdown (with stop_bot=true)
    pub async fn shutdown_sync(
        &mut self,
        current_price: Option<Decimal>,
        stop_reason: &str,
    ) -> Result<SyncResult, SyncError> {
        tracing::info!(
            "[TradeSyncer] Performing shutdown sync with reason: {}, price: {:?}",
            stop_reason,
            current_price
        );
        self.sync(current_price, true, stop_reason).await
    }
}

// Implement TradeSync trait for drop-in substitution with MockTradeSyncer
#[async_trait::async_trait]
impl crate::sync_traits::TradeSync for TradeSyncer {
    fn add_fill(&mut self, fill: Fill) {
        TradeSyncer::add_fill(self, fill)
    }

    fn should_sync(&self) -> bool {
        TradeSyncer::should_sync(self)
    }

    fn pending_count(&self) -> usize {
        TradeSyncer::pending_count(self)
    }

    fn last_pnl(&self) -> Option<f64> {
        TradeSyncer::last_pnl(self)
    }

    async fn sync(
        &mut self,
        current_price: Option<Decimal>,
        stop_bot: bool,
        stop_reason: &str,
    ) -> Result<crate::sync_traits::TradeSyncResult, SyncError> {
        let result = TradeSyncer::sync(self, current_price, stop_bot, stop_reason).await?;
        Ok(crate::sync_traits::TradeSyncResult {
            success: result.success,
            pnl: result.pnl,
        })
    }

    async fn shutdown_sync(
        &mut self,
        current_price: Option<Decimal>,
        stop_reason: &str,
    ) -> Result<crate::sync_traits::TradeSyncResult, SyncError> {
        let result = TradeSyncer::shutdown_sync(self, current_price, stop_reason).await?;
        Ok(crate::sync_traits::TradeSyncResult {
            success: result.success,
            pnl: result.pnl,
        })
    }
}

#[cfg(test)]

mod tests {
    use super::*;
    use bot_core::{AssetId, Fee, OrderSide, Price, Qty, TradeId};

    fn make_test_fill(trade_id: &str, instrument: &str) -> Fill {
        // Use a timestamp in the future to ensure it passes the start_timestamp check
        Fill {
            trade_id: TradeId::new(trade_id),
            client_id: None,
            exchange_order_id: None,
            instrument: InstrumentId::new(instrument),
            side: OrderSide::Buy,
            price: Price::new(Decimal::new(100, 0)),
            qty: Qty::new(Decimal::new(1, 0)),
            fee: Fee::new(Decimal::new(1, 2), AssetId::new("USDC")),
            ts: now_ms() + 1000, // Future timestamp to pass start_timestamp filter
        }
    }

    fn make_test_fill_with_ts(trade_id: &str, instrument: &str, ts: i64) -> Fill {
        Fill {
            trade_id: TradeId::new(trade_id),
            client_id: None,
            exchange_order_id: None,
            instrument: InstrumentId::new(instrument),
            side: OrderSide::Buy,
            price: Price::new(Decimal::new(100, 0)),
            qty: Qty::new(Decimal::new(1, 0)),
            fee: Fee::new(Decimal::new(1, 2), AssetId::new("USDC")),
            ts,
        }
    }

    #[test]
    fn test_config_validation() {
        // Empty bot_id should fail
        let config = TradeSyncerConfig {
            bot_id: String::new(),
            upstream_url: "http://test.com".to_string(),
            ..Default::default()
        };
        assert!(TradeSyncer::new(config).is_err());

        // Empty upstream_url should fail
        let config = TradeSyncerConfig {
            bot_id: "test-bot".to_string(),
            upstream_url: String::new(),
            ..Default::default()
        };
        assert!(TradeSyncer::new(config).is_err());

        // Valid config should succeed
        let config = TradeSyncerConfig {
            bot_id: "test-bot".to_string(),
            upstream_url: "http://test.com".to_string(),
            ..Default::default()
        };
        assert!(TradeSyncer::new(config).is_ok());
    }

    #[test]
    fn test_add_fill_deduplication() {
        let config = TradeSyncerConfig {
            bot_id: "test-bot".to_string(),
            upstream_url: "http://test.com".to_string(),
            ..Default::default()
        };
        let mut syncer = TradeSyncer::new(config).unwrap();

        // Add same fill twice (with future timestamp to pass start_timestamp filter)
        let fill = make_test_fill("trade-1", "BTC-PERP");
        syncer.add_fill(fill.clone());
        syncer.add_fill(fill);

        // Should have both since same trade_id isn't marked as synced yet
        assert_eq!(syncer.pending_count(), 2);

        // Mark as synced
        syncer.synced_trade_ids.insert("trade-1".to_string());

        // Clear pending
        syncer.pending_fills.clear();

        // Try to add again - should be skipped (already synced)
        let fill = make_test_fill("trade-1", "BTC-PERP");
        syncer.add_fill(fill);
        assert_eq!(syncer.pending_count(), 0);
    }

    #[test]
    fn test_start_timestamp_filter() {
        let config = TradeSyncerConfig {
            bot_id: "test-bot".to_string(),
            upstream_url: "http://test.com".to_string(),
            ..Default::default()
        };
        let mut syncer = TradeSyncer::new(config).unwrap();

        // Add fill with old timestamp (before syncer started) - should be filtered
        let old_fill =
            make_test_fill_with_ts("trade-old", "BTC-PERP", syncer.start_timestamp - 1000);
        syncer.add_fill(old_fill);
        assert_eq!(
            syncer.pending_count(),
            0,
            "Historical fill should be filtered"
        );

        // Add fill with new timestamp (after syncer started) - should be accepted
        let new_fill =
            make_test_fill_with_ts("trade-new", "BTC-PERP", syncer.start_timestamp + 1000);
        syncer.add_fill(new_fill);
        assert_eq!(syncer.pending_count(), 1, "New fill should be accepted");
    }

    #[test]
    fn test_instrument_filter() {
        // Single instrument filter
        let config = TradeSyncerConfig {
            bot_id: "test-bot".to_string(),
            upstream_url: "http://test.com".to_string(),
            instruments: vec![InstrumentId::new("BTC-PERP")],
            ..Default::default()
        };
        let mut syncer = TradeSyncer::new(config).unwrap();

        // Add fill for correct instrument
        syncer.add_fill(make_test_fill("trade-1", "BTC-PERP"));
        assert_eq!(syncer.pending_count(), 1);

        // Add fill for different instrument - should be filtered
        syncer.add_fill(make_test_fill("trade-2", "ETH-PERP"));
        assert_eq!(syncer.pending_count(), 1); // Still 1
    }

    #[test]
    fn test_multi_instrument_filter() {
        // Multi-instrument filter (arb: spot + perp)
        let config = TradeSyncerConfig {
            bot_id: "test-bot".to_string(),
            upstream_url: "http://test.com".to_string(),
            instruments: vec![
                InstrumentId::new("UBTC-SPOT"),
                InstrumentId::new("BTC-PERP"),
            ],
            ..Default::default()
        };
        let mut syncer = TradeSyncer::new(config).unwrap();

        // Both instruments should pass
        syncer.add_fill(make_test_fill("trade-1", "UBTC-SPOT"));
        syncer.add_fill(make_test_fill("trade-2", "BTC-PERP"));
        assert_eq!(syncer.pending_count(), 2);

        // Unrelated instrument should be filtered
        syncer.add_fill(make_test_fill("trade-3", "ETH-PERP"));
        assert_eq!(syncer.pending_count(), 2); // Still 2
    }

    #[test]
    fn test_fill_to_trade_conversion() {
        let config = TradeSyncerConfig {
            bot_id: "test-bot".to_string(),
            upstream_url: "http://test.com".to_string(),
            ..Default::default()
        };
        let syncer = TradeSyncer::new(config).unwrap();

        let fill = Fill {
            trade_id: TradeId::new("trade-123"),
            client_id: Some(bot_core::ClientOrderId::new("client-456")),
            exchange_order_id: Some(bot_core::ExchangeOrderId::new("exchange-789")),
            instrument: InstrumentId::new("BTC-PERP"),
            side: OrderSide::Sell,
            price: Price::new(Decimal::new(50000, 0)),
            qty: Qty::new(Decimal::new(5, 1)), // 0.5
            fee: Fee::new(Decimal::new(25, 2), AssetId::new("USDC")), // 0.25
            ts: 1700000000000,
        };

        let trade = syncer.fill_to_trade(&fill);

        assert_eq!(trade.trade_id, "trade-123");
        assert_eq!(trade.client_order_id, "client-456");
        assert_eq!(trade.venue_order_id, "exchange-789");
        assert_eq!(trade.instrument_id, "BTC-PERP");
        assert_eq!(trade.side, "SELL");
        assert_eq!(trade.price, "50000");
        assert_eq!(trade.qty, "0.5");
        // quote_notional = 50000 * 0.5 = 25000 (may have trailing decimal depending on Decimal impl)
        assert!(
            trade.quote_notional.starts_with("25000"),
            "Expected quote_notional to start with 25000, got: {}",
            trade.quote_notional
        );
        assert_eq!(trade.fee, "0.25");
        assert_eq!(trade.fee_currency, "USDC");
        assert_eq!(trade.ts_event, 1700000000000);
    }
}
