# Hyperliquid Outcome Markets — Context Dump

> **Date**: 2026-03-10 (session ended ~23:00 IST)
> **Status**: Core implementation DONE. Compiles clean, 7 unit tests pass.
> **Remaining**: Sample config + testnet dry-run.

---

## TL;DR — What Was Done

Added **prediction market (Outcome)** support across 8 files. Outcomes are HL testnet-only binary event markets. They trade like spot but use asset IDs in the `100,000,000+` range.

---

## Protocol Facts

| Property | Value |
|----------|-------|
| Encoding | `10 × outcome_id + side` |
| Asset ID (orders) | `100_000_000 + encoding` |
| Coin name (allMids/fills) | `#<encoding>` (e.g. `#5160`) |
| Token name | `+<encoding>` |
| Sides | `0` = Yes, `1` = No |
| Info endpoint | `{"type": "outcomeMeta"}` |
| Balance query | `spotClearinghouseState` (same as spot) |
| Leverage | None (spot-like) |
| Environment | **Testnet only** (`api.hyperliquid-testnet.xyz`) |

### Example

```
Market: "BTC > 69070" (outcome 516, side 0 = Yes)
Encoding: 10 × 516 + 0 = 5160
Asset ID: 100,005,160
Coin key in allMids: "#5160"
Price: 0.4825 (= 48.25% implied probability)
```

---

## Files Changed (13 files, +440 / -59 lines)

### bot-core

| File | Changes |
|------|---------|
| `crates/bot-core/src/types.rs` | Added `InstrumentKind::Outcome` variant |
| `crates/bot-core/src/market.rs` | Added `HyperliquidMarket::Outcome { name, outcome_id, side, instrument_meta }` variant. Added methods: `outcome_encoding()`, `outcome_params() -> Option<(u32, u8, String)>`, `is_outcome()`. Implemented all match arms (instrument_id, market_index, effective_asset_id, base, quote, etc). Added `PartialEq` to `Hip3MarketConfig`. Added 7 unit tests. |

### exchange-hyperliquid

| File | Changes |
|------|---------|
| `crates/exchange-hyperliquid/src/types.rs` | Added `OutcomeConfig` struct with `encoding()`, `asset_id()`, `coin_name()` helpers. Added `is_outcome: bool` and `outcome: Option<OutcomeConfig>` to `HyperliquidConfig`. |
| `crates/exchange-hyperliquid/src/client.rs` | Updated `effective_asset_id()` for outcome branch. Updated `fetch_user_state()` to route outcomes to `spotClearinghouseState`. Updated `parse_fills()` to handle `#<encoding>` coin format. Updated `poll_quotes()` for `#<encoding>` lookup. Added `fetch_outcome_meta()` method. |
| `crates/exchange-hyperliquid/src/lib.rs` | Re-exported `OutcomeConfig`. |

### bot-engine

| File | Changes |
|------|---------|
| `crates/bot-engine/src/config.rs` | Added `BotConfig::is_outcome()` method (mirrors existing `is_spot()`). |

### bot-cli

| File | Changes |
|------|---------|
| `crates/bot-cli/src/main.rs` | Builds `OutcomeConfig` from `market.outcome_params()`. Populates `is_outcome` and `outcome` in `HyperliquidConfig`. Uses `market.instrument_kind()` instead of manual check. Skips leverage update for outcome markets. Removed unused `InstrumentKind` import. |

### strategy-dca

| File | Changes |
|------|---------|
| `crates/strategy-dca/src/strategy.rs` | Fixed test: added missing `instrument_meta: None` to `Spot` constructor. |

---

## Key Design Decisions

1. **Outcome = spot-like**: No leverage, no funding. Uses `spotClearinghouseState` for balances.
2. **Encoding is the universal key**: `10 * outcome_id + side` maps to asset IDs, coin names, and token names.
3. **Coin prefix convention**: `#` for outcomes, `@` for spot, bare name for perps — used in fill parsing and price lookups.
4. **`outcome_params()` accessor on `Market`**: Clean way to extract outcome fields without exposing `HyperliquidMarket` internals to CLI.

---

## Verification Results

```
# Compile
$ cargo check
✅ Clean (only pre-existing warnings in bot-engine and derive/cockpit)

# Outcome tests
$ cargo test -p bot-core -- outcome
✅ 7/7 passed:
  test_outcome_encoding
  test_outcome_market_effective_asset_id
  test_outcome_market_properties
  test_outcome_market_kind
  test_outcome_market_instrument_id
  test_outcome_market_coin_name
  test_outcome_market_serde

# DCA tests (regression check)
$ cargo test -p strategy-dca
✅ 6/6 passed
```

---

## What's Left To Do

### 1. Create Sample Config (`config-v2-mm-outcome.json`)

```json
{
  "environment": "testnet",
  "private_key": "<TESTNET_PK>",
  "address": "<TESTNET_ADDRESS>",
  "strategy_type": "mm",
  "markets": [{
    "exchange": "hyperliquid",
    "type": "outcome",
    "name": "BTC > 69070",
    "outcome_id": 516,
    "side": 0,
    "instrument_meta": {
      "tick_size": "0.001",
      "lot_size": "1"
    }
  }],
  "mm": {
    "base_order_size": "10",
    "base_spread": "0.02",
    "max_position_size": "100"
  }
}
```

> **Note**: `outcome_id` and market details must be fetched fresh from the testnet `outcomeMeta` endpoint — these markets rotate/expire.

### 2. Testnet Dry-Run

```bash
# First query available outcome markets
curl -s https://api.hyperliquid-testnet.xyz/info \
  -H 'Content-Type: application/json' \
  -d '{"type": "outcomeMeta"}' | jq .

# Then run the bot in paper mode
cargo run --bin bot -- --config config-v2-mm-outcome.json --mode paper
```

### 3. Edge Cases to Watch

- **Price convergence**: Outcome prices snap to 0.0 or 1.0 near expiry — MM spread needs to account for this
- **Recurring markets**: Markets like "HYPE > 200.05 in 15m" reset periodically — the outcome_id changes each cycle
- **szDecimals**: Need to verify what `szDecimals` value outcomes use (likely integer quantities like spot)

---

## Live Testnet Markets (as of 2026-03-10)

| ID | Name | Type | Yes Price | No Price |
|:---:|:---|:---|:---:|:---:|
| 9 | HL 100m Dash | Fun | 0.948 | 0.052 |
| 10 | Akami | Fun | 0.540 | 0.460 |
| 11 | Canned Tuna | Fun | 0.175 | 0.825 |
| 12 | Otoro | Fun | 0.590 | 0.410 |
| **516** | **BTC > 69070** | **Recurring** | **0.483** | **0.518** |
| **686** | **HYPE > 200.05** | **Recurring** | **0.450** | **0.550** |

> Recurring markets (516, 686) are the best candidates for MM — they have real price dynamics tied to underlying crypto.

---

## Architecture Diagram

```
Asset ID Ranges:
  Perps:    0 — 9,999
  Spot:     10,000 — 99,999
  HIP-3:    100,000 — 999,999
  Outcomes: 100,000,000+          ← NEW

Data Flow:
  Config JSON
    → BotConfig.primary_market()
    → Market::Hyperliquid(HyperliquidMarket::Outcome { ... })
    → outcome_params() → OutcomeConfig { outcome_id, side, name }
    → HyperliquidConfig { is_outcome: true, outcome: Some(...) }
    → HyperliquidClient
      → effective_asset_id() returns 100M + encoding
      → parse_fills() matches "#<encoding>" prefix
      → poll_quotes() looks up "#<encoding>" in allMids
      → fetch_user_state() routes to spotClearinghouseState
```
