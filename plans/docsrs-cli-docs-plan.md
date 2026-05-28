# docs.rs CLI Documentation Plan

## Finding

`hyperliquid-trading-bot` publishes the package from `crates/bot-cli`, but docs.rs renders the library target named `bot_cli`.

The current library root only says it exports config types, while the actual CLI behavior lives in `src/main.rs`. Rustdoc does not turn binary help text into product documentation.

## Change

- Add a package README inside `crates/bot-cli`.
- Use that README as the crate-level rustdoc via `include_str!`.
- Include the README in the packaged crate.
- Bump only the CLI package to `0.1.1` for a docs-only patch release.

## Verification

- Run `cargo package -p hyperliquid-trading-bot --allow-dirty --list`.
- Run `cargo publish -p hyperliquid-trading-bot --dry-run --allow-dirty`.
- Do not publish `0.1.1` without explicit confirmation.
