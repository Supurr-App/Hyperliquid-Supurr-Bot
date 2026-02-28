---
name: backward-compatibility-check
description: Gives ability to agent to decide whether the new changes are backward compatible or not
---

Our deployement process is via bot_api's k8s.ts file which spins bot with rust binaries executing "--config /app/config.json".

So what we want in backward compatibleness is everything which was working earlier

- Bot spinning
- CLI parsing
- Runner spinning
- Exchange spinning
- Engine spinning and integration with exchange (hyperliquid-exchange) to be specific.
- Strategy spinning and integration with engine
- All fills polling via exchange through engine
- pnl Tracking
- commands exposed to strategies working accurately
- stop bot path & working

And all other miscs which were working earlier should remain working as it is with 0% bugs (especially unknown ones). They should remain working as it is.
For this you can run tests and all & use your own discretion.

We are also doing wasm backtesting which is very prone to breaking changes because of our wasm build pipeline. Some of bot-deps are not wasm compatible yet. So we have to be careful about that.

Also rust cli is directly accessed by our supurr_cli (bun based cli) so take care of that too, any commands in cli shouldn't break or have any unintentional side effects.
