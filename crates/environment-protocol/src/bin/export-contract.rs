//! Writes the committed contract under `crates/environment-protocol/contract/`.
//!
//! Usage: `cargo run -p environment-protocol --features schema --bin export-contract [output-dir]`

use std::{env, fs, path::PathBuf};

fn main() {
    let out_dir = env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("contract"));
    fs::create_dir_all(&out_dir).expect("create output directory");
    for plane in environment_protocol::contract::export() {
        for (name, value) in [
            (plane.schema_file_name(), &plane.schema_bundle),
            (plane.openrpc_file_name(), &plane.openrpc),
        ] {
            let mut text = serde_json::to_string_pretty(value).expect("serialize contract");
            text.push('\n');
            let path = out_dir.join(name);
            fs::write(&path, text).expect("write contract");
            println!("wrote {}", path.display());
        }
    }
}
