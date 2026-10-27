//! The committed `contract/` documents match the current wire types. When
//! this fails, regenerate with
//! `cargo run -p environment-protocol --features schema --bin export-contract`
//! and commit the result with the type change.

use std::{fs, path::PathBuf};

use serde_json::Value;

#[test]
fn committed_contract_is_current() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("contract");
    for plane in environment_protocol::contract::export() {
        for (name, current) in [
            (plane.schema_file_name(), &plane.schema_bundle),
            (plane.openrpc_file_name(), &plane.openrpc),
        ] {
            let path = dir.join(&name);
            let text = fs::read_to_string(&path).unwrap_or_else(|error| {
                panic!(
                    "missing {}: {error}; run the export-contract binary",
                    path.display()
                )
            });
            let committed: Value = serde_json::from_str(&text).expect("contract parses");
            assert_eq!(
                &committed, current,
                "{name} is stale; run the export-contract binary"
            );
        }
    }
}

#[test]
fn every_method_appears_in_its_plane_document() {
    for plane in environment_protocol::contract::export() {
        let names: Vec<&str> = plane.openrpc["methods"]
            .as_array()
            .expect("methods")
            .iter()
            .filter_map(|method| method["name"].as_str())
            .collect();
        for spec in environment_protocol::manifest::plane_methods(plane.plane) {
            assert!(names.contains(&spec.name), "{} missing", spec.name);
        }
    }
}
