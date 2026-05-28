# Hyperliquid Trading Bot

Command-line runner for Supurr Hyperliquid trading bots.

This package installs the `bot` binary and exposes configuration schema types for tooling. The trading runtime is provided by `bot-engine`; this crate wires config, exchange setup, simulation mode, strategy creation, and process shutdown into one executable.

## Install

```bash
cargo install hyperliquid-trading-bot
```

Run:

```bash
bot --help
```

## What It Runs

| Mode | Quotes | Fills | Use case |
| --- | --- | --- | --- |
| `live` | Hyperliquid live prices | Real exchange orders | Production trading |
| `paper` | Hyperliquid live prices | Local simulated fills | Risk-free live-market testing |
| backtest via `--prices` | Historical JSON prices | Local simulated fills | Strategy replay |
| `--dry-run` | none | none | Config validation |

```bash
bot --config config.json --dry-run
bot --config config.json --mode paper
bot --config config.json
bot --config config.json --prices prices.json
```

## Minimal Perp Grid Config

```json
{
  "environment": "testnet",
  "private_key": "0x...",
  "address": "0x...",
  "strategy_type": "grid",
  "markets": [
    {
      "exchange": "hyperliquid",
      "type": "perp",
      "base": "BTC",
      "quote": "USDC",
      "index": 0
    }
  ],
  "grid": {
    "mode": "neutral",
    "start_price": "88000",
    "end_price": "92000",
    "levels": 5,
    "order_size": "0.001",
    "max_investment_quote": "1000"
  }
}
```

## Strategy Types

| `strategy_type` | Config section | Crate |
| --- | --- | --- |
| `grid` | `grid` | `strategy-grid` |
| `mm` or `market_maker` | `mm` | `strategy-market-maker` |
| `dca` | `dca` | `strategy-dca` |
| `arbitrage` or `arb` | `arbitrage` | `strategy-arbitrage` |
| `orchestrator` | `orchestrator` | `bot-orchestrator` |

## Market Types

| Market type | Required fields | Notes |
| --- | --- | --- |
| `perp` | `exchange`, `type`, `base`, `index` | Native Hyperliquid perpetuals |
| `spot` | `exchange`, `type`, `base`, `quote`, `index` | Spot-style balances and fills |
| `hip3` | `exchange`, `type`, `base`, `dex`, `index` | HIP-3 sub-DEX markets |
| `outcome` | `exchange`, `type`, `name`, `outcome_id`, `side` | Prediction markets from `outcomeMeta` |

For HIP-4/outcome markets, query Hyperliquid `outcomeMeta`; do not infer availability from `meta`, `metaAndAssetCtxs`, or `perpDexs`.

## Credentials

Configs may include `private_key` and `address`. If either is omitted, the runner attempts to resolve credentials from the local Supurr credential store.

Private keys should be API-wallet keys, not primary wallet keys. Use `--dry-run` before live mode.

## Generated Schema

The package also installs a `schema` binary:

```bash
schema
schema --copy
```

`schema` prints the JSON schema for the CLI config. `schema --copy` writes it into the local repository schema locations when those paths exist.

## Runtime Pipeline

```text
config.json
  -> bot CLI parses and validates config
  -> exchange adapter is created
  -> strategy is built from strategy_type
  -> bot-engine runner polls quotes/fills
  -> strategy emits commands
  -> exchange adapter places/cancels orders
  -> optional trade/account sync reports state upstream
```

## Crate Map

| Crate | Role |
| --- | --- |
| `hyperliquid-trading-bot` | Installable CLI package; provides `bot` and `schema` binaries |
| `bot-engine` | Runtime loop, order manager, inventory, simulation, syncers, strategy builder |
| `bot-core` | Shared domain types, commands, events, exchange and strategy traits |
| `exchange-hyperliquid` | Hyperliquid HTTP adapter and signing |
| strategy crates | Grid, DCA, market maker, arbitrage, RSI, tick trader |

## Panic Conditions

The CLI is intended to return errors for invalid config, missing credentials, exchange setup failure, and runtime failures.

Known direct exits:

- `bot --help` prints help and exits with status `0`.
- malformed or incomplete command-line options are ignored by the current parser unless they make later config loading fail.

## Safety Notes

Live mode places real orders. Validate with `--dry-run`, then `--mode paper`, before using live mode.

Package contents are limited to Cargo metadata, this README, and Rust source files. Local `.env` files, deployment scripts, generated configs, and shell scripts are not included in the crates.io package.
