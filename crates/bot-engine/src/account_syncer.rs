//! Account syncer: syncs account state snapshots to upstream API.
//!
//! This module provides async HTTP syncing of clearinghouse state snapshots
//! to an external API for Arbitrage and other snapshot-based strategies.

use bot_core::{now_ms, AccountState};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Configuration for account syncing
#[derive(Debug, Clone)]
pub struct AccountSyncerConfig {
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
}

impl Default for AccountSyncerConfig {
    fn default() -> Self {
        Self {
            bot_id: String::new(),
            upstream_url: String::new(),
            sync_interval_ms: 10_000,
            timeout_secs: 10,
            max_retries: 3,
            retry_delay_ms: 1000,
        }
    }
}

/// Request payload for clearinghouse state sync API
#[derive(Debug, Clone, Serialize)]
pub struct ClearingHouseStateRequest {
    pub account_value: String,
    pub unrealized_pnl: String,
    pub positions: Vec<PositionInfo>,
    pub ts: i64,
    pub stop_bot: bool,
    pub stop_reason: String,
    /// Optional metadata for strategy-specific data
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Position information for sync
#[derive(Debug, Clone, Serialize)]
pub struct PositionInfo {
    pub instrument_id: String,
    pub qty: String,
    pub entry_px: String,
    pub unrealized_pnl: String,
}

/// Response from clearinghouse state sync API
#[derive(Debug, Clone, Deserialize)]
pub struct SyncResponse {
    pub pnl: f64,
}

/// Error types for account syncing
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("HTTP request failed: {0}")]
    Http(String),

    #[error("Network error: {0}")]
    Network(String),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("API error: status={status}, body={body}")]
    Api { status: u16, body: String },

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Max retries exceeded")]
    MaxRetries,
}

impl From<reqwest::Error> for SyncError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            SyncError::Network("Request timeout".to_string())
        } else if e.is_connect() {
            SyncError::Network(format!("Connection failed: {}", e))
        } else {
            SyncError::Http(e.to_string())
        }
    }
}

/// Result of a sync operation
#[derive(Debug, Clone)]
pub struct SyncResult {
    pub success: bool,
    pub pnl: Option<f64>,
}

/// Account syncer - handles syncing clearinghouse state to upstream API
pub struct AccountSyncer {
    config: AccountSyncerConfig,
    client: Client,
    /// Last successful sync timestamp
    last_sync_ts: i64,
    /// Last sync PnL
    last_pnl: Option<f64>,
}

impl AccountSyncer {
    /// Create a new account syncer with the given configuration
    pub fn new(config: AccountSyncerConfig) -> Result<Self, SyncError> {
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

        tracing::info!("[AccountSyncer] Initialized for bot_id={}", config.bot_id);

        Ok(Self {
            config,
            client,
            last_sync_ts: 0,
            last_pnl: None,
        })
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

    /// Sync account state to upstream API
    ///
    /// Returns the PnL from the upstream API on success
    pub async fn sync(
        &mut self,
        account_state: &AccountState,
        stop_bot: bool,
        stop_reason: &str,
    ) -> Result<SyncResult, SyncError> {
        let now = now_ms();

        // Convert AccountState to request format
        let positions: Vec<PositionInfo> = account_state
            .positions
            .iter()
            .map(|p| PositionInfo {
                instrument_id: p.instrument.0.clone(),
                qty: p.qty.to_string(),
                entry_px: p
                    .avg_entry_px
                    .map(|px| px.0.to_string())
                    .unwrap_or_else(|| "0".to_string()),
                unrealized_pnl: p
                    .unrealized_pnl
                    .map(|pnl| pnl.to_string())
                    .unwrap_or_else(|| "0".to_string()),
            })
            .collect();

        let request = ClearingHouseStateRequest {
            account_value: account_state
                .account_value
                .map(|v| v.to_string())
                .unwrap_or_else(|| "0".to_string()),
            unrealized_pnl: account_state
                .unrealized_pnl
                .map(|pnl| pnl.to_string())
                .unwrap_or_else(|| "0".to_string()),
            positions,
            ts: now / 1000, // seconds
            stop_bot,
            stop_reason: stop_reason.to_string(),
            metadata: None,
        };

        tracing::info!(
            "[AccountSyncer] Syncing account state: positions={}, pnl={:?}",
            account_state.positions.len(),
            account_state.unrealized_pnl
        );

        // Execute with retry
        let response = self.execute_with_retry(&request).await?;

        // Update state
        self.last_sync_ts = now;
        self.last_pnl = Some(response.pnl);

        tracing::info!("[AccountSyncer] Sync successful: pnl={:.4}", response.pnl);

        Ok(SyncResult {
            success: true,
            pnl: Some(response.pnl),
        })
    }

    /// Execute sync request with retry logic
    async fn execute_with_retry(
        &self,
        request: &ClearingHouseStateRequest,
    ) -> Result<SyncResponse, SyncError> {
        let url = format!(
            "{}/sync-clearingHouseState/{}",
            self.config.upstream_url.trim_end_matches('/'),
            self.config.bot_id
        );

        let mut last_error: Option<SyncError> = None;
        let mut delay_ms = self.config.retry_delay_ms;

        for attempt in 1..=self.config.max_retries {
            tracing::debug!(
                "[AccountSyncer] Sync attempt {}/{} to {}",
                attempt,
                self.config.max_retries,
                url
            );

            match self.execute_request(&url, request).await {
                Ok(response) => return Ok(response),
                Err(e) => {
                    tracing::warn!(
                        "[AccountSyncer] Sync attempt {}/{} failed: {}",
                        attempt,
                        self.config.max_retries,
                        e
                    );
                    last_error = Some(e);

                    if attempt < self.config.max_retries {
                        tracing::debug!("[AccountSyncer] Retrying in {}ms...", delay_ms);
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
        request: &ClearingHouseStateRequest,
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

    /// Perform final sync on shutdown (with stop_bot=true)
    pub async fn shutdown_sync(
        &mut self,
        account_state: &AccountState,
        stop_reason: &str,
    ) -> Result<SyncResult, SyncError> {
        tracing::info!(
            "[AccountSyncer] Performing shutdown sync with reason: {}",
            stop_reason
        );
        self.sync(account_state, true, stop_reason).await
    }
}

// Implement AccountSync trait for drop-in substitution with MockAccountSyncer
#[async_trait::async_trait]
impl crate::sync_traits::AccountSync for AccountSyncer {
    fn should_sync(&self) -> bool {
        AccountSyncer::should_sync(self)
    }

    fn last_pnl(&self) -> Option<f64> {
        AccountSyncer::last_pnl(self)
    }

    async fn sync(
        &mut self,
        account_state: &AccountState,
        stop_bot: bool,
        stop_reason: &str,
    ) -> Result<crate::sync_traits::AccountSyncResult, SyncError> {
        let result = AccountSyncer::sync(self, account_state, stop_bot, stop_reason).await?;
        Ok(crate::sync_traits::AccountSyncResult {
            success: result.success,
            pnl: result.pnl,
        })
    }

    async fn shutdown_sync(
        &mut self,
        account_state: &AccountState,
        stop_reason: &str,
    ) -> Result<crate::sync_traits::AccountSyncResult, SyncError> {
        let result = AccountSyncer::shutdown_sync(self, account_state, stop_reason).await?;
        Ok(crate::sync_traits::AccountSyncResult {
            success: result.success,
            pnl: result.pnl,
        })
    }
}

#[cfg(test)]

mod tests {
    use super::*;

    #[test]
    fn test_config_validation() {
        // Empty bot_id should fail
        let config = AccountSyncerConfig {
            bot_id: String::new(),
            upstream_url: "http://test.com".to_string(),
            ..Default::default()
        };
        assert!(AccountSyncer::new(config).is_err());

        // Empty upstream_url should fail
        let config = AccountSyncerConfig {
            bot_id: "test-bot".to_string(),
            upstream_url: String::new(),
            ..Default::default()
        };
        assert!(AccountSyncer::new(config).is_err());

        // Valid config should succeed
        let config = AccountSyncerConfig {
            bot_id: "test-bot".to_string(),
            upstream_url: "http://test.com".to_string(),
            ..Default::default()
        };
        assert!(AccountSyncer::new(config).is_ok());
    }
}
