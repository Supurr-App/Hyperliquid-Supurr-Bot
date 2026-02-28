//! Schema generator binary.
//!
//! Generates JSON Schema from Rust types for use with external tools.
//!
//! ## Usage
//!
//! Generate to stdout:
//!   cargo run --bin schema
//!
//! Generate and copy to bot_api:
//!   cargo run --bin schema -- --copy
//!
//! The schema includes the full V2 BotConfig structure used by both
//! the CLI and the Bot API.

use bot_cli::config::BotConfig;
use schemars::schema_for;
use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let schema = schema_for!(BotConfig);
    let json = serde_json::to_string_pretty(&schema).unwrap();

    // Check for --copy flag
    let args: Vec<String> = env::args().collect();
    let should_copy = args.iter().any(|a| a == "--copy");

    if should_copy {
        // Get the bot crate root directory
        let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
        let crate_root = PathBuf::from(&manifest_dir);

        // bot_api is at ../../bot_api relative to bot/crates/bot-cli
        let bot_api_schema_path =
            crate_root.join("../../../bot_api/src/schemas/bot-config.schema.json");

        // Also save in the bot crate root for reference
        let bot_schema_path = crate_root.join("../../schemas/bot-config.schema.json");

        // Write schema to stdout
        println!("{}", json);

        // Write to bot_api
        if let Some(parent) = bot_api_schema_path.parent() {
            fs::create_dir_all(parent).ok();
        }
        match fs::write(&bot_api_schema_path, &json) {
            Ok(_) => eprintln!("✓ Copied schema to: {}", bot_api_schema_path.display()),
            Err(e) => eprintln!("✗ Failed to write to bot_api: {}", e),
        }

        // Write to bot/schemas/
        if let Some(parent) = bot_schema_path.parent() {
            fs::create_dir_all(parent).ok();
        }
        match fs::write(&bot_schema_path, &json) {
            Ok(_) => eprintln!("✓ Copied schema to: {}", bot_schema_path.display()),
            Err(e) => eprintln!("✗ Failed to write to bot/schemas: {}", e),
        }
    } else {
        // Just print to stdout
        println!("{}", json);
        eprintln!("\nTip: Use --copy to automatically copy to bot_api/src/schemas/");
    }
}
