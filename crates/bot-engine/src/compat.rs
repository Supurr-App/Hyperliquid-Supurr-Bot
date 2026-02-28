//! Platform compatibility layer for WASM and native targets.
//!
//! Provides unified APIs for spawn and sleep that work on both:
//! - Native (tokio runtime) - enabled by `native` feature
//! - WASM (browser event loop via wasm-bindgen-futures) - enabled by `wasm` feature
//!
//! Note: When both features are enabled, native takes precedence.

use std::future::Future;
use std::time::Duration;

/// Spawn an async task.
/// - Native: Uses tokio::spawn (multi-threaded)
/// - WASM: Uses wasm_bindgen_futures::spawn_local (single-threaded)
#[cfg(feature = "native")]
pub fn spawn<F>(fut: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(fut);
}

#[cfg(all(feature = "wasm", not(feature = "native")))]
pub fn spawn<F>(fut: F)
where
    F: Future<Output = ()> + 'static,
{
    wasm_bindgen_futures::spawn_local(fut);
}

/// Async sleep.
/// - Native: Uses tokio::time::sleep
/// - WASM: Uses gloo_timers::future::TimeoutFuture
#[cfg(feature = "native")]
pub async fn sleep(duration: Duration) {
    tokio::time::sleep(duration).await;
}

#[cfg(all(feature = "wasm", not(feature = "native")))]
pub async fn sleep(duration: Duration) {
    gloo_timers::future::TimeoutFuture::new(duration.as_millis() as u32).await;
}

/// No-op sleep for backtesting (instant return).
/// Use this in backtest mode to skip real delays.
pub async fn sleep_noop(_duration: Duration) {
    // Instant return - no actual delay
}

/// Get current timestamp in milliseconds.
/// - Native: Uses std::time
/// - WASM: Uses js_sys::Date
#[cfg(feature = "native")]
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[cfg(all(feature = "wasm", not(feature = "native")))]
pub fn now_ms() -> i64 {
    js_sys::Date::now() as i64
}

/// Yield to the event loop without a timer delay.
/// - Native: Uses tokio::task::yield_now (cooperative scheduling)
/// - WASM: Uses Promise.resolve() microtask (much faster than setTimeout)
///
/// This is crucial for backtesting - allows browser to stay responsive
/// without the ~4ms+ overhead of setTimeout timers.
#[cfg(feature = "native")]
pub async fn yield_now() {
    tokio::task::yield_now().await;
}

#[cfg(all(feature = "wasm", not(feature = "native")))]
pub async fn yield_now() {
    // Create a resolved promise - this schedules a microtask which is MUCH faster
    // than setTimeout (which has minimum ~4ms in browsers)
    use wasm_bindgen::JsValue;
    let promise = js_sys::Promise::resolve(&JsValue::NULL);
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}
