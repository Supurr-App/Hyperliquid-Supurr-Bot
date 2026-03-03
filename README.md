<p align="center">
  <img src="https://supurr.app/favicon.ico" width="80" />
</p>

<h1 align="center">Supurr Bot Engine</h1>

<p align="center">
  <strong>High-performance trading bot engine for <a href="https://hyperliquid.xyz">Hyperliquid</a></strong><br/>
  Build, backtest, and deploy automated strategies across Perps, Spot, and HIP-3 markets
</p>

<p align="center">
  <a href="#strategies">Strategies</a> •
  <a href="#architecture">Architecture</a> •
  <a href="#quick-start">Quick Start</a> •
  <a href="#execution-modes">Modes</a> •
  <a href="#ai-powered">AI-Powered</a>
</p>

---

## Strategies

| Strategy                | Description                                                            | Markets           |
| ----------------------- | ---------------------------------------------------------------------- | ----------------- |
| **Grid**                | Places buy/sell limit orders at fixed intervals around a price range   | Perp, Spot, HIP-3 |
| **DCA**                 | Dollar-cost averaging with configurable intervals and position scaling | Perp, Spot, HIP-3 |
| **Market Maker**        | Spread-based quoting with inventory skew management                    | Perp, Spot, HIP-3 |
| **Spot-Perp Arbitrage** | Exploits price divergence between spot and perpetual markets           | Perp + Spot       |
| **Tick Trader**         | High-frequency tick-level strategy                                     | Perp              |
| **Custom (yours!)**     | Implement the `Strategy` trait in Rust → build any logic you want      | Any               |

---

## Architecture

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

## Quick Start

### Prerequisites

- [Rust](https://rustup.rs/) (1.75+)
- A Hyperliquid account with API wallet

### Build & Run

```bash
# Clone
git clone https://github.com/Supurr-App/bot-base.git
cd bot-base

# Build (release mode for trading)
cargo build --release

# Dry run — validate config without placing orders
cargo run --release --bin bot -- --config config.json --dry-run

# Live trading
cargo run --release --bin bot -- --config config.json
```

### Configuration

Every strategy reads from a JSON config. Example for Grid:

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

> Use `supurr new grid --asset BTC` to auto-generate configs interactively.

---

## Execution Modes

| Mode         | Quotes               | Fills              | Delay | Use Case                           |
| ------------ | -------------------- | ------------------ | ----- | ---------------------------------- |
| **Live**     | Real (allMids API)   | Real (Hyperliquid) | 500ms | Production trading                 |
| **Paper**    | Real (allMids API)   | Simulated locally  | 500ms | Risk-free testing with live prices |
| **Backtest** | Pre-queued from JSON | Simulated locally  | 0ms   | Historical strategy evaluation     |
| **Dry Run**  | —                    | —                  | —     | Config validation only             |

```bash
# Paper trading
cargo run --release --bin bot -- --config config.json --mode paper

# Backtest (via Supurr CLI — fetches prices, runs engine)
supurr backtest -c config.json -s 2026-01-01 -e 2026-02-01
```

The engine uses a unified `Exchange` trait — strategies can't tell whether they're running live or simulated:

```
Live:      HyperliquidClient           → Arc<dyn Exchange>
Paper:     PaperExchange<HLClient>     → Arc<dyn Exchange>  (real quotes, sim fills)
Backtest:  PaperExchange<standalone>   → Arc<dyn Exchange>  (queued quotes, sim fills)
```

---

## AI-Powered

### 🐱 Supurr Skill (Agentic Integration)

The [**Hyperliquid Supurr Skill**](https://github.com/Supurr-App/Hyperliquid-Supurr-Skill) lets AI coding assistants operate the full bot lifecycle — from config generation to deployment and monitoring.

```bash
# Install the skill (works with Claude Code, Cursor, Antigravity, and more)
npx skills add Supurr-App/Hyperliquid-Supurr-Skill
```

Once installed, you can talk to your AI assistant naturally:

> _"Create a grid bot for ETH with 10 levels between $3000-$3500, backtest it on last week's data, and deploy if PnL is positive"_

The skill teaches the AI to use the **Supurr CLI**:

```bash
# Install CLI
curl -fsSL https://cli.supurr.app/install | bash

# Full workflow
supurr init                                    # Setup credentials
supurr new grid --asset BTC --levels 4         # Generate config
supurr backtest -c config.json -s 2026-01-28   # Backtest
supurr deploy -c config.json                   # Deploy to cloud
supurr monitor --watch                         # Live monitoring
supurr inspect 302                             # Deep dive into bot
```

### Write Custom Strategies

Implement the `Strategy` trait to build any trading logic:

```rust
pub trait Strategy: Send + 'static {
    fn id(&self) -> &StrategyId;
    fn on_start(&mut self, ctx: &mut dyn StrategyContext);
    fn on_event(&mut self, ctx: &mut dyn StrategyContext, event: &Event);
    fn on_timer(&mut self, ctx: &mut dyn StrategyContext, timer_id: TimerId);
    fn on_stop(&mut self, ctx: &mut dyn StrategyContext);
}
```

Strategies are **deterministic state machines** — they receive events (quotes, fills, timers) and emit commands (place/cancel orders) through the `StrategyContext`. They never call HTTP directly.

```bash
# Scaffold a new strategy crate
supurr dev init
```

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

## License

MIT
