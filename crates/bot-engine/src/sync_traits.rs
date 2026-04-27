//! Syncer traits for testing abstraction.
//!
//! These traits allow substituting real syncers with mock implementations
//! for paper trading and testing scenarios.

use bot_core::{AccountState, Fill};
use rust_decimal::Decimal;

use crate::account_syncer::SyncError;
use crate::performance_metrics::PerformanceMetricsSnapshot;

/// Common result for sync operations (trait-level abstraction)
#[derive(Debug, Clone)]
pub struct TradeSyncResult {
    pub success: bool,
    pub pnl: Option<f64>,
}

/// Trait for trade-based syncing (Grid, MM strategies)
///
/// Syncers implementing this trait accept fills and sync them to upstream.
#[async_trait::async_trait]
pub trait TradeSync: Send + Sync {
    /// Add a fill to the pending queue
    fn add_fill(&mut self, fill: Fill);

    /// Check if it's time to sync based on interval
    fn should_sync(&self) -> bool;

    /// Get count of pending fills
    fn pending_count(&self) -> usize;

    /// Get last known PnL
    fn last_pnl(&self) -> Option<f64>;

    /// Set the latest performance metrics snapshot to include in sync metadata.
    fn set_metrics_snapshot(&mut self, _snapshot: Option<PerformanceMetricsSnapshot>) {}

    /// Sync pending fills to upstream
    async fn sync(
        &mut self,
        current_price: Option<Decimal>,
        stop_bot: bool,
        stop_reason: &str,
    ) -> Result<TradeSyncResult, SyncError>;

    /// Perform final sync on shutdown
    async fn shutdown_sync(
        &mut self,
        current_price: Option<Decimal>,
        stop_reason: &str,
    ) -> Result<TradeSyncResult, SyncError>;
}

/// Common result for account sync operations
#[derive(Debug, Clone)]
pub struct AccountSyncResult {
    pub success: bool,
    pub pnl: Option<f64>,
}

/// Trait for account snapshot syncing (Arbitrage strategies)
///
/// Syncers implementing this trait accept full account state snapshots.
#[async_trait::async_trait]
pub trait AccountSync: Send + Sync {
    /// Check if it's time to sync based on interval
    fn should_sync(&self) -> bool;

    /// Get last known PnL
    fn last_pnl(&self) -> Option<f64>;

    /// Set the latest performance metrics snapshot to include in sync metadata.
    fn set_metrics_snapshot(&mut self, _snapshot: Option<PerformanceMetricsSnapshot>) {}

    /// Sync account state to upstream
    async fn sync(
        &mut self,
        account_state: &AccountState,
        stop_bot: bool,
        stop_reason: &str,
    ) -> Result<AccountSyncResult, SyncError>;

    /// Perform final sync on shutdown
    async fn shutdown_sync(
        &mut self,
        account_state: &AccountState,
        stop_reason: &str,
    ) -> Result<AccountSyncResult, SyncError>;
}
