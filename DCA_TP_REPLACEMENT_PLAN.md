# DCA TP Replacement Plan

## Goal

Fix the DCA take-profit replacement flow so a new TP order is placed only after the old TP cancel is acknowledged.

## Observed Failure

- DCA strategy emits `cancel_order(old_tp)` and `place_order(new_tp)` in the same event pass.
- Engine drains and executes `PlaceOrder` before `CancelOrder`.
- On spot, the old TP can still reserve base inventory when the new TP is submitted.
- Exchange rejects the replacement TP with `Insufficient spot balance`, and the engine stops the bot.

## Minimal Safe Change

- Keep the fix inside `strategy-dca`.
- Add explicit TP replacement state:
  - active TP order id
  - TP cancel-in-flight marker
  - pending replacement TP spec
- On fill:
  - if no active TP exists, place TP immediately
  - if active TP exists, store the new TP spec, send only cancel, and wait
- On `OrderCanceled` for the active TP:
  - clear cancel-in-flight
  - place the latest pending TP replacement
- On TP reject:
  - clear active TP tracking
  - keep current retry-on-next-fill behavior
- On cycle completion / reset:
  - clear all TP replacement state

## Why This Scope

- Avoids engine-wide command ordering changes.
- Limits behavioral change to DCA TP replacement only.
- Uses already working `OrderCanceled` wiring.

## Verification

- Add a focused regression test at strategy level:
  - existing TP present
  - new fill triggers TP update
  - assert no replacement TP is placed before cancel ack
  - assert replacement TP is placed after `OrderCanceled`
- Run targeted `strategy-dca` tests after patching.
