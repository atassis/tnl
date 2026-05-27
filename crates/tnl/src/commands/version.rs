//! `tnl version` — print client version, optionally as JSON.

use serde_json::json;

pub fn run(json: bool) {
    if json {
        println!(
            "{}",
            json!({
                "name": "tnl",
                "version": env!("CARGO_PKG_VERSION"),
            })
        );
    } else {
        println!("tnl {}", env!("CARGO_PKG_VERSION"));
    }
}
