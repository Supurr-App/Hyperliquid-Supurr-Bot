//! Hyperliquid exchange adapter (HTTP-only).
//!
//! This crate provides a Hyperliquid exchange implementation that uses
//! HTTP APIs exclusively (no WebSockets).
//!
//! ## Features
//!
//! - Order placement with EIP-712 signing
//! - Order cancellation (by OID or cloid)
//! - User fills polling
//! - Quote (mid price) polling
//! - Account state queries
//!
//! ## Usage
//!
//! ```rust,ignore
//! use exchange_hyperliquid::{HyperliquidConfig, new_client};
//! use bot_core::Environment;
//!
//! let config = HyperliquidConfig {
//!     environment: Environment::Testnet,
//!     private_key: "your_private_key".to_string(),
//!     main_address: None,
//!     vault_address: None,
//!     timeout_secs: 10,
//!     proxy_url: None,
//!     base_url_override: None,
//! };
//!
//! let client = new_client(config)?;
//! ```

pub mod client;
pub mod signing;
pub mod types;

pub use client::{new_client, new_client_with_registration, HyperliquidClient};
pub use signing::HyperliquidSigner;
pub use types::{BuilderFee, Hip3Config, HyperliquidConfig};
