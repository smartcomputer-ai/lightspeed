//! Contract fixtures, schema validation, derivation vectors, and the
//! committed-artifact staleness gate.

use std::{fs, path::PathBuf};

use serde_json::{Value, json};

fn contract_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("contract")
}

fn committed(name: &str) -> Value {
    let path = contract_dir().join(name);
    let text = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "missing committed artifact {}: {error}; run `cargo run -p temporal-workflow --bin export-workflow-contract`",
            path.display()
        )
    });
    serde_json::from_str(&text).expect("committed workflow artifact parses as JSON")
}

fn committed_text(name: &str) -> String {
    let path = contract_dir().join(name);
    fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "missing committed artifact {}: {error}; run `cargo run -p temporal-workflow --bin export-workflow-contract`",
            path.display()
        )
    })
}

fn assert_validates(bundle: &Value, definition: &str, instance: &Value) {
    let schema = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "$ref": format!("#/definitions/{definition}"),
        "definitions": bundle["definitions"].clone(),
    });
    let validator = jsonschema::validator_for(&schema).expect("workflow schema compiles");
    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|error| format!("{} at {}", error, error.instance_path))
        .collect();
    assert!(
        errors.is_empty(),
        "instance does not validate against {definition}: {errors:?}\n{instance:#}"
    );
}

#[test]
fn vector_fixtures_validate_against_every_public_root() {
    let exported = temporal_workflow::workflow_contract::export();
    let vectors = &exported.manifest["vectors"];
    for envelope in vectors["envelopes"].as_array().expect("envelope vectors") {
        assert_validates(&exported.schema_bundle, "EmissionEnvelope", envelope);
    }
    assert_validates(
        &exported.schema_bundle,
        "WorkflowToolStartArgs",
        &vectors["startArgs"],
    );
    assert_validates(
        &exported.schema_bundle,
        "WorkflowToolRecoveryResult",
        &vectors["recoveryResult"],
    );
    assert_validates(
        &exported.schema_bundle,
        "WorkflowToolRecipeV1",
        &vectors["recipe"],
    );
}

#[test]
fn vectors_cover_all_derivations_and_round_trips() {
    let exported = temporal_workflow::workflow_contract::export();
    let vectors = &exported.manifest["vectors"];
    assert_eq!(
        vectors["emissionIds"].as_object().map(|ids| ids.len()),
        Some(4)
    );
    assert!(
        vectors["recipeFingerprint"]
            .as_str()
            .is_some_and(|value| value.starts_with("wtr:sha256:"))
    );
    assert_eq!(
        vectors["workflowIds"]["split"]["sessionId"],
        vectors["inputs"]["sessionId"]
    );
    for envelope in vectors["envelopes"].as_array().expect("envelope vectors") {
        let decoded: harness::EmissionEnvelope =
            serde_json::from_value(envelope.clone()).expect("vector envelope decodes");
        assert_eq!(
            serde_json::to_value(decoded).expect("vector re-encodes"),
            *envelope
        );
    }
}

#[test]
fn committed_workflow_contract_is_current() {
    let exported = temporal_workflow::workflow_contract::export();
    assert_eq!(
        committed("workflow.schema.json"),
        exported.schema_bundle,
        "workflow.schema.json is stale; run `cargo run -p temporal-workflow --bin export-workflow-contract`"
    );
    assert_eq!(
        committed("workflow.json"),
        exported.manifest,
        "workflow.json is stale; run `cargo run -p temporal-workflow --bin export-workflow-contract`"
    );
    assert_eq!(
        committed_text("workflow-contract.md"),
        exported.reference,
        "workflow-contract.md is stale; run `cargo run -p temporal-workflow --bin export-workflow-contract`"
    );
}
