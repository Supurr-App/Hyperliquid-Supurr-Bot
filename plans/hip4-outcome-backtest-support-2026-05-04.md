---
agent_scope: Plan for making HIP4 outcome backtests execute through the existing Rust/Supurr backtest path without touching live trading.
DO_NOT:
  - Cover live HIP4 deployment safety; that belongs in runtime/live-trading plans.
  - Cover production image rollout; that belongs in FastClaw deployment plans.
SEE ALSO:
  - bot/crates/bot-cli/src/main.rs
  - bot/crates/bot-engine/src/testing/paper_exchange.rs
  - bot/crates/bot-engine/src/testing/fill_simulator.rs
  - supurr_cli/src/lib/backtest-runner.ts
---

# HIP4 Outcome Backtest Support

One-line read: outcome backtests already start, but the simulation still thinks in BTC-style USDC terms.

| Gap | Fix | Verification |
| --- | --- | --- |
| Outcome quotes use `$1` spread | Use outcome tick-size spread | Outcome synthetic prices can cross grid levels |
| Backtest seeds only `USDC` | Seed primary market quote asset | Outcome gets `USDH`; perps stay `USDC` |
| Outcome short backtests need sell-side inventory | Seed spot-like base inventory in backtest only for short/neutral grids | Outcome SELL opens can fill, then BUY closes can fire |
| Paper simulator hardcodes `USDC` | Resolve quote/base from registered instrument metadata | Spot/USDH and outcome accounting stop depending on string guesses |
| CLI `-o` ignored | Write raw engine JSON to requested path | `supurr backtest -o file.json` creates parseable JSON |
| Sample outcome configs miss `base`/`quote` | Add CLI-facing fields | Supurr CLI accepts repo sample configs |

Safety boundary:
- No live orders.
- No deployment.
- Perp/spot behavior must remain covered by existing tests plus direct backtest smoke.
