/**
 * Web Worker for WASM Grid Backtest
 * Runs the heavy computation on a separate thread, keeping main thread free.
 */

// Import WASM module
import init, { run_grid_backtest, get_version } from './pkg/bot_engine.js';

let wasmReady = false;

// Initialize WASM when worker starts
async function initWasm() {
  if (wasmReady) return;
  
  try {
    await init();
    wasmReady = true;
    self.postMessage({ type: 'ready', version: get_version() });
  } catch (e) {
    self.postMessage({ type: 'error', message: 'Failed to initialize WASM: ' + e.message });
  }
}

// Handle messages from main thread
self.onmessage = async function(e) {
  const { type, prices, config, id } = e.data;
  
  if (type === 'init') {
    await initWasm();
    return;
  }
  
  if (type === 'backtest') {
    if (!wasmReady) {
      await initWasm();
    }
    
    try {
      const startMs = performance.now();
      
      // Run the backtest
      const pricesJson = JSON.stringify(prices);
      const configJson = JSON.stringify(config);
      
      const resultJson = await run_grid_backtest(pricesJson, configJson);
      const result = JSON.parse(resultJson);
      
      const endMs = performance.now();
      const execTime = endMs - startMs;
      
      self.postMessage({
        type: 'result',
        id,
        result,
        execTime,
        quotesCount: prices.length,
      });
    } catch (e) {
      self.postMessage({
        type: 'error',
        id,
        message: e.message || e.toString(),
      });
    }
  }
};

// Auto-init on worker load
initWasm();
