//! Bot CLI library exports.
//!
//! This crate provides the bot CLI runner and configuration types.
//! The config module exports types used by both the CLI and schema generation.

pub mod config;

// Re-export commonly used types
pub use config::BotConfig;
