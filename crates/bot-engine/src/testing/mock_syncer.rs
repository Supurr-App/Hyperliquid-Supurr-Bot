//! Mock Syncers for testing backend integration without real API calls.

use async_lock::RwLock;
use bot_core::{AccountState, PositionSnapshot};
use rust_decimal::Decimal;
use std::sync::Arc;
use std::time::Duration;

// Re-export error types
pub use crate::account_syncer::{SyncError, SyncResult};
pub use bot_core::Fill;

// === MockAccountSyncer (for Arbitrage/Snapshot strategies) ===

#[derive(Debug, Clone)]
pub struct AccountSyncCall {
    pub account_value: Decimal,
    pub unrealized_pnl: Decimal,
    pub positions: Vec<PositionInfo>,
    pub ts: i64,
    pub stop_bot: bool,
    pub stop_reason: String,
}

#[derive(Debug, Clone)]
pub struct PositionInfo {
    pub instrument_id: String,
    pub qty: String,
    pub entry_px: String,
    pub unrealized_pnl: String,
}

struct MockAccountSyncerState {
    sync_calls: Vec<AccountSyncCall>,
    should_fail: bool,
    simulated_pnl: f64,
    network_delay_ms: u64,
}

pub struct MockAccountSyncer {
    inner: Arc<RwLock<MockAccountSyncerState>>,
}

impl MockAccountSyncer {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(MockAccountSyncerState {
                sync_calls: Vec::new(),
                should_fail: false,
                simulated_pnl: 0.0,
                network_delay_ms: 0,
            })),
        }
    }

    // === KNOBS ===

    pub async fn set_should_fail(&self, should_fail: bool) {
        self.inner.write().await.should_fail = should_fail;
    }

    pub async fn set_simulated_pnl(&self, pnl: f64) {
        self.inner.write().await.simulated_pnl = pnl;
    }

    pub async fn set_network_delay(&self, ms: u64) {
        self.inner.write().await.network_delay_ms = ms;
    }

    // === VERIFICATION ===

    pub async fn sync_calls(&self) -> Vec<AccountSyncCall> {
        self.inner.read().await.sync_calls.clone()
    }

    pub async fn last_sync(&self) -> Option<AccountSyncCall> {
        self.inner.read().await.sync_calls.last().cloned()
    }

    pub async fn shutdown_syncs(&self) -> Vec<AccountSyncCall> {
        self.inner
            .read()
            .await
            .sync_calls
            .iter()
            .filter(|c| c.stop_bot)
            .cloned()
            .collect()
    }

    pub async fn assert_shutdown_sync_sent(&self) {
        let shutdowns = self.shutdown_syncs().await;
        assert!(!shutdowns.is_empty(), "No shutdown sync was sent");
        assert_eq!(shutdowns.len(), 1, "Multiple shutdown syncs sent");
    }

    pub async fn assert_periodic_syncs(&self, min_count: usize) {
        let state = self.inner.read().await;
        let active_syncs: Vec<_> = state.sync_calls.iter().filter(|c| !c.stop_bot).collect();

        assert!(
            active_syncs.len() >= min_count,
            "Expected at least {} periodic syncs, got {}",
            min_count,
            active_syncs.len()
        );
    }

    // === SYNCER IMPLEMENTATION ===

    pub async fn sync(
        &mut self,
        account_state: &AccountState,
        stop_bot: bool,
        stop_reason: &str,
    ) -> Result<SyncResult, SyncError> {
        let mut state = self.inner.write().await;

        // Simulate network delay
        if state.network_delay_ms > 0 {
            let delay_ms = state.network_delay_ms;
            drop(state); // Release lock during sleep
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            state = self.inner.write().await;
        }

        // Check failure mode
        if state.should_fail {
            return Err(SyncError::Network("Mock network failure".into()));
        }

        // Record the sync call
        let positions: Vec<PositionInfo> = account_state
            .positions
            .iter()
            .map(|p| PositionInfo {
                instrument_id: p.instrument.to_string(),
                qty: p.qty.to_string(),
                entry_px: p
                    .avg_entry_px
                    .map(|px| px.0.to_string())
                    .unwrap_or_else(|| "0".into()),
                unrealized_pnl: p
                    .unrealized_pnl
                    .map(|pnl| pnl.to_string())
                    .unwrap_or_else(|| "0".into()),
            })
            .collect();

        state.sync_calls.push(AccountSyncCall {
            account_value: account_state.account_value.unwrap_or_default(),
            unrealized_pnl: account_state.unrealized_pnl.unwrap_or_default(),
            positions,
            ts: bot_core::now_ms() / 1000,
            stop_bot,
            stop_reason: stop_reason.to_string(),
        });

        Ok(SyncResult {
            success: true,
            pnl: Some(state.simulated_pnl),
        })
    }

    pub async fn shutdown_sync(
        &mut self,
        account_state: &AccountState,
        stop_reason: &str,
    ) -> Result<SyncResult, SyncError> {
        self.sync(account_state, true, stop_reason).await
    }
}

// Implement AccountSync trait for drop-in substitution
#[async_trait::async_trait]
impl crate::sync_traits::AccountSync for MockAccountSyncer {
    fn should_sync(&self) -> bool {
        true // Mock always returns true, tests control when to call sync
    }

    fn last_pnl(&self) -> Option<f64> {
        // Return from inner state synchronously would require blocking
        // For mock, we just return None - tests should use sync_calls() to verify
        None
    }

    async fn sync(
        &mut self,
        account_state: &AccountState,
        stop_bot: bool,
        stop_reason: &str,
    ) -> Result<crate::sync_traits::AccountSyncResult, SyncError> {
        let result = MockAccountSyncer::sync(self, account_state, stop_bot, stop_reason).await?;
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
        let result = MockAccountSyncer::shutdown_sync(self, account_state, stop_reason).await?;
        Ok(crate::sync_traits::AccountSyncResult {
            success: result.success,
            pnl: result.pnl,
        })
    }
}

impl Default for MockAccountSyncer {
    fn default() -> Self {
        Self::new()
    }
}

// === MockTradeSyncer (for Grid/MM strategies) ===

#[derive(Debug, Clone)]
pub struct TradeSyncCall {
    pub fills: Vec<Fill>,
    pub current_price: Option<Decimal>,
    pub stop_bot: bool,
    pub stop_reason: String,
    pub timestamp: i64,
}

struct MockTradeSyncerState {
    sync_calls: Vec<TradeSyncCall>,
    should_fail: bool,
    simulated_pnl: f64,
}

pub struct MockTradeSyncer {
    inner: Arc<RwLock<MockTradeSyncerState>>,
}

impl MockTradeSyncer {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(MockTradeSyncerState {
                sync_calls: Vec::new(),
                should_fail: false,
                simulated_pnl: 0.0,
            })),
        }
    }

    // === KNOBS ===

    pub async fn set_should_fail(&self, should_fail: bool) {
        self.inner.write().await.should_fail = should_fail;
    }

    pub async fn set_simulated_pnl(&self, pnl: f64) {
        self.inner.write().await.simulated_pnl = pnl;
    }

    // === VERIFICATION ===

    pub async fn sync_calls(&self) -> Vec<TradeSyncCall> {
        self.inner.read().await.sync_calls.clone()
    }

    pub async fn total_fills_synced(&self) -> usize {
        self.inner
            .read()
            .await
            .sync_calls
            .iter()
            .map(|c| c.fills.len())
            .sum()
    }

    pub async fn assert_all_fills_synced(&self, expected_fills: &[Fill]) {
        let total = self.total_fills_synced().await;
        assert_eq!(
            total,
            expected_fills.len(),
            "Expected {} fills synced, got {}",
            expected_fills.len(),
            total
        );
    }

    // === SYNCER IMPLEMENTATION ===

    pub async fn sync(
        &mut self,
        fills: Vec<Fill>,
        current_price: Option<Decimal>,
        stop_bot: bool,
        stop_reason: &str,
    ) -> Result<SyncResult, SyncError> {
        let mut state = self.inner.write().await;

        if state.should_fail {
            return Err(SyncError::Network("Mock sync failure".into()));
        }

        state.sync_calls.push(TradeSyncCall {
            fills,
            current_price,
            stop_bot,
            stop_reason: stop_reason.to_string(),
            timestamp: bot_core::now_ms(),
        });

        Ok(SyncResult {
            success: true,
            pnl: Some(state.simulated_pnl),
        })
    }
}

// Implement TradeSync trait for drop-in substitution
#[async_trait::async_trait]
impl crate::sync_traits::TradeSync for MockTradeSyncer {
    fn add_fill(&mut self, _fill: Fill) {
        // For MockTradeSyncer, fills are passed directly to sync()
        // This is a no-op, tests use sync() directly with fills
    }

    fn should_sync(&self) -> bool {
        true // Mock always returns true
    }

    fn pending_count(&self) -> usize {
        0 // Mock doesn't accumulate, passes fills directly to sync
    }

    fn last_pnl(&self) -> Option<f64> {
        None // Tests use sync_calls() to verify
    }

    async fn sync(
        &mut self,
        current_price: Option<rust_decimal::Decimal>,
        stop_bot: bool,
        stop_reason: &str,
    ) -> Result<crate::sync_traits::TradeSyncResult, SyncError> {
        // For trait impl, we sync with empty fills (trait-based usage)
        let result =
            MockTradeSyncer::sync(self, vec![], current_price, stop_bot, stop_reason).await?;
        Ok(crate::sync_traits::TradeSyncResult {
            success: result.success,
            pnl: result.pnl,
        })
    }

    async fn shutdown_sync(
        &mut self,
        current_price: Option<rust_decimal::Decimal>,
        stop_reason: &str,
    ) -> Result<crate::sync_traits::TradeSyncResult, SyncError> {
        let result = MockTradeSyncer::sync(self, vec![], current_price, true, stop_reason).await?;
        Ok(crate::sync_traits::TradeSyncResult {
            success: result.success,
            pnl: result.pnl,
        })
    }
}

impl Default for MockTradeSyncer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bot_core::InstrumentId;

    #[tokio::test]
    async fn test_account_syncer_recording() {
        let mut syncer = MockAccountSyncer::new();

        let account_state = AccountState {
            positions: vec![PositionSnapshot {
                instrument: InstrumentId::new("ETH-PERP"),
                qty: Decimal::new(-1, 1), // -0.1
                avg_entry_px: Some(bot_core::Price::new(Decimal::new(3000, 0))),
                unrealized_pnl: Some(Decimal::new(10, 0)),
            }],
            account_value: Some(Decimal::new(50000, 0)),
            unrealized_pnl: Some(Decimal::new(10, 0)),
        };

        syncer.sync(&account_state, false, "").await.unwrap();

        let calls = syncer.sync_calls().await;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].stop_bot, false);
        assert_eq!(calls[0].positions.len(), 1);
    }

    #[tokio::test]
    async fn test_shutdown_sync() {
        let mut syncer = MockAccountSyncer::new();

        let account_state = AccountState {
            positions: vec![],
            account_value: Some(Decimal::new(50000, 0)),
            unrealized_pnl: Some(Decimal::ZERO),
        };

        syncer
            .shutdown_sync(&account_state, "shutdown:external")
            .await
            .unwrap();

        syncer.assert_shutdown_sync_sent().await;

        let shutdown = syncer.last_sync().await.unwrap();
        assert_eq!(shutdown.stop_bot, true);
        assert_eq!(shutdown.stop_reason, "shutdown:external");
    }

    #[tokio::test]
    async fn test_syncer_failure_mode() {
        let mut syncer = MockAccountSyncer::new();
        syncer.set_should_fail(true).await;

        let account_state = AccountState {
            positions: vec![],
            account_value: None,
            unrealized_pnl: None,
        };

        let result = syncer.sync(&account_state, false, "").await;
        assert!(result.is_err());

        // No calls recorded on failure
        let calls = syncer.sync_calls().await;
        assert_eq!(calls.len(), 0);
    }
}
