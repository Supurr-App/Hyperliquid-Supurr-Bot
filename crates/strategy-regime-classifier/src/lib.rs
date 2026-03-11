//! Market Regime Classifier
//!
//! Detects which of four market regimes the current price action represents:
//! - **Trending**: Strong directional move (ADX > 25)
//! - **Ranging**: Oscillating sideways (ADX < 20)
//! - **LowVol**: Tight consolidation (Bollinger squeeze)
//! - **VolExpansion**: Wild swings (ATR spike)
//!
//! Uses ATR + Bollinger Width from the `ta` crate and a custom ADX indicator.
//! Includes hysteresis-based debouncing to prevent regime flickering.

pub mod adx;
pub mod bar_ext;
pub mod classifier;

pub use classifier::{Regime, RegimeClassifier, RegimeClassifierConfig};
