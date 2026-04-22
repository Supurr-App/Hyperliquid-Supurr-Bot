---
agent_scope: Preserve the read-only runtime evidence and code reasoning for why bot 353 was running without live DCA orders on April 16, 2026.
DO NOT:
  - Do not store Grafana credentials, private keys, or signed payload secrets here.
  - Do not treat this file as the implementation plan; code changes belong in a separate artifact.
SEE ALSO:
  - /Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/strategy-dca/src/strategy.rs
  - /Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/strategy-dca/src/state.rs
  - /Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/bot-engine/src/runner.rs
  - /Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/exchange-hyperliquid/src/client.rs
  - /Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/DCA_TP_REPLACEMENT_PLAN.md
snapshot_date: 2026-04-16
bot_id: 353
mode: read-only
---

Read-only snapshot for bot `353` before log retention changes.

## Snapshot

| Key | Value |
| --- | --- |
| Bot ID | `353` |
| Status API | `db_status=running`, `pod_status=Running` |
| Bot type | `dca` |
| Market | `BTC-USDC` / `BTC-PERP` |
| Created at | `2026-04-15T12:48:45.790Z` |
| Wallet | `0x0ecba0a94c0797bec6916bce5094651b8fdd10db` |
| Direction | `long` |
| Trigger | `74000` |
| Base size | `0.00030` |
| DCA size | `0.00030` |
| Max DCA orders | `4` |
| Size multiplier | `1.15` |
| Price deviation % | `1.8` |
| Deviation multiplier | `1.25` |
| Take profit % | `1.9` |
| Leverage | `3` |
| Restart on complete | `true` |
| Cooldown | `420s` |

Status source: `GET /bots/353/status` via `https://apiv2.supurr.app`.

## Read Trail

Everything below came from read-only calls:

1. `GET https://apiv2.supurr.app/bots/353/status`
2. `GET https://apiv2.supurr.app/bots/353/logs?tail=...`
3. Loki `query_range` for bot pod logs through Grafana datasource proxy
4. Hyperliquid `Info.open_orders`
5. Hyperliquid `Info.user_state`
6. Hyperliquid `Info.user_fills`
7. Hyperliquid `Info.historical_orders`
8. Hyperliquid `Info.query_order_by_cloid`
9. Local code reads only

No Kubernetes writes, no Bot API writes, no exchange writes, no stop/restart calls were issued from this investigation.

## What I Think Is Happening

```text
bot start
  -> DCA strategy builds 5-level ladder
  -> strategy marks those 5 orders as "placed" locally
  -> runner sends one batch place_orders request
  -> exchange outcome should become OrderAccepted / OrderRejected follow-up events
  -> startup path does not feed those follow-ups back into the strategy
  -> strategy stays in Active phase as if ladder exists
  -> Hyperliquid has no record of those 5 cloids
  -> live loop keeps logging filled=0/5 qty=0 tp=None
```

The shortest version:

- The bot definitely attempted to place the startup DCA ladder.
- Hyperliquid does not show those startup orders as open, filled, canceled, or historical.
- The strategy marks startup orders as placed before exchange confirmation.
- The runner startup path executes commands but drops the accept/reject follow-up events.
- That leaves the strategy believing the ladder exists when the exchange has nothing.

## Pipeline

### Step 1: Strategy starts and immediately builds the ladder

What Happens: `handle_start()` validates config, builds the 5-order ladder, and immediately calls `place_all_dca_orders()`.

Input -> Output: trigger `74000` with `max_dca_orders=4` becomes ladder prices `74000`, `72668`, `71032`, `69035`, `66608`.

Where in Code:
- [strategy.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/strategy-dca/src/strategy.rs:81)
- [strategy.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/strategy-dca/src/strategy.rs:123)
- [strategy.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/strategy-dca/src/strategy.rs:324)

Example:

```rust
self.build_dca_ladder(ctx);
self.state.is_initialized = true;

// Place all DCA orders immediately
self.place_all_dca_orders(ctx);
```

### Step 2: Strategy marks orders as placed before exchange confirmation

What Happens: `place_all_dca_orders()` generates `client_id`s, registers them locally, and calls `order.set_placed(...)` before the exchange says accepted/rejected.

Input -> Output: five pending ladder entries become five locally tracked "placed" orders.

Where in Code:
- [strategy.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/strategy-dca/src/strategy.rs:393)
- [strategy.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/strategy-dca/src/strategy.rs:436)

Example:

```rust
self.state.register_order(client_id, *order_idx);
if let Some(order) = self.state.order_mut(*order_idx) {
    order.set_placed(client_id.clone());
}
```

### Step 3: Runner does know how to turn exchange results into follow-up events

What Happens: `execute_place_batch()` maps accepted batch results to `OrderAccepted` and rejected results to `OrderRejected`.

Input -> Output: exchange batch response should become strategy events.

Where in Code:
- [runner.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/bot-engine/src/runner.rs:991)
- [runner.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/bot-engine/src/runner.rs:1085)
- [runner.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/bot-engine/src/runner.rs:1099)
- [runner.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/bot-engine/src/runner.rs:1142)
- [runner.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/bot-engine/src/runner.rs:1156)

Example:

```rust
match exchange.place_orders(&order_inputs).await {
    Ok(results) => { /* Accepted -> OrderAccepted, Rejected -> OrderRejected */ }
    Err(e) => { /* reject all valid orders */ }
}
```

### Step 4: Startup path executes commands but does not re-dispatch follow-up events

What Happens: during startup, the runner calls `start_strategies()` and `execute_commands(...)`, but unlike the normal event path, it does not put returned follow-up events back through `dispatch_event(...)`.

Input -> Output: startup accept/reject events are created but not consumed by the strategy.

Where in Code:
- [runner.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/bot-engine/src/runner.rs:310)
- [runner.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/bot-engine/src/runner.rs:671)
- [runner.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/bot-engine/src/runner.rs:789)

Example:

```rust
let start_cmds = self.engine.start_strategies();
self.execute_commands(start_cmds).await;
```

Normal path:

```rust
let cmds = self.engine.dispatch_event(&ev);
let followups = self.execute_commands(cmds).await;
for f in followups {
    queue.push_back(f);
}
```

### Step 5: The bot keeps running with no exchange orders and no scheduled retry

What Happens: `on_timer()` can retry pending orders, but this file has no production `set_timer()` or `set_interval()` call. The live loop keeps logging `phase=Active`, `filled=0/5`, `qty=0`, `tp=None`.

Input -> Output: once startup reconciliation is lost, the bot can sit in an "active but empty" state.

Where in Code:
- [strategy.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/strategy-dca/src/strategy.rs:635)
- [strategy.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/strategy-dca/src/strategy.rs:692)
- [strategy.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/strategy-dca/src/strategy.rs:774)

Example:

```rust
fn on_timer(&mut self, ctx: &mut dyn StrategyContext, _timer_id: TimerId) {
    if self.state.pending_orders_count() > 0 {
        self.place_all_dca_orders(ctx);
    }
}
```

## Runtime Evidence

### Startup logs from Loki

These are the high-signal startup lines for bot `353`:

```text
2026-04-15T12:48:46.912046Z ERROR bot: Failed to set leverage: Rate limited (429)
2026-04-15T12:48:47.246302Z  INFO exchange_hyperliquid::client: [init] Connected. Account state fetched successfully.
2026-04-15T12:48:47.246338Z  INFO bot_engine::context: DCAStrategy started: BTC-PERP mode=PERP LONG (buy->sell) base_size=0.00030 dca_size=0.00030 max_orders=4
2026-04-15T12:48:47.246354Z  INFO bot_engine::context: DCA ladder[0]: limit=74000 size=0.00030 (base order)
2026-04-15T12:48:47.246364Z  INFO bot_engine::context: DCA ladder[1]: limit=72668 size=0.00030 (deviation=1.8%)
2026-04-15T12:48:47.246374Z  INFO bot_engine::context: DCA ladder[2]: limit=71032 size=0.00034 (deviation=2.250%)
2026-04-15T12:48:47.246383Z  INFO bot_engine::context: DCA ladder[3]: limit=69035 size=0.00040 (deviation=2.81250%)
2026-04-15T12:48:47.246392Z  INFO bot_engine::context: DCA ladder[4]: limit=66608 size=0.00046 (deviation=3.5156250%)
2026-04-15T12:48:47.246446Z  INFO bot_engine::context: Placing 5 DCA limit orders as batch
2026-04-15T12:48:47.246478Z  INFO bot_engine::runner: Executing batch place_orders with 5 orders
2026-04-15T12:48:47.246583Z  INFO exchange_hyperliquid::client: === PLACING 5 ORDERS (BATCH) ===
2026-04-15T12:48:47.246588Z  INFO exchange_hyperliquid::client:   Order 0: side=BUY, qty=0.00030, price=74000, client_id=0xb29f5228bba84c7c9e636105537b06f4, asset_id=0
2026-04-15T12:48:47.246597Z  INFO exchange_hyperliquid::client:   Order 1: side=BUY, qty=0.00030, price=72668, client_id=0x16234b5069b641248949104112b98717, asset_id=0
2026-04-15T12:48:47.246604Z  INFO exchange_hyperliquid::client:   Order 2: side=BUY, qty=0.00034, price=71032, client_id=0x4dec46460af543b4b58a1fa2976eceaf, asset_id=0
2026-04-15T12:48:47.246611Z  INFO exchange_hyperliquid::client:   Order 3: side=BUY, qty=0.00040, price=69035, client_id=0x9116fedea3914af6b0ed5baac8548389, asset_id=0
2026-04-15T12:48:47.246617Z  INFO exchange_hyperliquid::client:   Order 4: side=BUY, qty=0.00046, price=66608, client_id=0x4343ecdafa23438eb5ef6d93a968639f, asset_id=0
```

Important absence in the same startup window:

- No `Order accepted:` lines
- No `Order rejected:` lines
- No `=== EXCHANGE RESPONSE ===`
- No `HTTP Status: ...`
- No `Response: ...`

That absence matters because the Hyperliquid client normally logs the response body after the HTTP request returns:

- [client.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/exchange-hyperliquid/src/client.rs:274)
- [client.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/exchange-hyperliquid/src/client.rs:292)
- [client.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/exchange-hyperliquid/src/client.rs:293)
- [client.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/exchange-hyperliquid/src/client.rs:849)

### Live-loop logs before stop

Recent bot logs still show the bot active but empty:

```text
2026-04-16T15:27:11.367563Z  INFO bot_engine::context: DCA status: phase=Active mid=Some(Price(74319.5)) filled=0/5 qty=0 avg=None tp=None
2026-04-16T15:27:19.708347Z  INFO bot_engine::runner: [META] pos=BTC-PERP:0.0000 orders=0/0 vol=0.00 fees=0.0000 u_pnl=0.0000
2026-04-16T15:27:30.592365Z  WARN bot_engine::poll_guard: [PollGuard:fills] Transient error, backoff=1000ms: Rate limited (429)
2026-04-16T15:27:30.711753Z  WARN bot_engine::poll_guard: [PollGuard:quotes] Transient error, backoff=1000ms: Rate limited (429)
2026-04-16T15:28:15.113404Z  INFO bot_engine::context: DCA status: phase=Active mid=Some(Price(74310.5)) filled=0/5 qty=0 avg=None tp=None
2026-04-16T15:28:24.617168Z  INFO bot_engine::runner: [META] pos=BTC-PERP:0.0000 orders=0/0 vol=0.00 fees=0.0000 u_pnl=0.0000
2026-04-16T15:28:55.419709Z  INFO bot_engine::runner: [META] pos=BTC-PERP:0.0000 orders=0/0 vol=0.00 fees=0.0000 u_pnl=0.0000
```

## Exchange-Read Proof

Current Hyperliquid read state for wallet `0x0ecba0...10db`:

| Check | Result |
| --- | --- |
| `open_orders` | `0` |
| `assetPositions` | `[]` |
| `withdrawable` | `60.803071` |
| fills since bot creation | `0` |
| matching historical orders for startup cloids | `0` |

Startup `cloid` check:

```json
{
  "0xb29f5228bba84c7c9e636105537b06f4": { "status": "unknownOid" },
  "0x16234b5069b641248949104112b98717": { "status": "unknownOid" },
  "0x4dec46460af543b4b58a1fa2976eceaf": { "status": "unknownOid" },
  "0x9116fedea3914af6b0ed5baac8548389": { "status": "unknownOid" },
  "0x4343ecdafa23438eb5ef6d93a968639f": { "status": "unknownOid" }
}
```

Read-path sanity check:

- A known older cloid, `0xd8388f3c6db94a1aa850b4a35dcd42bc`, does resolve via `query_order_by_cloid(...)` as `status=order` with `status=filled`.
- So the `unknownOid` answers for the five startup cloids look real, not like a broken read script.

## What This Means

| Signal | What it strongly suggests |
| --- | --- |
| Startup logs show full 5-order batch with cloids | The bot definitely attempted startup order placement |
| Hyperliquid shows `unknownOid` for all 5 startup cloids | Those orders never became registered exchange orders |
| No fills since creation | Nothing from that startup ladder executed |
| No open orders / no position | There is nothing live on exchange for this cycle |
| Strategy logs `phase=Active filled=0/5 qty=0` | The bot believes it is active even though the exchange has nothing |

Most likely root cause: startup reconciliation bug in the bot runner, not a "cancel disappeared" issue.

Inference, not proof:

- The startup `429` on leverage shows the exchange session was rate-limited during startup.
- That could also have affected the batch order request.
- But there is no direct response log for the batch, so the exact request-level failure mode is still unproven.

## Why You Never Saw a Cancel on Hyperliquid

Because there is no exchange record of those startup orders.

The visual version:

```text
bot says: "I placed 5 orders"
exchange says: "I never registered those 5 cloids"
result: no live order, no cancel, no fill, no position
```

So the missing cancel is not the first mystery. The first mystery is: why did startup placement not become a real exchange order while the strategy still advanced to `Active`.

## TP Replacement Patch Review

> [!NOTE]
> ### Changes for TP replacement patch
> There is an uncommitted DCA patch in the dirty worktree. It is probably not the main cause of bot `353`, because bot `353` never got the initial ladder onto Hyperliquid. But the patch does introduce a separate risk that should be kept in mind.

Dirty worktree state when this snapshot was taken:

```text
M crates/strategy-dca/src/state.rs
M crates/strategy-dca/src/strategy.rs
?? DCA_TP_REPLACEMENT_PLAN.md
```

What changed in the dirty DCA diff:

- Adds `PendingTakeProfit`
- Adds `tp_cancel_in_flight`
- Adds `pending_tp_replacement`
- Defers TP replacement until cancel acknowledgment arrives

Where in Code:
- [state.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/strategy-dca/src/state.rs:121)
- [state.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/strategy-dca/src/state.rs:179)
- [strategy.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/strategy-dca/src/strategy.rs:200)
- [strategy.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/strategy-dca/src/strategy.rs:230)
- [strategy.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/strategy-dca/src/strategy.rs:451)

Separate bug risk in that patch:

- `update_take_profit_order()` now sets `tp_cancel_in_flight` and waits for `OrderCanceled` before placing replacement TP.
- If `execute_cancel()` fails, the runner only logs a warning and returns `None`.
- That means the strategy may stay stuck with `tp_cancel_in_flight` set forever.

Where in Code:
- [strategy.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/strategy-dca/src/strategy.rs:472)
- [strategy.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/strategy-dca/src/strategy.rs:481)
- [runner.rs](/Users/amitsharma/Desktop/work/botfromscratch/ai-agent/bot/crates/bot-engine/src/runner.rs:1196)

Visual version:

```text
new fill arrives
  -> strategy wants to replace TP
  -> sets "cancel in flight"
  -> cancel call fails once
  -> no cancel event comes back
  -> replacement TP never gets placed
```

Again: that TP issue is real, but it is not the cleanest explanation for bot `353` because this bot never got past startup ladder placement.

## What To Check Right After Restart

1. Capture the new startup `cloid`s from logs immediately.
2. Query `query_order_by_cloid(...)` for those exact new `cloid`s within a few seconds.
3. Check whether startup now shows `Order accepted: ...`, `Order rejected: ...`, or `=== EXCHANGE RESPONSE ===`.
4. If new startup `cloid`s again return `unknownOid`, the startup reconciliation bug remains the top suspect.
5. If new startup `cloid`s do reach Hyperliquid, then move to the TP replacement path next.

## Summary Table

| Question | Best current answer |
| --- | --- |
| Did bot `353` try to create startup orders? | Yes |
| Did those startup orders reach Hyperliquid as real orders? | No evidence; all 5 startup cloids are `unknownOid` |
| Is there anything to cancel on exchange for that startup ladder? | No |
| Why does the bot still look active? | It likely marked orders as placed locally and lost startup follow-up events |
| Is the TP replacement patch the primary cause for this bot? | Probably not |
| Is the TP replacement patch still risky? | Yes, cancel failure can wedge replacement state |
