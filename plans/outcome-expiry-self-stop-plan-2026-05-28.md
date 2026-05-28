# Outcome Expiry Self-Stop Plan

## Scope

Outcome bots must not trade after the Hyperliquid outcome market expires.

## Minimal Change

1. At runtime startup, fetch `outcomeMeta` only for outcome configs.
2. Resolve expiry from `outcomes[].description`, then parent `questions[].description`.
3. If HL expiry is missing, use explicit top-level `outcome_expiry`.
4. If live/paper outcome expiry is still unknown, fail startup.
5. Pass resolved epoch milliseconds into `RunnerConfig`.
6. Runner stops cleanly when `now_ms >= stop_at_ms`, reusing existing `on_stop`, `CancelAll`, and final sync.

## Non-Goals

- No behavior changes for perp, spot, or HIP-3 bots.
- No new scheduler/service.
- No strategy-specific expiry logic.
