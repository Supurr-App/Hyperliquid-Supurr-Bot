//! Bridge between our `Bar` struct and the `ta` crate's trait requirements.
//!
//! The `ta` crate's indicators use `Next<&T>` where T implements various
//! accessor traits (High, Low, Close). This module provides a lightweight
//! wrapper that implements those traits for OHLCV data.

/// OHLCV bar data compatible with the `ta` crate's trait requirements.
///
/// This is a simple struct that implements `ta::High`, `ta::Low`, `ta::Close`
/// so it can be fed directly to `ta` indicators like `AverageTrueRange`
/// and `BollingerBands`.
#[derive(Debug, Clone)]
pub struct TaBar {
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
}

impl TaBar {
    pub fn new(open: f64, high: f64, low: f64, close: f64) -> Self {
        Self {
            open,
            high,
            low,
            close,
        }
    }
}

impl ta::Open for TaBar {
    fn open(&self) -> f64 {
        self.open
    }
}

impl ta::High for TaBar {
    fn high(&self) -> f64 {
        self.high
    }
}

impl ta::Low for TaBar {
    fn low(&self) -> f64 {
        self.low
    }
}

impl ta::Close for TaBar {
    fn close(&self) -> f64 {
        self.close
    }
}

impl ta::Volume for TaBar {
    fn volume(&self) -> f64 {
        0.0 // We don't track volume
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ta_bar_traits() {
        let bar = TaBar::new(100.0, 110.0, 95.0, 105.0);
        assert_eq!(ta::Open::open(&bar), 100.0);
        assert_eq!(ta::High::high(&bar), 110.0);
        assert_eq!(ta::Low::low(&bar), 95.0);
        assert_eq!(ta::Close::close(&bar), 105.0);
        assert_eq!(ta::Volume::volume(&bar), 0.0);
    }
}
