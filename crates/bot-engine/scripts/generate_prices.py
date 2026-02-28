#!/usr/bin/env python3
"""
Generate synthetic price data with realistic movements for WASM stress testing.
Uses Geometric Brownian Motion (GBM) for realistic price simulation.
"""

import json
import numpy as np
import matplotlib.pyplot as plt
from datetime import datetime
import os

# Configuration
NUM_POINTS = 1_000_000  # 1 million points (adjust as needed)
START_PRICE = 90000.0  # Bitcoin-like starting price
VOLATILITY = 0.00002   # Per-tick volatility (~3-5% range over 1M ticks)
DRIFT = 0.0           # No drift (mean-reverting-ish)
START_TIMESTAMP = int(datetime.now().timestamp() * 1000)  # Current time in ms
TIME_INTERVAL_MS = 1000  # 1 second between ticks

OUTPUT_FILE = "prices.json"


def generate_gbm_prices(n: int, start: float, vol: float, drift: float) -> np.ndarray:
    """Generate prices using Geometric Brownian Motion."""
    print(f"Generating {n:,} price points using GBM...")
    
    # Generate random returns
    np.random.seed(42)  # Reproducible results
    random_shocks = np.random.standard_normal(n)
    
    # GBM formula: S(t+1) = S(t) * exp((drift - vol^2/2) + vol * Z)
    log_returns = (drift - 0.5 * vol ** 2) + vol * random_shocks
    
    # Cumulative sum to get log prices, then exponentiate
    log_prices = np.log(start) + np.cumsum(log_returns)
    prices = np.exp(log_prices)
    
    return prices


def main():
    print("=" * 60)
    print("WASM Backtest Stress Test - Price Generator")
    print("=" * 60)
    
    # Generate prices
    prices = generate_gbm_prices(NUM_POINTS, START_PRICE, VOLATILITY, DRIFT)
    
    # Generate timestamps
    timestamps = START_TIMESTAMP + np.arange(NUM_POINTS) * TIME_INTERVAL_MS
    
    # Stats
    min_price = prices.min()
    max_price = prices.max()
    price_range = max_price - min_price
    
    print(f"\nPrice Statistics:")
    print(f"  Points:     {NUM_POINTS:,}")
    print(f"  Start:      ${START_PRICE:,.2f}")
    print(f"  Min:        ${min_price:,.2f}")
    print(f"  Max:        ${max_price:,.2f}")
    print(f"  Range:      ${price_range:,.2f} ({price_range/START_PRICE*100:.2f}%)")
    print(f"  Final:      ${prices[-1]:,.2f}")
    
    # Create output format matching WASM expectation
    # Format: { ts_ms: number, price: string }
    print(f"\nBuilding JSON array...")
    data = [
        {"ts_ms": int(ts), "price": f"{p:.6f}"}
        for ts, p in zip(timestamps, prices)
    ]
    
    # Save to file
    output_path = os.path.join(os.path.dirname(__file__), OUTPUT_FILE)
    print(f"Saving to {output_path}...")
    
    with open(output_path, 'w') as f:
        json.dump(data, f)
    
    file_size = os.path.getsize(output_path)
    print(f"File size: {file_size / 1_000_000:.1f} MB")
    
    # Plot a sample of the data
    print(f"\nPlotting price movement (sampled)...")
    
    # Sample every 1000th point for plotting
    sample_rate = max(1, NUM_POINTS // 10000)
    sample_idx = np.arange(0, NUM_POINTS, sample_rate)
    sample_prices = prices[sample_idx]
    sample_times = np.arange(len(sample_idx))
    
    fig, (ax1, ax2) = plt.subplots(2, 1, figsize=(14, 8))
    
    # Price chart
    ax1.plot(sample_times, sample_prices, 'b-', linewidth=0.5)
    ax1.axhline(y=min_price, color='g', linestyle='--', alpha=0.5, label=f'Min ${min_price:,.0f}')
    ax1.axhline(y=max_price, color='r', linestyle='--', alpha=0.5, label=f'Max ${max_price:,.0f}')
    ax1.set_title(f'Synthetic BTC Price - {NUM_POINTS:,} Points (GBM Model)')
    ax1.set_xlabel(f'Sample Index (1/{sample_rate} of data)')
    ax1.set_ylabel('Price ($)')
    ax1.legend()
    ax1.grid(True, alpha=0.3)
    
    # Returns distribution
    returns = np.diff(np.log(prices))
    ax2.hist(returns, bins=100, density=True, alpha=0.7, color='blue')
    ax2.set_title('Log Returns Distribution')
    ax2.set_xlabel('Log Return')
    ax2.set_ylabel('Density')
    ax2.grid(True, alpha=0.3)
    
    plt.tight_layout()
    
    plot_path = os.path.join(os.path.dirname(__file__), "prices_chart.png")
    plt.savefig(plot_path, dpi=150)
    print(f"Chart saved to {plot_path}")
    
    plt.show()
    
    print("\n" + "=" * 60)
    print("Done! Now update index.html to load from prices.json")
    print("=" * 60)


if __name__ == "__main__":
    main()
