---
name: debug-bot-e2e
description: Debug why a bot exits, crashes, or misbehaves. Verify strategy behavior against expected logic. Query logs, check pod status, inspect bot configuration, and trace failures end-to-end.
---

> **NEVER stop, delete, or write to any running bot. READ-ONLY operations only.**

## System Architecture

```
Frontend (Buffer-UI)  ─┐
                        ├─► Bot API (apiv2.supurr.app) ─► K8s Pod (bots namespace)
CLI (supurr_cli)      ─┘       │                              │
                               │                              ├─ stdout → Promtail → Loki → Grafana
                               │                              └─ bot_engine + strategy-{grid,arb,dca}
                               │
                               └─► Legacy API (api.supurr.app) ─► Dashboard sync
```

### Key Code Paths

| Component     | Path                                                                                |
| ------------- | ----------------------------------------------------------------------------------- |
| Bot engine    | `/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/bot-cli`                  |
| Strategies    | `/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/strategy-{grid,arb,dca}`  |
| Bot API (v2)  | `/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot_api/src/routes/bots.ts`          |
| API Logger    | `/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot_api/src/services/logger.ts`      |
| API Telemetry | `/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot_api/src/middleware/telemetry.ts` |
| Legacy API    | `/Users/amitsharma/Desktop/work/algobot/algobot/botRoutes.py`                       |
| Frontend      | `/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/Buffer-UI`                  |
| CLI           | `/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/supurr_cli`                 |

---

## Credentials & Endpoints

| Service             | URL                                | Auth                                 |
| ------------------- | ---------------------------------- | ------------------------------------ |
| Grafana             | `https://observability.supurr.app` | `admin` / `0cyg9wt2rg9dqdk84n6hormy` |
| Loki Datasource UID | `bf9ckencc7x8gb`                   | (via Grafana proxy)                  |
| Bot API v2          | `https://apiv2.supurr.app`         | —                                    |
| Legacy API          | `https://api.supurr.app`           | —                                    |

**CRITICAL**: The Loki reverse proxy (`loki-reverse-proxy-production.up.railway.app`) BLOCKS all read requests. All Loki queries MUST go through Grafana's datasource proxy API. Do NOT attempt direct Loki access.

---

## Logging Pipeline — Two Sources

There are **two independent log streams** in Loki. Know which one you need:

| Source                   | Loki Label            | Pipeline                                     | Log Format                                         |
| ------------------------ | --------------------- | -------------------------------------------- | -------------------------------------------------- |
| Bot pods (Rust engine)   | `{bot_id="289"}`      | stdout → Promtail → Loki                     | Plain text with `INFO`/`DEBUG`/`ERROR` prefix      |
| Bot API server (Node.js) | `{service="bot-api"}` | Structured JSON → Loki push (via `LOKI_URL`) | JSON with `trace_id`, `bot_id`, `duration_ms` etc. |

### Bot Pod Logs (Promtail)

```
Bot Pod (stdout) → Promtail DaemonSet → Loki Reverse Proxy (push-only) → Loki (Railway internal) → Grafana (read proxy)
```

Promtail extracts labels from pod log file paths using regex:

- Path: `/var/log/pods/bots_bot-{ID}_*/bot/0.log`
- Extracted: `bot_id`, `pod`, `namespace`, `container`
- Loki auto-adds: `detected_level` (info/debug/warn/error)

### Bot API Server Logs (Structured JSON)

```
Bot API (Hono/Node.js) → JSON stdout/Loki push → Loki → Grafana
```

The Bot API emits structured JSON logs via `logger.ts`. Each log entry includes:

- `trace_id` — unique per request (also returned as `X-Trace-ID` response header)
- `bot_id` — extracted from URL path if present (e.g. `/bots/42/stop`)
- `http_method`, `path`, `status_code`, `duration_ms` — request lifecycle
- `error`, `stack` — on error logs
- `service: "bot-api"` — Loki label for filtering

**Query API server logs for a specific bot:**

```bash
curl -s -u "$AUTH" -G "$LOKI_BASE/query_range" \
  --data-urlencode 'query={service="bot-api"} | json | bot_id="<BOT_ID>"' \
  --data-urlencode "start=$START" --data-urlencode "end=$END" \
  --data-urlencode 'limit=20' --data-urlencode 'direction=backward'
```

**Query API server errors only:**

```bash
curl -s -u "$AUTH" -G "$LOKI_BASE/query_range" \
  --data-urlencode 'query={service="bot-api"} | json | level="ERROR"' \
  --data-urlencode "start=$START" --data-urlencode "end=$END" \
  --data-urlencode 'limit=20' --data-urlencode 'direction=backward'
```

**Correlate by trace_id (find all logs for one request):**

```bash
curl -s -u "$AUTH" -G "$LOKI_BASE/query_range" \
  --data-urlencode 'query={service="bot-api"} | json | trace_id="<TRACE_ID>"' \
  --data-urlencode "start=$START" --data-urlencode "end=$END" \
  --data-urlencode 'limit=20'
```

**Query slow requests (> 1 second):**

```bash
curl -s -u "$AUTH" -G "$LOKI_BASE/query_range" \
  --data-urlencode 'query={service="bot-api"} | json | duration_ms > 1000' \
  --data-urlencode "start=$START" --data-urlencode "end=$END" \
  --data-urlencode 'limit=20' --data-urlencode 'direction=backward'
```

---

## Querying Logs — Two API Layers

### Base URL (Loki Datasource Proxy)

```
LOKI_BASE="https://observability.supurr.app/api/datasources/proxy/uid/bf9ckencc7x8gb/loki/api/v1"
AUTH="admin:0cyg9wt2rg9dqdk84n6hormy"
```

### Layer 1: Raw Loki API (preferred for scripts)

Returns native Loki JSON. Needs explicit unix timestamps.

**List all bot_ids with logs:**

```bash
curl -s -u "$AUTH" "$LOKI_BASE/label/bot_id/values"
# → {"status":"success","data":["241","282","287"]}
```

**Get stream labels for a bot:**

```bash
curl -s -u "$AUTH" "$LOKI_BASE/series" --data-urlencode 'match[]={bot_id="<BOT_ID>"}'
```

**Fetch log lines (main query):**

```bash
START=$(date -v-1H +%s) && END=$(date +%s)
curl -s -u "$AUTH" -G "$LOKI_BASE/query_range" \
  --data-urlencode 'query={bot_id="<BOT_ID>"}' \
  --data-urlencode "start=$START" \
  --data-urlencode "end=$END" \
  --data-urlencode 'limit=50' \
  --data-urlencode 'direction=backward'
```

**Filter errors only:**

```bash
curl -s -u "$AUTH" -G "$LOKI_BASE/query_range" \
  --data-urlencode 'query={bot_id="<BOT_ID>"} |= "ERROR"' \
  --data-urlencode "start=$START" --data-urlencode "end=$END" \
  --data-urlencode 'limit=50' --data-urlencode 'direction=backward'
```

**Parse response with python:**

```bash
... | python3 -c "
import sys,json
d=json.load(sys.stdin)
for stream in d.get('data',{}).get('result',[]):
    labels = stream['stream']
    for ts, line in stream.get('values',[]):
        print(f'[{labels.get(\"detected_level\",\"?\")}] {line}')
"
```

Response shape:

```json
{
  "data": {
    "result": [{
      "stream": {"bot_id": "287", "detected_level": "info", "pod": "bot-287"},
      "values": [["<nanosecond_ts>", "<log_line>"], ...]
    }]
  }
}
```

> WARNING: `/query` (instant) rejects log stream queries. ALWAYS use `/query_range`.

### Layer 2: Grafana Native Query API

Returns Grafana data frames. Supports relative time (`now-1h`).

```bash
curl -s -u "$AUTH" "https://observability.supurr.app/api/ds/query" \
  -H "Content-Type: application/json" -X POST \
  -d '{"queries":[{"refId":"A","datasource":{"uid":"bf9ckencc7x8gb","type":"loki"},"expr":"{bot_id=\"<BOT_ID>\"}","queryType":"range","maxLines":50}],"from":"now-1h","to":"now"}'
```

**Parse response:**

```bash
... | python3 -c "
import sys,json
d=json.load(sys.stdin)
for frame in d.get('results',{}).get('A',{}).get('frames',[]):
    fields = frame.get('schema',{}).get('fields',[])
    values = frame.get('data',{}).get('values',[])
    line_idx = next((i for i,f in enumerate(fields) if f['name']=='Line'), None)
    if line_idx and line_idx < len(values):
        for line in values[line_idx]:
            print(line[:300])
"
```

Response shape: `results.A.frames[]` → each frame is one Loki stream.

- `schema.fields[i]` = column definition (labels, Time, Line, tsNs, id)
- `data.values[i]` = column data array
- `fields[0].labels` = stream labels as metadata

### Available Loki API Endpoints

| Endpoint               | Method | Use                                               |
| ---------------------- | ------ | ------------------------------------------------- |
| `/labels`              | GET    | All label names                                   |
| `/label/{name}/values` | GET    | Distinct values for a label                       |
| `/series`              | GET    | Full label set for matching streams               |
| `/query_range`         | GET    | **Fetch log lines** (needs start/end timestamps)  |
| `/query`               | GET    | Metric queries only (e.g. `count_over_time(...)`) |
| `/index/stats`         | GET    | Stream/chunk/byte counts                          |

---

---

## Bot API Endpoints

```bash
# Bot status (from DB + K8s)
curl -s "https://apiv2.supurr.app/bots/<ID>/status"

# Bot logs via API (proxies kubectl logs)
curl -s "https://apiv2.supurr.app/bots/<ID>/logs"

# List active bots for a wallet
curl -s "https://apiv2.supurr.app/bots/active/<WALLET_ADDRESS>"

# Health check
curl -s "https://apiv2.supurr.app/health"
```

> NOTE: Bot API returns Internal Server Error (not 404) if the pod doesn't exist — this is a known bug.

---

## Direct Database Inspection

When the API status endpoint isn't enough, inspect the DB directly using `tsx`:

```bash
cd /Users/amitsharma/Desktop/work/botfromscratch/bot_api

# Get full bot session row
node --import dotenv/config --import tsx -e "
import { db } from './src/db';
import { botSessions } from './src/db/schema';
import { eq } from 'drizzle-orm';
const [s] = await db.select().from(botSessions).where(eq(botSessions.id, <BOT_ID>));
console.log(JSON.stringify(s, null, 2));
process.exit(0);
"

# List all running bots
node --import dotenv/config --import tsx -e "
import { db } from './src/db';
import { botSessions, SessionStatus } from './src/db/schema';
import { eq } from 'drizzle-orm';
const bots = await db.select({ id: botSessions.id, market: botSessions.market, type: botSessions.botType, address: botSessions.userAddress }).from(botSessions).where(eq(botSessions.status, SessionStatus.RUNNING));
console.table(bots);
process.exit(0);
"
```

> Useful when: bot shows `running` in DB but pod is gone, or you need to check `config`, `performance`, or `createdBy` fields.

---

## Common Failure Patterns

| Symptom                   | What you'll see in logs                                 | Root cause                                             | How to diagnose                                                    |
| ------------------------- | ------------------------------------------------------- | ------------------------------------------------------ | ------------------------------------------------------------------ |
| Bot immediately exits     | `"exit_reason"` or `"config validation failed"` in Loki | Config error (invalid key, missing market)             | Query Loki for `{bot_id="<ID>"}` — check first few log lines       |
| Bot exits and restarts    | Repeated short-lived log bursts in Loki                 | Recurring panic or connection failure                  | Query Loki with `\|= "ERROR"` or `\|= "panic"` filter              |
| Bot never starts          | No Loki logs for bot_id at all                          | Pod creation failed (image pull, scheduling)           | Check Bot API logs: `{service="bot-api"} \| json \| bot_id="<ID>"` |
| Bot killed mid-run        | `"OOMKilled"` or sudden log stop                        | Memory limit exceeded                                  | Check Bot API status endpoint + Loki for last log before silence   |
| Bot runs but no trades    | No errors in logs, strategy ticking normally            | Strategy conditions not met (price outside grid range) | Check grid levels vs current market price in config                |
| Bot shows running, no pod | API status says `running` but no recent Loki logs       | Pod died but DB wasn't updated                         | Direct DB inspection via tsx to check actual state                 |

---

## Debugging Decision Tree

```
Bot reported as failed
│
├─ 1. Check Loki for bot pod logs
│     curl ... query_range {bot_id="<ID>"}
│     ├─ Logs present → check for ERROR/panic/exit_reason
│     └─ No logs → bot never started (startup crash) → continue ↓
│
├─ 2. Check Bot API server logs (was the create request received?)
│     curl ... query_range {service="bot-api"} | json | bot_id="<ID>"
│     ├─ Create log found → check for K8s errors in same trace_id
│     └─ No create log → request never reached API (frontend/CLI issue)
│
├─ 3. Check bot API for DB state
│     curl -s "https://apiv2.supurr.app/bots/<ID>/status"
│     └─ Look for: status, exit reason, timestamps, config issues
│
├─ 4. Direct DB inspection (if API is unhelpful)
│     tsx one-liner to query botSessions table directly
│     └─ Check: config.private_key validity, market, strategy_type
│
├─ 5. Check if it's a known pattern
│     Common: "Invalid private key" → credential issue
│     Common: OOMKilled → memory limit
│     Common: CrashLoopBackOff → recurring panic
│
└─ 6. Inspect bot source code (clone repos — see below)
      Strategy: bot-base/crates/strategy-{grid,arb,dca}/src/
      Engine: bot-base/crates/bot-engine/src/
      CLI entry: bot-base/crates/bot-cli/src/
```

---

## Source Code Reference

Sometimes logs alone aren't enough — you need to verify behavior against the actual code. Clone these repositories to read and reason about the implementation.

### Cloning Repositories

Use the read-only fine-grained PAT to clone:

```bash
PAT="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"

# Bot engine (Rust) — strategies, order management, exchange integration
git clone https://$PAT@github.com/Supurr-App/bot-base.git

# Bot API (Node.js/Hono) — HTTP endpoints, K8s pod lifecycle, DB ops
git clone https://$PAT@github.com/Supurr-App/supurr_api.git
```

> **READ-ONLY access.** This PAT cannot push, create branches, or modify anything.

### Bot Engine (Rust) — `bot-base`

The trading bot binary. Each bot pod runs this as a single process.

```
bot-base/crates/
├── bot-cli/          # Binary entrypoint — parses config, launches engine
├── bot-core/         # Shared types and traits
│   ├── commands.rs   #   Commands sent TO the engine (stop, update config)
│   ├── events.rs     #   Events emitted BY the engine (fill, error, exit)
│   ├── exchange.rs   #   Exchange trait — abstraction over any CEX/DEX
│   ├── market.rs     #   Market types (Perp, Spot, HIP-3)
│   ├── strategy.rs   #   Strategy trait — every strategy implements this
│   └── types.rs      #   Order, Position, Fill, Balance types
├── bot-engine/       # Core execution loop
│   ├── engine.rs     #   Main event loop — ticks strategies, processes fills
│   ├── runner.rs     #   Pod lifecycle — startup, shutdown, signal handling
│   ├── config.rs     #   Config parsing and validation
│   ├── order_manager.rs    # Order placement, cancellation, tracking
│   ├── trade_syncer.rs     # Syncs fills to Bot API for PnL calculation
│   └── account_syncer.rs   # Syncs balances and positions from exchange
├── exchange-hyperliquid/   # Hyperliquid-specific implementation
│   ├── client.rs     #   REST + WebSocket client for HL
│   └── signing.rs    #   EIP-712 signing for HL actions
├── strategy-grid/    # Grid trading strategy
├── strategy-arb/     # Spot-perp arbitrage strategy
├── strategy-dca/     # Dollar-cost averaging strategy
└── strategy-market-maker/  # Market maker strategy (legacy)
```

**When to read bot code:**

- Log says `"strategy error"` or `"exit_reason"` → check the strategy crate's `tick()` or `on_fill()` methods
- Log says `"order rejected"` → check `order_manager.rs` or `exchange-hyperliquid/client.rs`
- Log says `"config validation failed"` → check `bot-engine/config.rs`
- Bot exits silently → check `runner.rs` for signal handling and shutdown logic

### Bot API (Node.js/Hono) — `supurr_api`

The HTTP backend that manages bot lifecycle via Kubernetes.

```
supurr_api/src/
├── index.ts          # Hono app setup, route registration
├── routes/
│   └── bots.ts       # All bot endpoints: create, stop, status, logs
├── services/
│   ├── k8s.ts        # K8s pod creation, deletion, log retrieval
│   ├── config-builder.ts   # Builds ConfigMaps from user input
│   ├── logger.ts     # Structured JSON logging → Loki
│   ├── signature.ts  # Wallet signature verification
│   ├── referral.ts   # Referral fee discount logic
│   └── file-uploader.ts    # S3 uploads (bot configs, logs)
├── db/
│   └── schema.ts     # Drizzle ORM schema (botSessions table)
├── middleware/
│   └── telemetry.ts  # Request tracing, trace_id injection
└── schemas/          # Zod validation schemas for API inputs
```

**When to read API code:**

- Bot API returns 500 on create → check `routes/bots.ts` create handler and `services/k8s.ts` pod creation
- ConfigMap looks wrong → check `services/config-builder.ts`
- Bot shows `running` in DB but pod is gone → check `services/k8s.ts` status reconciliation
- Auth/signature errors → check `services/signature.ts`

---

## Hyperliquid Python SDK for Debugging

The `hyperliquid-python-sdk` is installed globally in `python3` and provides read-only access to Hyperliquid's Info API. Use it to inspect account states, positions, orders, and market data when debugging bot behavior.

### Installation Location

```bash
/Users/amitsharma/hyperliquid-python-sdk
```

### Basic Setup

```python
from hyperliquid.info import Info
from hyperliquid.utils import constants

# Initialize Info API (read-only)
info = Info(constants.MAINNET_API_URL, skip_ws=True)
```

---

### Query User State (Perps)

**Get complete account state including balances, positions, and margin:**

```bash
python3 -c "
from hyperliquid.info import Info
from hyperliquid.utils import constants

info = Info(constants.MAINNET_API_URL, skip_ws=True)
address = '0x...'  # User address

state = info.user_state(address)

# Account summary
margin = state['marginSummary']
print(f'Account Value: {margin[\"accountValue\"]}')
print(f'Total Margin Used: {margin[\"totalMarginUsed\"]}')
print(f'Withdrawable: {state[\"withdrawable\"]}')

# Positions
for asset_pos in state['assetPositions']:
    pos = asset_pos['position']
    print(f'{pos[\"coin\"]}: {pos[\"szi\"]} @ {pos[\"entryPx\"]} (PnL: {pos[\"unrealizedPnl\"]})')
"
```

**Response structure:**

```json
{
  "marginSummary": {
    "accountValue": "1234.56",
    "totalMarginUsed": "100.00",
    "totalNtlPos": "500.00",
    "totalRawUsd": "1234.56"
  },
  "withdrawable": "1134.56",
  "assetPositions": [
    {
      "position": {
        "coin": "ETH",
        "szi": "0.5",
        "entryPx": "2000.0",
        "unrealizedPnl": "10.5",
        "leverage": { "value": 5 }
      }
    }
  ]
}
```

---

### Query Spot Balances

**Get spot wallet balances:**

```bash
python3 -c "
from hyperliquid.info import Info
from hyperliquid.utils import constants

info = Info(constants.MAINNET_API_URL, skip_ws=True)
address = '0x...'

spot_state = info.spot_user_state(address)

for bal in spot_state['balances']:
    coin = bal['coin']
    total = bal['total']
    hold = bal['hold']
    free = float(total) - float(hold)
    print(f'{coin}: Total={total}, Hold={hold}, Free={free}')
"
```

---

### Query Active Orders

**Get all open orders for an account:**

```bash
python3 -c "
from hyperliquid.info import Info
from hyperliquid.utils import constants

info = Info(constants.MAINNET_API_URL, skip_ws=True)
address = '0x...'

# Perps open orders
open_orders = info.open_orders(address)

for order in open_orders:
    print(f'Order #{order[\"oid\"]}: {order[\"coin\"]} {order[\"side\"]} {order[\"sz\"]} @ {order[\"limitPx\"]}')
    print(f'  Status: {order[\"orderType\"]}, Filled: {order.get(\"filledSz\", \"0\")}')
"
```

**Response structure:**

```json
[
  {
    "coin": "ETH",
    "side": "B",
    "limitPx": "1950.0",
    "sz": "0.01",
    "oid": 123456789,
    "timestamp": 1708123456789,
    "orderType": "Limit",
    "origSz": "0.01",
    "cloid": "0x..."
  }
]
```

---

### Query Order History

**Get historical orders (filled, canceled, rejected):**

```bash
python3 -c "
from hyperliquid.info import Info
from hyperliquid.utils import constants

info = Info(constants.MAINNET_API_URL, skip_ws=True)
address = '0x...'

# Get user fills (executed trades)
fills = info.user_fills(address)

for fill in fills[:10]:  # Last 10 fills
    print(f'{fill[\"time\"]}: {fill[\"coin\"]} {fill[\"side\"]} {fill[\"sz\"]} @ {fill[\"px\"]}')
    print(f'  Fee: {fill[\"fee\"]}, Closed PnL: {fill.get(\"closedPnl\", \"0\")}')
"
```

**Get funding history:**

```bash
python3 -c "
from hyperliquid.info import Info
from hyperliquid.utils import constants

info = Info(constants.MAINNET_API_URL, skip_ws=True)
address = '0x...'

# Funding payments
funding = info.user_funding(address)

for payment in funding[:10]:
    print(f'{payment[\"time\"]}: {payment[\"coin\"]} funding={payment[\"fundingRate\"]} payment={payment[\"usdc\"]}')
"
```

---

### Query Market Data

**Get current funding rate:**

```bash
python3 -c "
from hyperliquid.info import Info
from hyperliquid.utils import constants

info = Info(constants.MAINNET_API_URL, skip_ws=True)

# Get metadata for all markets
meta = info.meta()

for asset in meta['universe']:
    if asset['name'] == 'ETH':
        print(f'ETH Funding Rate: {asset.get(\"funding\", \"N/A\")}')
        print(f'Open Interest: {asset.get(\"openInterest\", \"N/A\")}')
        print(f'24h Volume: {asset.get(\"dayNtlVlm\", \"N/A\")}')
"
```

**Get order book (L2 data):**

```bash
python3 -c "
from hyperliquid.info import Info
from hyperliquid.utils import constants

info = Info(constants.MAINNET_API_URL, skip_ws=True)

# Get L2 order book for ETH
l2 = info.l2_snapshot('ETH')

print('Bids:')
for level in l2['levels'][0][:5]:  # Top 5 bids
    print(f'  {level[\"px\"]} x {level[\"sz\"]}')

print('Asks:')
for level in l2['levels'][1][:5]:  # Top 5 asks
    print(f'  {level[\"px\"]} x {level[\"sz\"]}')
"
```

**Get all mid prices:**

```bash
python3 -c "
from hyperliquid.info import Info
from hyperliquid.utils import constants

info = Info(constants.MAINNET_API_URL, skip_ws=True)

# Get all market mid prices
mids = info.all_mids()

for coin, price in mids.items():
    print(f'{coin}: {price}')
"
```

---

### Debugging Bot Accounts

**Complete diagnostic script for a bot's trading account:**

```bash
python3 -c "
from hyperliquid.info import Info
from hyperliquid.utils import constants

info = Info(constants.MAINNET_API_URL, skip_ws=True)
address = '0x...'  # Bot's user address

print('=' * 60)
print(f'Bot Account Diagnostic: {address}')
print('=' * 60)

# 1. Perps state
print('\n[PERPS ACCOUNT]')
state = info.user_state(address)
margin = state['marginSummary']
print(f'Account Value: {margin[\"accountValue\"]} USD')
print(f'Margin Used: {margin[\"totalMarginUsed\"]} USD')
print(f'Withdrawable: {state[\"withdrawable\"]} USD')

# 2. Positions
positions = state['assetPositions']
print(f'\nOpen Positions: {len(positions)}')
for asset_pos in positions:
    pos = asset_pos['position']
    print(f'  {pos[\"coin\"]}: {pos[\"szi\"]} @ {pos[\"entryPx\"]} (PnL: {pos[\"unrealizedPnl\"]})')

# 3. Active orders
orders = info.open_orders(address)
print(f'\nActive Orders: {len(orders)}')
for order in orders[:10]:
    print(f'  {order[\"coin\"]} {order[\"side\"]} {order[\"sz\"]} @ {order[\"limitPx\"]}')

# 4. Recent fills
fills = info.user_fills(address)
print(f'\nRecent Fills: {len(fills[:10])}')
for fill in fills[:5]:
    print(f'  {fill[\"time\"]}: {fill[\"coin\"]} {fill[\"side\"]} {fill[\"sz\"]} @ {fill[\"px\"]}')

# 5. Spot balances
spot_state = info.spot_user_state(address)
balances = spot_state['balances']
print(f'\nSpot Balances: {len(balances)}')
for bal in balances:
    if float(bal['total']) > 0:
        print(f'  {bal[\"coin\"]}: {bal[\"total\"]} (hold: {bal[\"hold\"]})')

print('\n' + '=' * 60)
"
```

---

### Common Debugging Queries

**Check if bot has sufficient balance:**

```bash
python3 -c "
from hyperliquid.info import Info
from hyperliquid.utils import constants

info = Info(constants.MAINNET_API_URL, skip_ws=True)
address = '0x...'

state = info.user_state(address)
withdrawable = float(state['withdrawable'])
required = 100.0  # Bot investment amount

if withdrawable < required:
    print(f'INSUFFICIENT BALANCE: {withdrawable} < {required}')
else:
    print(f'OK: {withdrawable} >= {required}')
"
```

**Verify grid orders are placed:**

```bash
python3 -c "
from hyperliquid.info import Info
from hyperliquid.utils import constants

info = Info(constants.MAINNET_API_URL, skip_ws=True)
address = '0x...'

orders = info.open_orders(address)
eth_orders = [o for o in orders if o['coin'] == 'ETH']

print(f'ETH orders: {len(eth_orders)}')
for order in eth_orders:
    print(f'  {order[\"side\"]} {order[\"sz\"]} @ {order[\"limitPx\"]}')
"
```

**Check if orders were silently removed (Hyperliquid bug):**

```bash
python3 -c "
from hyperliquid.info import Info
from hyperliquid.utils import constants
import time

info = Info(constants.MAINNET_API_URL, skip_ws=True)
address = '0x...'

# Check orders now
orders_now = info.open_orders(address)
print(f'Current orders: {len(orders_now)}')

# Wait and check again
time.sleep(5)
orders_later = info.open_orders(address)
print(f'Orders after 5s: {len(orders_later)}')

if len(orders_later) < len(orders_now):
    print('WARNING: Orders disappeared (possible silent removal)')
"
```

---

### API Reference

| Method                          | Description                  | Returns                     |
| ------------------------------- | ---------------------------- | --------------------------- |
| `info.user_state(address)`      | Complete perps account state | Balances, positions, margin |
| `info.spot_user_state(address)` | Spot wallet balances         | Spot token balances         |
| `info.open_orders(address)`     | Active orders                | List of open orders         |
| `info.user_fills(address)`      | Trade history                | List of executed fills      |
| `info.user_funding(address)`    | Funding payments             | Funding history             |
| `info.meta()`                   | Market metadata              | Funding rates, OI, volume   |
| `info.all_mids()`               | All market prices            | Dict of coin → mid price    |
| `info.l2_snapshot(coin)`        | Order book                   | Bids and asks               |

**Full SDK documentation:** https://github.com/hyperliquid-dex/hyperliquid-python-sdk

---

## Write Operations (Trading & Order Management)

> **⚠️ CRITICAL WARNING ⚠️**
>
> **ALWAYS ASK FOR USER APPROVAL BEFORE EXECUTING ANY WRITE OPERATIONS.**
>
> Write operations modify account state and can result in:
>
> - Canceled orders (potentially causing losses if market moves)
> - Executed trades (real money at risk)
> - Position changes (exposure to market risk)
> - Fee costs (trading fees, gas fees)
>
> **NEVER auto-run write operations.** Always present the command to the user and wait for explicit confirmation.

### Setup for Write Operations

Write operations require a private key for signing transactions:

```python
from hyperliquid.exchange import Exchange
from hyperliquid.utils import constants
from eth_account import Account

# Initialize Exchange API (requires private key)
private_key = "0x..."  # NEVER hardcode, use env vars or prompt
account = Account.from_key(private_key)
exchange = Exchange(account, constants.MAINNET_API_URL)
```

---

### Cancel a Specific Order

**Cancel a single order by order ID:**

```bash
python3 -c "
from hyperliquid.exchange import Exchange
from hyperliquid.utils import constants
from eth_account import Account

private_key = '0x...'  # API wallet private key
account = Account.from_key(private_key)
exchange = Exchange(account, constants.MAINNET_API_URL)

# Cancel order
coin = 'ETH'
order_id = 123456789  # Order ID (oid) from open_orders()

result = exchange.cancel(coin, order_id)
print(f'Cancel result: {result}')
"
```

**Response:**

```json
{
  "status": "ok",
  "response": {
    "type": "cancel",
    "data": {
      "statuses": ["success"]
    }
  }
}
```

---

### Cancel All Orders

**Cancel all open orders for a specific coin:**

```bash
python3 -c "
from hyperliquid.exchange import Exchange
from hyperliquid.info import Info
from hyperliquid.utils import constants
from eth_account import Account

private_key = '0x...'
account = Account.from_key(private_key)
exchange = Exchange(account, constants.MAINNET_API_URL)
info = Info(constants.MAINNET_API_URL, skip_ws=True)

# Get all open orders
address = account.address
orders = info.open_orders(address)

# Filter by coin (e.g., ETH)
coin = 'ETH'
eth_orders = [o for o in orders if o['coin'] == coin]

print(f'Canceling {len(eth_orders)} {coin} orders...')

# Cancel each order
for order in eth_orders:
    result = exchange.cancel(coin, order['oid'])
    print(f'Canceled order {order[\"oid\"]}: {result[\"status\"]}')'
"
```

**Cancel ALL orders across all coins:**

```bash
python3 -c "
from hyperliquid.exchange import Exchange
from hyperliquid.info import Info
from hyperliquid.utils import constants
from eth_account import Account

private_key = '0x...'
account = Account.from_key(private_key)
exchange = Exchange(account, constants.MAINNET_API_URL)
info = Info(constants.MAINNET_API_URL, skip_ws=True)

# Get all open orders
address = account.address
orders = info.open_orders(address)

print(f'Canceling {len(orders)} total orders...')

# Group by coin
from collections import defaultdict
orders_by_coin = defaultdict(list)
for order in orders:
    orders_by_coin[order['coin']].append(order)

# Cancel all orders for each coin
for coin, coin_orders in orders_by_coin.items():
    print(f'\\nCanceling {len(coin_orders)} {coin} orders...')
    for order in coin_orders:
        result = exchange.cancel(coin, order['oid'])
        print(f'  Order {order[\"oid\"]}: {result[\"status\"]}')'
"
```

---

### Place Perps Order

**Place a limit order on perpetuals:**

```bash
python3 -c "
from hyperliquid.exchange import Exchange
from hyperliquid.utils import constants
from eth_account import Account

private_key = '0x...'
account = Account.from_key(private_key)
exchange = Exchange(account, constants.MAINNET_API_URL)

# Order parameters
coin = 'ETH'
is_buy = True  # True for buy, False for sell
size = 0.01  # Size in base asset (ETH)
limit_price = 2000.0  # Limit price in USD
reduce_only = False  # Set True to only reduce position

# Place order
order_result = exchange.order(
    coin=coin,
    is_buy=is_buy,
    sz=size,
    limit_px=limit_price,
    order_type={'limit': {'tif': 'Gtc'}},  # Good-til-canceled
    reduce_only=reduce_only
)

print(f'Order result: {order_result}')
"
```

**Place a market order:**

```bash
python3 -c "
from hyperliquid.exchange import Exchange
from hyperliquid.utils import constants
from eth_account import Account

private_key = '0x...'
account = Account.from_key(private_key)
exchange = Exchange(account, constants.MAINNET_API_URL)

# Market order (use current market price with slippage)
coin = 'ETH'
is_buy = True
size = 0.01
slippage = 0.05  # 5% slippage tolerance

# Get current price
from hyperliquid.info import Info
info = Info(constants.MAINNET_API_URL, skip_ws=True)
mids = info.all_mids()
current_price = float(mids[coin])

# Calculate limit price with slippage
if is_buy:
    limit_price = current_price * (1 + slippage)
else:
    limit_price = current_price * (1 - slippage)

order_result = exchange.order(
    coin=coin,
    is_buy=is_buy,
    sz=size,
    limit_px=limit_price,
    order_type={'limit': {'tif': 'Ioc'}},  # Immediate-or-cancel (market-like)
    reduce_only=False
)

print(f'Market order result: {order_result}')
"
```

---

### Place Spot Order

**Place a spot limit order:**

```bash
python3 -c "
from hyperliquid.exchange import Exchange
from hyperliquid.utils import constants
from eth_account import Account

private_key = '0x...'
account = Account.from_key(private_key)
exchange = Exchange(account, constants.MAINNET_API_URL)

# Spot order parameters
coin = 'ETH'  # Base asset
is_buy = True  # Buy ETH with USDC
size = 0.01  # Amount of ETH
limit_price = 2000.0  # Price in USDC

# Place spot order
order_result = exchange.order(
    coin=coin,
    is_buy=is_buy,
    sz=size,
    limit_px=limit_price,
    order_type={'limit': {'tif': 'Gtc'}},
    reduce_only=False,
    spot=True  # IMPORTANT: Set spot=True for spot orders
)

print(f'Spot order result: {order_result}')
"
```

---

### Close Position

**Close an entire position (market order):**

```bash
python3 -c "
from hyperliquid.exchange import Exchange
from hyperliquid.info import Info
from hyperliquid.utils import constants
from eth_account import Account

private_key = '0x...'
account = Account.from_key(private_key)
exchange = Exchange(account, constants.MAINNET_API_URL)
info = Info(constants.MAINNET_API_URL, skip_ws=True)

# Get current position
address = account.address
state = info.user_state(address)

coin = 'ETH'
position_size = None

for asset_pos in state['assetPositions']:
    pos = asset_pos['position']
    if pos['coin'] == coin:
        position_size = float(pos['szi'])
        break

if position_size is None or position_size == 0:
    print(f'No {coin} position to close')
else:
    # Determine order side (opposite of position)
    is_buy = position_size < 0  # If short, buy to close
    size = abs(position_size)

    # Get current price for market order
    mids = info.all_mids()
    current_price = float(mids[coin])
    slippage = 0.05

    if is_buy:
        limit_price = current_price * (1 + slippage)
    else:
        limit_price = current_price * (1 - slippage)

    # Place close order
    order_result = exchange.order(
        coin=coin,
        is_buy=is_buy,
        sz=size,
        limit_px=limit_price,
        order_type={'limit': {'tif': 'Ioc'}},
        reduce_only=True  # IMPORTANT: Ensure we only close, not reverse
    )

    print(f'Close position result: {order_result}')
"
```

---

### Error Handling

**Always wrap write operations in try-except:**

```python
from hyperliquid.exchange import Exchange
from hyperliquid.utils import constants
from eth_account import Account

try:
    private_key = '0x...'
    account = Account.from_key(private_key)
    exchange = Exchange(account, constants.MAINNET_API_URL)

    result = exchange.order(
        coin='ETH',
        is_buy=True,
        sz=0.01,
        limit_px=2000.0,
        order_type={'limit': {'tif': 'Gtc'}},
        reduce_only=False
    )

    # Check result status
    if result['status'] == 'ok':
        print(f"Order placed successfully: {result['response']}")
    else:
        print(f"Order failed: {result}")

except Exception as e:
    print(f"Error placing order: {e}")
```

---

### Order Type Reference

| Order Type                  | Description                       | Use Case                               |
| --------------------------- | --------------------------------- | -------------------------------------- |
| `{'limit': {'tif': 'Gtc'}}` | Good-til-canceled limit order     | Normal limit orders                    |
| `{'limit': {'tif': 'Ioc'}}` | Immediate-or-cancel (market-like) | Market orders with slippage protection |
| `{'limit': {'tif': 'Alo'}}` | Add-liquidity-only (post-only)    | Grid bots, market making               |

### Common Parameters

| Parameter     | Type  | Description                                |
| ------------- | ----- | ------------------------------------------ |
| `coin`        | str   | Asset symbol (e.g., 'ETH', 'BTC')          |
| `is_buy`      | bool  | True for buy, False for sell               |
| `sz`          | float | Order size in base asset                   |
| `limit_px`    | float | Limit price in quote currency (USD/USDC)   |
| `reduce_only` | bool  | True to only reduce position, not increase |
| `spot`        | bool  | True for spot orders, False/omit for perps |

---

### Safety Checklist

Before executing any write operation, verify:

- [ ] User has explicitly approved the operation
- [ ] Private key is correct (API wallet, not main wallet)
- [ ] Order parameters are correct (size, price, side)
- [ ] Sufficient balance exists for the operation
- [ ] Market conditions are understood (volatility, liquidity)
- [ ] Error handling is in place
- [ ] Operation is logged for audit trail

**Remember: Real money is at risk. Always double-check before executing.**
