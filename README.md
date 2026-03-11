<p align="center">
  <img src="https://supurr.app/favicon.ico" width="100" />
</p>

<h1 align="center">Supurr</h1>

<p align="center">
  <strong>The Open-Source Trading Bot Engine for Hyperliquid</strong><br/>
  Built in Rust. 24K LOC. 9 crates. 69 tests. Zero unsafe code.<br/>
  One binary. Backtest to production in minutes. Strategies that trade for you.
</p>

<p align="center">
  <a href="#what-is-supurr">What is Supurr?</a> •
  <a href="#why-supurr">Why Supurr?</a> •
  <a href="#strategies">Strategies</a> •
  <a href="#quick-start">Quick Start</a> •
  <a href="#ai-powered">AI-Powered</a> •
  <a href="#architecture">Architecture</a>
</p>

<p align="center">
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/Rust-000000?style=flat&logo=rust&logoColor=white" alt="Rust" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT" /></a>
  <img src="https://img.shields.io/badge/version-0.1.0-orange" alt="v0.1.0" />
  <img src="https://img.shields.io/badge/tests-69-green" alt="Tests" />
  <img src="https://img.shields.io/badge/crates-9-purple" alt="Crates" />
  <a href="https://hyperliquid.xyz"><img src="https://img.shields.io/badge/exchange-Hyperliquid-1DB954" alt="Hyperliquid" /></a>
</p>

---

## What is Supurr?

Supurr is an open-source **trading bot engine** purpose-built for [Hyperliquid](https://hyperliquid.xyz) — the highest-performance on-chain perpetuals DEX. It is **not** a signal dashboard, **not** a copy-trading wrapper, **not** a Python script calling an API. It is a full **trading engine** built from scratch in Rust.

Traditional trading bots force you to choose: performance **or** flexibility. Python bots are easy to write but choke under load. Rust gives you both — zero-cost abstractions, deterministic execution, and compile-time safety.

The entire system compiles to a **single native binary**. One install. One config file. Your bot is live.

```bash
# Install CLI
curl -fsSL https://cli.supurr.app/install | bash

# Generate a config, backtest it, deploy it
supurr new grid --asset BTC --levels 10
supurr backtest -c config.json -s 2026-01-01 -e 2026-02-01
supurr deploy -c config.json
```

### Why Supurr?

There are mature open-source trading frameworks out there — [NautilusTrader](https://github.com/nautechsystems/nautilus_trader) and [Hummingbot](https://github.com/hummingbot/hummingbot) being the two heavyweights. They're excellent projects. But they were designed for a different world: multi-exchange, multi-asset, institutional-grade complexity.

Supurr is the opposite bet. **One exchange. One binary. Zero bloat.**

|                              | **Supurr**                            | **NautilusTrader**                  | **Hummingbot**                 |
| ---------------------------- | ------------------------------------- | ----------------------------------- | ------------------------------ |
| **RAM (idle)**               | **~15 MB**                            | ~500 MB minimum                     | ~1 GB per instance             |
| **Live Paper Trading**       | ✅ Real DEX prices, simulated fills   | ✅ Sandbox (live data + sim fills)  | ⚠️ Simulated orderbook         |
| **Hyperliquid Native**       | ✅ First-class — Perp, Spot, HIP-3    | ❌ No adapter                       | ❌ Community connector         |
| **Network Upgrade Handling** | ✅ HTTP-only + health-gating          | ❌ Generic retry                    | ⚠️ WS reconnect issues         |
| **Backtest ↔ Live Parity**   | ✅ Identical code path                | ✅ Identical code path              | ⚠️ Different connectors        |
| **AI-Native**                | ✅ Natural language bot ops           | ❌                                  | ❌                             |
| **Deployment**               | Single binary, zero deps              | pip + virtualenv + Rust toolchain   | pip + Docker + Gateway         |
| **Time to First Bot**        | **~3 minutes**                        | ~30+ min (env setup)                | ~30+ min (Docker + config)     |
| **Poll Interval**            | 500 ms                                | Event-driven (sub-ms core)          | 1 s default tick               |
| **Type Safety**              | Compile-time (Rust ownership)         | Hybrid (Rust core + Python surface) | Runtime (Python)               |
| **Concurrency**              | Native async, zero-cost futures       | Rust core + Python GIL              | Async (GIL-bounded)            |
| **Strategy Safety**          | Ownership model prevents shared state | Python classes (mutable state)      | Python classes (mutable state) |
| **Language**                 | Pure Rust                             | Python + Rust + Cython              | Python + Cython                |
| **Browser / WASM**           | ✅ Engine compiles to WASM            | ❌ Python runtime required          | ❌ Python runtime required     |
| **Codebase**                 | 24K LOC, 9 crates                     | 100K+ LOC, multi-language           | 100K+ LOC, 40+ connectors      |

#### 🛡️ Reliability-First Execution on Hyperliquid

Most trading bots depend on WebSocket connections for real-time data. When Hyperliquid pushes a network upgrade or has a transient hiccup, those WS connections drop — and so does your bot. Hummingbot's Docker deployment has [known issues](https://github.com/hummingbot/hummingbot/issues) where it fails to transition between connection states after temporary disconnects.

Supurr's Hyperliquid adapter is **HTTP-only** — no WebSocket dependency means no WS disconnect edge cases. The engine runner includes **backoff and health-gating** so transient failures don't cascade into missed trades or orphaned orders. When Hyperliquid upgrades, your bot waits, recovers, and resumes. No manual restart required.

> Fewer "my bot died on a transient HL hiccup" incidents.

#### 🔄 Same Strategy Code: Live = Paper = Backtest

The #1 cause of "works in sim, breaks live" is that sim and live use different code paths. Supurr eliminates this entirely — your strategy runs against a unified `Exchange` trait across all three modes:

```
Live:      HyperliquidClient        → Arc<dyn Exchange>   (real everything)
Paper:     PaperExchange<HLClient>  → Arc<dyn Exchange>   (real HL quotes, sim fills)
Backtest:  PaperExchange<standalone>→ Arc<dyn Exchange>   (replay queued ticks, sim fills)
```

Paper mode streams **real-time prices from Hyperliquid's `allMids` API** — not a fake orderbook, not historical replays. Your strategy sees the actual market, it just can't lose real money. When you're confident, flip one flag and it runs live. **Zero code changes.**

> Other frameworks simulate the market. Supurr shows you the real one.

#### ⚡ Lower Setup, Faster Shipping

| Step                  | Supurr                              | Others                                        |
| --------------------- | ----------------------------------- | --------------------------------------------- |
| **Install**           | `curl` one binary                   | Docker + pip + virtualenv + Gateway + configs |
| **Configure**         | Single JSON with generated schema   | Multiple YAML/config files + env vars         |
| **Iterate**           | `new → backtest → deploy → monitor` | Manual config → run → check logs → redeploy   |
| **Time to first bot** | **~3 minutes**                      | 30+ minutes of setup                          |

The Supurr CLI + [AI Skill](https://github.com/Supurr-App/Hyperliquid-Supurr-Skill) give you a complete workflow — config generation, backtesting, cloud deployment, and live monitoring — in one tool. Talk to your bot in English. Works with 20+ AI coding agents.

```bash
supurr new grid --asset BTC --levels 10     # generate config
supurr backtest -c config.json              # test against history
supurr deploy -c config.json                # push to production
supurr monitor --watch                      # live dashboard
```

#### 🧠 10x Less Memory

Hummingbot's [official docs](https://hummingbot.org) recommend **4 GB RAM per instance** — macOS users have [reported 30 GB+ on startup](https://github.com/hummingbot/hummingbot/issues). NautilusTrader recommends **8 GB minimum**. Supurr idles at **~15 MB** — no Python runtime, no garbage collector, no pandas, no numpy. Just compiled Rust.

> Run **dozens of Supurr instances** on the same machine that struggles with **one** Hummingbot.

#### 🌐 Runs in the Browser — WASM Target

Supurr's engine compiles to **WebAssembly**, enabling backtesting and strategy simulation directly in the browser — no server, no install, no Rust toolchain needed. The same Rust code that runs on your server runs in a browser tab.

Python-based frameworks are fundamentally blocked here — CPython can't compile to WASM. Rust's zero-runtime design makes the entire engine portable to `wasm32-unknown-unknown` with feature flags swapping out OS-specific dependencies (tokio → browser event loop, filesystem → in-memory).

> Your users can backtest strategies in a web UI without downloading anything.

---

## Strategies

5 production-ready strategies + a trait for building your own. Every strategy works across **Perps**, **Spot**, and **HIP-3** sub-DEX markets.

| Strategy                | What It Actually Does                                                                                                                    | Markets           |
| ----------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- | ----------------- |
| **Grid**                | Places buy/sell limit orders at fixed price intervals. Captures profit from range-bound markets. Auto-rebalances when levels are filled. | Perp, Spot, HIP-3 |
| **DCA**                 | Dollar-cost averages into positions on a schedule. Configurable intervals, position scaling, and exit targets. Set it and forget it.     | Perp, Spot, HIP-3 |
| **Market Maker**        | Maintains a two-sided order book with dynamic spreads. Inventory skew management prevents directional exposure from blowing up.          | Perp, Spot, HIP-3 |
| **Spot-Perp Arbitrage** | Detects and exploits price divergence between spot and perpetual markets. Delta-neutral by construction.                                 | Perp + Spot       |
| **Tick Trader**         | High-frequency tick-level strategy for capturing micro price movements. Sub-millisecond decision loop.                                   | Perp              |
| **Custom (yours!)**     | Implement the `Strategy` trait in Rust → build literally any trading logic you can imagine.                                              | Any               |

```rust
// Your strategy is a deterministic state machine.
// It receives events; it emits commands. That's it.
pub trait Strategy: Send + 'static {
    fn id(&self) -> &StrategyId;
    fn on_start(&mut self, ctx: &mut dyn StrategyContext);
    fn on_event(&mut self, ctx: &mut dyn StrategyContext, event: &Event);
    fn on_timer(&mut self, ctx: &mut dyn StrategyContext, timer_id: TimerId);
    fn on_stop(&mut self, ctx: &mut dyn StrategyContext);
}
```

Strategies **never call HTTP directly**. They receive market data (quotes, fills, timers) and emit commands (place/cancel orders) through the `StrategyContext`. The engine handles execution. This separation is what makes backtesting identical to live trading.

```bash
# Scaffold a new strategy crate with full boilerplate
supurr dev init
```

---

## Execution Modes

One engine. Four modes. **Strategies can't tell the difference** — they run against a unified `Exchange` trait:

```
Live:      HyperliquidClient           → Arc<dyn Exchange>       (real everything)
Paper:     PaperExchange<HLClient>     → Arc<dyn Exchange>       (real quotes, sim fills)
Backtest:  PaperExchange<standalone>   → Arc<dyn Exchange>       (queued quotes, sim fills)
```

| Mode         | Quotes               | Fills              | Latency | Use Case                           |
| ------------ | -------------------- | ------------------ | ------- | ---------------------------------- |
| **Live**     | Real (allMids API)   | Real (Hyperliquid) | 500ms   | Production trading                 |
| **Paper**    | Real (allMids API)   | Simulated locally  | 500ms   | Risk-free testing with live prices |
| **Backtest** | Pre-queued from JSON | Simulated locally  | 0ms     | Historical strategy evaluation     |
| **Dry Run**  | —                    | —                  | —       | Config validation only             |

```bash
# Paper trade with live prices, zero risk
cargo run --release --bin bot -- --config config.json --mode paper

# Backtest a full month via Supurr CLI
supurr backtest -c config.json -s 2026-01-01 -e 2026-02-01
```

---

## Quick Start

### Prerequisites

- [Rust](https://rustup.rs/) (1.75+)
- A Hyperliquid account with an API wallet

### 3 Minutes to First Bot

```bash
# 1. Clone
git clone https://github.com/Supurr-App/bot-base.git && cd bot-base

# 2. Build
cargo build --release

# 3. Validate your config (no orders placed)
cargo run --release --bin bot -- --config config.json --dry-run

# 4. Go live
cargo run --release --bin bot -- --config config.json
```

### Configuration

Every strategy reads from a JSON config:

```json
{
  "strategy_id": "my-grid",
  "environment": "Mainnet",
  "market": {
    "exchange": "hyperliquid",
    "type": "perp",
    "base": "BTC",
    "index": 0
  },
  "grid": {
    "start_price": "88000",
    "end_price": "92000",
    "levels": 4,
    "order_size": "0.001"
  }
}
```

> Don't write configs by hand — use `supurr new grid --asset BTC` to generate them interactively.

---

## AI-Powered

### 🐱 Supurr Skill — Talk to Your Bot in English

The [**Hyperliquid Supurr Skill**](https://github.com/Supurr-App/Hyperliquid-Supurr-Skill) turns your AI coding assistant into a trading bot operator. Config generation, backtesting, deployment, monitoring — all through natural language.

```bash
# Install the skill (works with Claude Code, Cursor, Antigravity, and more)
npx skills add Supurr-App/Hyperliquid-Supurr-Skill
```

Then just talk:

> _"Create a grid bot for ETH with 10 levels between $3000–$3500, backtest it on last week's data, and deploy if PnL is positive"_

The AI handles the full workflow:

```bash
supurr init                                    # Setup credentials
supurr new grid --asset BTC --levels 4         # Generate config
supurr backtest -c config.json -s 2026-01-28   # Backtest
supurr deploy -c config.json                   # Deploy to cloud
supurr monitor --watch                         # Live monitoring
supurr inspect 302                             # Deep dive into bot
```

---

## Architecture

9 Rust crates. 24,000 lines of code. Modular engine design.

```
                          ┌─────────────────────────────────────────┐
                          │             bot-cli (launcher)          │
                          │  parse config → init exchange/strategy  │
                          └──────────────────┬──────────────────────┘
                                             │
                          ┌──────────────────▼──────────────────────┐
                          │           bot-engine (runtime)          │
                          │                                        │
                          │  ┌────────────┐    ┌────────────────┐  │
                          │  │  Engine     │    │  EngineRunner  │  │
                          │  │  events →   │◄───│  main poll     │  │
                          │  │  strategy → │    │  loop (500ms)  │  │
                          │  │  commands   │    └────────────────┘  │
                          │  └────────────┘                        │
                          │                                        │
                          │  ┌────────────────┐  ┌──────────────┐  │
                          │  │ OrderManager   │  │ TradeSyncer  │  │
                          │  │ in-memory SOT  │  │ fills → API  │  │
                          │  └────────────────┘  └──────────────┘  │
                          └──────────────────┬──────────────────────┘
                                             │
              ┌──────────────────────────────┼──────────────────────────────┐
              │                              │                              │
   ┌──────────▼──────────┐     ┌─────────────▼────────────┐    ┌───────────▼──────────┐
   │  HyperliquidClient  │     │     PaperExchange        │    │    bot-core          │
   │  (live exchange)    │     │  (paper / backtest)       │    │  types, events,      │
   │  real orders/fills  │     │  simulated fills          │    │  commands, traits     │
   └─────────────────────┘     └──────────────────────────┘    └──────────────────────┘
```

### Crate Map

| Crate                   | Role                                                                       |
| ----------------------- | -------------------------------------------------------------------------- |
| `bot-core`              | Exchange-agnostic domain model — types, events, commands, `Strategy` trait |
| `bot-engine`            | Runtime: order manager, inventory ledger, event routing, polling loop      |
| `exchange-hyperliquid`  | Hyperliquid exchange adapter (REST + WebSocket)                            |
| `bot-cli`               | Binary entry point — config parsing, signal handling, launcher             |
| `strategy-grid`         | Grid trading strategy                                                      |
| `strategy-dca`          | Dollar-cost averaging strategy                                             |
| `strategy-market-maker` | Spread-based market making                                                 |
| `strategy-arbitrage`    | Spot-perp arbitrage                                                        |
| `strategy-tick-trader`  | High-frequency tick trading                                                |

---

## Cross-Platform Builds

The engine compiles to native binaries for all major platforms:

| Platform       | Method               | Output                      |
| -------------- | -------------------- | --------------------------- |
| `darwin-arm64` | Native               | `releases/bot-darwin-arm64` |
| `darwin-x64`   | Native cross-compile | `releases/bot-darwin-x64`   |
| `linux-x64`    | Docker (amd64)       | `releases/bot-linux-x64`    |
| `linux-arm64`  | Docker (arm64)       | `releases/bot-linux-arm64`  |

---

## Project Structure

```
bot/
├── crates/
│   ├── bot-cli/                # Binary entry point
│   ├── bot-core/               # Domain types, Strategy trait, Exchange trait
│   ├── bot-engine/             # Runtime (engine, runner, order manager, paper exchange)
│   ├── exchange-hyperliquid/   # Hyperliquid REST + WS adapter
│   ├── strategy-grid/          # Grid bot
│   ├── strategy-dca/           # DCA bot
│   ├── strategy-market-maker/  # Market maker
│   ├── strategy-arbitrage/     # Spot-perp arb
│   └── strategy-tick-trader/   # Tick trader
├── releases/                   # Cross-platform binaries
└── schemas/                    # Auto-generated JSON schemas from Rust types
```

---

## Development

```bash
# Build the workspace
cargo build --workspace --lib

# Run all tests
cargo test --workspace

# Lint (must be 0 warnings)
cargo clippy --workspace --all-targets -- -D warnings

# Format
cargo fmt --all -- --check
```

---

## License

MIT — use it however you want.

---

<p align="center">
  <strong>Built with Rust. Purpose-built for Hyperliquid. Strategies that trade for you.</strong><br/><br/>
  <a href="https://supurr.app">Website</a> •
  <a href="https://github.com/Supurr-App/bot-base">GitHub</a> •
  <a href="https://github.com/Supurr-App/Hyperliquid-Supurr-Skill">AI Skill</a>
</p>
