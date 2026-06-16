//! Writes the committed wire-contract artifacts under `interop/contract/`.
//!
//! Usage: `cargo run -p api --bin export-schema [output-dir]`

use std::{env, fs, path::PathBuf};

fn main() {
    let out_dir = env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../interop/contract")
    });
    fs::create_dir_all(&out_dir).expect("create output directory");

    let exported = api::export_schemas();
    let artifacts = [
        ("api.schema.json", &exported.schema_bundle),
        ("methods.json", &exported.methods),
        ("openrpc.json", &exported.openrpc),
    ];
    for (name, value) in artifacts {
        let mut text = serde_json::to_string_pretty(value).expect("serialize artifact");
        text.push('\n');
        let path = out_dir.join(name);
        fs::write(&path, text).expect("write artifact");
        println!("wrote {}", path.display());
    }
}
