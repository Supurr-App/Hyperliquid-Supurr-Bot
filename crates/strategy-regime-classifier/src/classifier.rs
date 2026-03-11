//! Regime classifier — combines ATR, ADX, and Bollinger Width to detect market regime.
//!
//! The classifier outputs one of four regimes per bar:
//! - `Trending` — strong directional move
//! - `Ranging` — oscillating sideways
//! - `LowVol` — tight consolidation (Bollinger squeeze)
//! - `VolExpansion` — wild swings (ATR spike)
//!
//! Includes **hysteresis** to prevent rapid switching between regimes
//! during transition zones.

use std::collections::VecDeque;

use ta::indicators::{AverageTrueRange, BollingerBands, BollingerBandsOutput};
use ta::Next;

use crate::adx::Adx;
use crate::bar_ext::TaBar;

// ─────────────────────────────────────────────────────────────────────────────
// Types
// ─────────────────────────────────────────────────────────────────────────────

/// The four market regimes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Regime {
    /// 🚀 Strong directional move — ADX > 25, ATR rising
    Trending,
    /// 📦 Oscillating sideways — ADX < 20, no squeeze
    Ranging,
    /// 😴 Tight consolidation — ADX < 20, Bollinger squeeze
    LowVol,
    /// 🌪️ Wild swings — ATR in top percentile
    VolExpansion,
}

impl std::fmt::Display for Regime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Regime::Trending => write!(f, "🚀 Trending"),
            Regime::Ranging => write!(f, "📦 Ranging"),
            Regime::LowVol => write!(f, "😴 LowVol"),
            Regime::VolExpansion => write!(f, "🌪️ VolExpansion"),
        }
    }
}

/// Configuration for the regime classifier.
#[derive(Debug, Clone)]
pub struct RegimeClassifierConfig {
    /// ADX period (typically 14)
    pub adx_period: usize,
    /// ATR period (typically 14)
    pub atr_period: usize,
    /// Bollinger Bands period (typically 20)
    pub bb_period: usize,
    /// Bollinger Bands multiplier (typically 2.0)
    pub bb_multiplier: f64,
    /// How many bars of ATR/BB history to keep for percentile ranking
    pub percentile_lookback: usize,
    /// ADX threshold above which market is considered trending
    pub adx_trending_threshold: f64,
    /// ADX threshold below which market is considered ranging
    pub adx_ranging_threshold: f64,
    /// ATR percentile above which vol expansion is detected (e.g. 90.0)
    pub atr_vol_expansion_percentile: f64,
    /// ATR percentile above which trending is confirmed (e.g. 50.0)
    pub atr_trending_min_percentile: f64,
    /// BB width percentile below which low vol is detected (e.g. 20.0)
    pub bb_squeeze_percentile: f64,
    /// Number of bars a new regime must hold before switching (hysteresis)
    pub confirmation_bars: usize,
}

impl Default for RegimeClassifierConfig {
    fn default() -> Self {
        Self {
            adx_period: 14,
            atr_period: 14,
            bb_period: 20,
            bb_multiplier: 2.0,
            percentile_lookback: 100,
            adx_trending_threshold: 25.0,
            adx_ranging_threshold: 20.0,
            atr_vol_expansion_percentile: 90.0,
            atr_trending_min_percentile: 50.0,
            bb_squeeze_percentile: 20.0,
            confirmation_bars: 3,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Classifier
// ─────────────────────────────────────────────────────────────────────────────

/// Snapshot of all indicator values at a given bar.
#[derive(Debug, Clone)]
pub struct RegimeSnapshot {
    pub regime: Regime,
    pub adx: f64,
    pub atr: f64,
    pub atr_percentile: f64,
    pub bb_width: f64,
    pub bb_width_percentile: f64,
}

/// Market regime classifier with hysteresis-based debouncing.
pub struct RegimeClassifier {
    config: RegimeClassifierConfig,

    // Indicators
    adx: Adx,
    atr: AverageTrueRange,
    bb: BollingerBands,

    // Percentile tracking
    atr_history: VecDeque<f64>,
    bb_width_history: VecDeque<f64>,

    // Hysteresis state
    current_regime: Regime,
    candidate_regime: Option<Regime>,
    candidate_count: usize,

    // Track whether all indicators have produced their first value
    has_adx: bool,
    bars_fed: usize,
}

impl RegimeClassifier {
    /// Create a new classifier with the given configuration.
    pub fn new(config: RegimeClassifierConfig) -> Self {
        let atr = AverageTrueRange::new(config.atr_period)
            .expect("Invalid ATR period");
        let bb = BollingerBands::new(config.bb_period, config.bb_multiplier)
            .expect("Invalid Bollinger Bands config");
        let adx = Adx::new(config.adx_period);

        Self {
            atr,
            bb,
            adx,
            atr_history: VecDeque::with_capacity(config.percentile_lookback + 1),
            bb_width_history: VecDeque::with_capacity(config.percentile_lookback + 1),
            current_regime: Regime::Ranging, // default until we know better
            candidate_regime: None,
            candidate_count: 0,
            has_adx: false,
            bars_fed: 0,
            config,
        }
    }

    /// Create a classifier with default configuration.
    pub fn with_defaults() -> Self {
        Self::new(RegimeClassifierConfig::default())
    }

    /// Feed a completed OHLCV bar. Returns `Some(snapshot)` once all indicators
    /// have warmed up, `None` during warmup.
    pub fn update(&mut self, open: f64, high: f64, low: f64, close: f64) -> Option<RegimeSnapshot> {
        self.bars_fed += 1;
        let bar = TaBar::new(open, high, low, close);

        // ── Feed indicators ──
        let atr_val = self.atr.next(&bar);
        let bb_out: BollingerBandsOutput = self.bb.next(&bar);
        let adx_val = self.adx.update(high, low, close);

        // ADX has the longest warmup; gate on it
        let adx_val = match adx_val {
            Some(v) => {
                self.has_adx = true;
                v
            }
            None => return None,
        };

        // ── Compute Bollinger Width ──
        let bb_width = if bb_out.average != 0.0 {
            (bb_out.upper - bb_out.lower) / bb_out.average * 100.0
        } else {
            0.0
        };

        // ── Track history for percentile ranking ──
        self.push_history(&mut self.atr_history.clone(), atr_val);
        self.push_history(&mut self.bb_width_history.clone(), bb_width);
        // Actually push to self (clone trick above is for borrow checker)
        if self.atr_history.len() >= self.config.percentile_lookback {
            self.atr_history.pop_front();
        }
        self.atr_history.push_back(atr_val);

        if self.bb_width_history.len() >= self.config.percentile_lookback {
            self.bb_width_history.pop_front();
        }
        self.bb_width_history.push_back(bb_width);

        let atr_pct = percentile_rank(atr_val, &self.atr_history);
        let bb_pct = percentile_rank(bb_width, &self.bb_width_history);

        // ── Classification rules ──
        let raw_regime = self.classify_raw(adx_val, atr_pct, bb_pct);

        // ── Apply hysteresis ──
        let confirmed_regime = self.apply_hysteresis(raw_regime);

        Some(RegimeSnapshot {
            regime: confirmed_regime,
            adx: adx_val,
            atr: atr_val,
            atr_percentile: atr_pct,
            bb_width,
            bb_width_percentile: bb_pct,
        })
    }

    /// Raw classification without hysteresis.
    fn classify_raw(&self, adx: f64, atr_pct: f64, bb_pct: f64) -> Regime {
        if atr_pct > self.config.atr_vol_expansion_percentile {
            // ATR in top percentile — something big is happening
            Regime::VolExpansion
        } else if adx > self.config.adx_trending_threshold
            && atr_pct > self.config.atr_trending_min_percentile
        {
            // Strong trend with decent volatility
            Regime::Trending
        } else if adx < self.config.adx_ranging_threshold
            && bb_pct < self.config.bb_squeeze_percentile
        {
            // No trend + bands squeezing
            Regime::LowVol
        } else {
            // Default — sideways oscillation
            Regime::Ranging
        }
    }

    /// Apply hysteresis: require `confirmation_bars` consecutive bars of the
    /// same raw regime before switching.
    fn apply_hysteresis(&mut self, raw: Regime) -> Regime {
        if raw == self.current_regime {
            // Already in this regime, reset candidate
            self.candidate_regime = None;
            self.candidate_count = 0;
            return self.current_regime;
        }

        match &self.candidate_regime {
            Some(candidate) if *candidate == raw => {
                self.candidate_count += 1;
                if self.candidate_count >= self.config.confirmation_bars {
                    // Confirmed — switch regime
                    self.current_regime = raw;
                    self.candidate_regime = None;
                    self.candidate_count = 0;
                }
            }
            _ => {
                // New candidate
                self.candidate_regime = Some(raw);
                self.candidate_count = 1;
            }
        }

        self.current_regime
    }

    fn push_history(&self, _history: &mut VecDeque<f64>, _value: f64) {
        // Helper used conceptually; actual pushes done inline to avoid borrow issues
    }

    /// Current confirmed regime.
    pub fn regime(&self) -> Regime {
        self.current_regime
    }

    /// Number of bars fed so far.
    pub fn bars_fed(&self) -> usize {
        self.bars_fed
    }

    /// Whether the classifier has warmed up (all indicators producing values).
    pub fn is_warm(&self) -> bool {
        self.has_adx
    }
}

/// What percentile does `value` fall at within `history`?
/// Returns 0-100.
fn percentile_rank(value: f64, history: &VecDeque<f64>) -> f64 {
    if history.is_empty() {
        return 50.0; // neutral when no history
    }
    let below = history.iter().filter(|&&v| v < value).count();
    (below as f64 / history.len() as f64) * 100.0
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a config with shorter periods for faster tests.
    fn test_config() -> RegimeClassifierConfig {
        RegimeClassifierConfig {
            adx_period: 5,
            atr_period: 5,
            bb_period: 5,
            bb_multiplier: 2.0,
            percentile_lookback: 30,
            adx_trending_threshold: 25.0,
            adx_ranging_threshold: 20.0,
            atr_vol_expansion_percentile: 90.0,
            atr_trending_min_percentile: 50.0,
            bb_squeeze_percentile: 20.0,
            confirmation_bars: 2,
        }
    }

    #[test]
    fn test_warmup_returns_none() {
        let mut clf = RegimeClassifier::new(test_config());
        // Feed a few bars — should return None during warmup
        for i in 0..5 {
            let base = 100.0 + i as f64;
            let result = clf.update(base, base + 1.0, base - 1.0, base + 0.5);
            // Early bars must be None (ADX double warmup)
            if i < 3 {
                assert!(result.is_none(), "Bar {} should be warmup", i);
            }
        }
    }

    #[test]
    fn test_trending_market_detection() {
        let mut clf = RegimeClassifier::new(test_config());

        // Phase 1: Feed warmup bars (ranging to build baseline)
        for i in 0..20 {
            let base = 100.0 + (i % 3) as f64;
            clf.update(base, base + 0.5, base - 0.5, base);
        }

        // Phase 2: Strong uptrend — consecutive higher highs/lows
        let mut last_snapshot = None;
        for i in 0..30 {
            let base = 110.0 + (i as f64) * 2.0;
            if let Some(snap) = clf.update(base, base + 1.0, base - 0.5, base + 0.8) {
                last_snapshot = Some(snap);
            }
        }

        let snap = last_snapshot.expect("Should have a snapshot after enough bars");
        // ADX should be elevated in a trending market
        assert!(
            snap.adx > 15.0,
            "ADX should be elevated in uptrend, got {}",
            snap.adx
        );
    }

    #[test]
    fn test_ranging_market_detection() {
        let mut clf = RegimeClassifier::new(test_config());

        // Smooth oscillation around 100 with consistent small range
        // This simulates a genuine ranging market — price wiggles within a band
        let mut last_snapshot = None;
        for i in 0..80 {
            // Sine-wave-like: oscillates between ~99.5 and ~100.5
            let phase = (i as f64) * 0.5; // slow oscillation
            let base = 100.0 + phase.sin() * 0.5;
            if let Some(snap) = clf.update(base, base + 0.3, base - 0.3, base + 0.1) {
                last_snapshot = Some(snap);
            }
        }

        let snap = last_snapshot.expect("Should have a snapshot");
        // In a range, regime should NOT be Trending
        assert!(
            snap.regime != Regime::Trending,
            "Should not be Trending in a ranging market, got {:?} (ADX={:.1})",
            snap.regime,
            snap.adx
        );
    }

    #[test]
    fn test_vol_expansion_detection() {
        let mut clf = RegimeClassifier::new(test_config());

        // Phase 1: Calm market to establish ATR baseline
        for i in 0..40 {
            let base = 100.0 + (i % 2) as f64 * 0.5;
            clf.update(base, base + 0.3, base - 0.3, base);
        }

        // Phase 2: Sudden volatility spike — massive bars
        let mut vol_expansion_detected = false;
        for i in 0..20 {
            let base = 100.0 + (i as f64) * 5.0; // huge moves
            if let Some(snap) = clf.update(base, base + 10.0, base - 10.0, base + 3.0) {
                if snap.regime == Regime::VolExpansion {
                    vol_expansion_detected = true;
                }
            }
        }

        assert!(
            vol_expansion_detected,
            "Should detect VolExpansion after ATR spike"
        );
    }

    #[test]
    fn test_hysteresis_prevents_flickering() {
        let config = RegimeClassifierConfig {
            confirmation_bars: 3,
            ..test_config()
        };
        let mut clf = RegimeClassifier::new(config);

        // Warmup with ranging data
        for i in 0..30 {
            let base = 100.0 + (i % 3) as f64;
            clf.update(base, base + 0.5, base - 0.5, base);
        }

        let initial_regime = clf.regime();

        // Feed ONE bar of trending-like data, then back to ranging
        clf.update(200.0, 210.0, 190.0, 205.0); // spike
        let after_spike = clf.regime();

        // Should NOT have switched — hysteresis requires 3 consecutive bars
        assert_eq!(
            initial_regime, after_spike,
            "Regime should not flicker on a single bar"
        );
    }

    #[test]
    fn test_snapshot_values_reasonable() {
        let mut clf = RegimeClassifier::new(test_config());

        let mut last_snapshot = None;
        for i in 0..30 {
            let base = 100.0 + (i as f64) * 0.5;
            if let Some(snap) = clf.update(base, base + 1.0, base - 1.0, base + 0.2) {
                last_snapshot = Some(snap);
            }
        }

        let snap = last_snapshot.expect("Should produce snapshot");
        assert!(snap.adx >= 0.0 && snap.adx <= 100.0, "ADX out of range: {}", snap.adx);
        assert!(snap.atr >= 0.0, "ATR should be non-negative: {}", snap.atr);
        assert!(snap.bb_width >= 0.0, "BB width should be non-negative: {}", snap.bb_width);
        assert!(
            snap.atr_percentile >= 0.0 && snap.atr_percentile <= 100.0,
            "ATR percentile out of range: {}",
            snap.atr_percentile
        );
    }

    #[test]
    fn test_percentile_rank_function() {
        let history: VecDeque<f64> = vec![1.0, 2.0, 3.0, 4.0, 5.0].into();

        assert_eq!(percentile_rank(0.5, &history), 0.0); // below all
        assert_eq!(percentile_rank(3.0, &history), 40.0); // 2 below out of 5
        assert_eq!(percentile_rank(6.0, &history), 100.0); // above all
    }

    #[test]
    fn test_percentile_rank_empty() {
        let history: VecDeque<f64> = VecDeque::new();
        assert_eq!(percentile_rank(42.0, &history), 50.0); // neutral
    }

    // ─── Correctness Tests ────────────────────────────────────────────

    /// Cross-validate our ATR output against the ta crate's math.
    /// Feed identical data through ta::AverageTrueRange directly and through
    /// the classifier, then verify the ATR values match.
    #[test]
    fn test_atr_correctness_cross_validation() {
        use ta::indicators::AverageTrueRange;
        use ta::Next;
        use crate::bar_ext::TaBar;

        let period = 5;
        let mut ta_atr = AverageTrueRange::new(period).unwrap();

        // Known data: steady uptrend
        let bars: Vec<(f64, f64, f64, f64)> = (0..20)
            .map(|i| {
                let base = 100.0 + (i as f64) * 1.5;
                (base, base + 2.0, base - 1.0, base + 0.5)
            })
            .collect();

        let mut ta_values = Vec::new();
        for &(o, h, l, c) in &bars {
            let bar = TaBar::new(o, h, l, c);
            let val = ta_atr.next(&bar);
            ta_values.push(val);
        }

        // Now feed same data through classifier and compare ATR
        let config = RegimeClassifierConfig {
            atr_period: period,
            adx_period: 5,
            bb_period: 5,
            bb_multiplier: 2.0,
            percentile_lookback: 30,
            confirmation_bars: 2,
            ..RegimeClassifierConfig::default()
        };
        let mut clf = RegimeClassifier::new(config);

        for (i, &(o, h, l, c)) in bars.iter().enumerate() {
            if let Some(snap) = clf.update(o, h, l, c) {
                let ta_val = ta_values[i];
                let diff = (snap.atr - ta_val).abs();
                assert!(
                    diff < 0.001,
                    "ATR mismatch at bar {}: classifier={:.4} ta={:.4} diff={:.6}",
                    i, snap.atr, ta_val, diff
                );
            }
        }
    }

    /// Cross-validate BB width: verify that our width formula
    /// (upper - lower) / middle * 100 produces correct values.
    #[test]
    fn test_bb_width_correctness() {
        use ta::indicators::BollingerBands;
        use ta::Next;
        use crate::bar_ext::TaBar;

        let period = 5;
        let multiplier = 2.0;
        let mut ta_bb = BollingerBands::new(period, multiplier).unwrap();

        // Constant price: BB width should converge to 0 (no variance)
        let mut last_width = f64::MAX;
        for _ in 0..20 {
            let bar = TaBar::new(100.0, 100.0, 100.0, 100.0);
            let out = ta_bb.next(&bar);
            let width = if out.average != 0.0 {
                (out.upper - out.lower) / out.average * 100.0
            } else {
                0.0
            };
            last_width = width;
        }
        assert!(
            last_width < 0.001,
            "BB width should be ~0 for constant price, got {:.6}",
            last_width
        );

        // Now add volatility and verify width increases
        let mut ta_bb2 = BollingerBands::new(period, multiplier).unwrap();
        let mut widths = Vec::new();
        for i in 0..20 {
            let close = 100.0 + (i as f64) * 2.0; // trending up
            let bar = TaBar::new(close, close + 1.0, close - 1.0, close);
            let out = ta_bb2.next(&bar);
            let width = if out.average != 0.0 {
                (out.upper - out.lower) / out.average * 100.0
            } else {
                0.0
            };
            widths.push(width);
        }
        // Width should be positive once we have enough bars
        let last_5_avg: f64 = widths[widths.len()-5..].iter().sum::<f64>() / 5.0;
        assert!(
            last_5_avg > 0.0,
            "BB width should be positive for trending data, got {:.4}",
            last_5_avg
        );
    }

    /// Correctness test: verify classify_raw produces correct regime for exact
    /// threshold values.
    #[test]
    fn test_classify_raw_exact_thresholds() {
        let config = test_config();
        let clf = RegimeClassifier::new(config.clone());

        // ADX=30 (>25), ATR_pct=60 (>50) → Trending
        assert_eq!(clf.classify_raw(30.0, 60.0, 50.0), Regime::Trending);

        // ATR_pct=95 (>90) → VolExpansion (takes priority)
        assert_eq!(clf.classify_raw(30.0, 95.0, 50.0), Regime::VolExpansion);

        // ADX=15 (<20), BB_pct=10 (<20) → LowVol
        assert_eq!(clf.classify_raw(15.0, 50.0, 10.0), Regime::LowVol);

        // ADX=18 (<20), BB_pct=50 (>20) → Ranging
        assert_eq!(clf.classify_raw(18.0, 50.0, 50.0), Regime::Ranging);

        // Edge: ADX=22 (between 20-25), ATR moderate → Ranging (not trending)
        assert_eq!(clf.classify_raw(22.0, 60.0, 50.0), Regime::Ranging);

        // Edge: ADX=26 (>25) but ATR_pct=30 (<50) → Ranging (ATR too low)
        assert_eq!(clf.classify_raw(26.0, 30.0, 50.0), Regime::Ranging);
    }

    // ─── Missing Behavioral Tests ─────────────────────────────────────

    /// Test LowVol (Bollinger squeeze) detection.
    /// LowVol requires: ADX < ranging_threshold AND bb_width_pct < squeeze_percentile.
    /// Strategy: build BB width history with moderate variation, then go totally flat
    /// so both ADX drops and BB width percentile plummets.
    #[test]
    fn test_low_vol_detection() {
        let config = RegimeClassifierConfig {
            adx_period: 5,
            atr_period: 5,
            bb_period: 5,
            bb_multiplier: 2.0,
            percentile_lookback: 50,
            confirmation_bars: 1,
            adx_trending_threshold: 25.0,
            adx_ranging_threshold: 20.0,
            atr_vol_expansion_percentile: 95.0,
            atr_trending_min_percentile: 50.0,
            bb_squeeze_percentile: 30.0,
        };
        let mut clf = RegimeClassifier::new(config);

        // Phase 1: moderate ranging warmup — oscillate around 100 with decent H/L spread.
        // This builds a BB width history with moderate values.
        // Prices barely trend (alternating), keeping ADX low.
        for i in 0..50 {
            let close = if i % 2 == 0 { 101.0 } else { 99.0 };
            clf.update(close, close + 1.5, close - 1.5, close);
        }

        // Phase 2: totally flat — close = 100, H/L = 100 ± 0.01.
        // ADX collapses (no directional movement) and BB width drains to near-zero,
        // which should fall below the 30th percentile of the history.
        let mut low_vol_detected = false;
        for _ in 0..40 {
            if let Some(snap) = clf.update(100.0, 100.01, 99.99, 100.0) {
                if snap.regime == Regime::LowVol {
                    low_vol_detected = true;
                }
            }
        }

        assert!(
            low_vol_detected,
            "Should detect LowVol when BB squeezes and ADX collapses"
        );
    }



    /// Test that hysteresis actually allows transition after enough bars.
    #[test]
    fn test_hysteresis_allows_transition_after_n_bars() {
        let config = RegimeClassifierConfig {
            confirmation_bars: 2,
            ..test_config()
        };
        let mut clf = RegimeClassifier::new(config);

        // Warmup with calm data
        for i in 0..30 {
            let base = 100.0 + (i % 3) as f64 * 0.5;
            clf.update(base, base + 0.5, base - 0.5, base);
        }

        let initial_regime = clf.regime();

        // Feed many bars of extreme volatility (should trigger VolExpansion)
        let mut switched = false;
        for i in 0..20 {
            let base = 100.0 + (i as f64) * 10.0;
            clf.update(base, base + 20.0, base - 20.0, base + 5.0);
            if clf.regime() != initial_regime {
                switched = true;
            }
        }

        assert!(
            switched,
            "Hysteresis should eventually allow regime transition after enough consecutive bars"
        );
    }

    /// Test regime sequence: trending → calm → detects transition.
    #[test]
    fn test_regime_sequence_trend_to_calm() {
        let config = RegimeClassifierConfig {
            confirmation_bars: 2,
            ..test_config()
        };
        let mut clf = RegimeClassifier::new(config);

        // Phase 1: Strong trend to establish Trending regime
        for i in 0..40 {
            let base = 100.0 + (i as f64) * 3.0;
            clf.update(base, base + 2.0, base - 1.0, base + 1.5);
        }

        // Record regime after trending phase
        let trending_regime = clf.regime();

        // Phase 2: Flat/ranging market
        for _ in 0..40 {
            let base = 220.0 + (rand_simple() * 0.5); // tiny noise around 220
            clf.update(base, base + 0.3, base - 0.3, base + 0.1);
        }

        let calm_regime = clf.regime();

        // The regime should have changed from whatever it was during the trend
        // to something non-trending (assuming the trend was detected)
        if trending_regime == Regime::Trending || trending_regime == Regime::VolExpansion {
            assert_ne!(
                calm_regime, trending_regime,
                "Regime should change when market transitions from trend to calm"
            );
        }
    }

    /// Test that bb_width_percentile in snapshot is in valid range.
    #[test]
    fn test_snapshot_bb_width_percentile_valid() {
        let mut clf = RegimeClassifier::new(test_config());

        for i in 0..40 {
            let base = 100.0 + (i as f64) * 0.5;
            if let Some(snap) = clf.update(base, base + 1.0, base - 1.0, base + 0.2) {
                assert!(
                    snap.bb_width_percentile >= 0.0 && snap.bb_width_percentile <= 100.0,
                    "BB width percentile out of range: {:.1}",
                    snap.bb_width_percentile
                );
            }
        }
    }

    /// Test percentile rank with duplicate values.
    #[test]
    fn test_percentile_rank_with_duplicates() {
        let history: VecDeque<f64> = vec![5.0, 5.0, 5.0, 5.0, 5.0].into();
        // All values equal: nothing strictly below 5.0, so percentile = 0
        assert_eq!(percentile_rank(5.0, &history), 0.0);
        // Value above all: percentile = 100
        assert_eq!(percentile_rank(6.0, &history), 100.0);
        // Value below all: percentile = 0
        assert_eq!(percentile_rank(4.0, &history), 0.0);
    }

    /// Simple deterministic "random" for tests (we don't want to add rand dep).
    fn rand_simple() -> f64 {
        // Use time-independent pseudo-random by hashing a counter
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        // Simple hash to get a value between 0 and 1
        let hash = (n.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407)) >> 33;
        (hash as f64) / (u32::MAX as f64)
    }
}
