//! Asserts the committed `interop/contract/` artifacts match the current wire types.
//!
//! When these fail, regenerate with `cargo run -p api --bin export-schema`
//! and commit the result alongside the type change.

use std::{fs, path::PathBuf};

use api::{
    AgentApiOutcome, AgentNotification, EventCursor, InputItem, RunStartParams, RunStartResponse,
    RunStartSource, RunStatus, RunView, RunViewSource, SessionEventsReadParams,
};
use serde_json::{Value, json};

fn schemas_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../interop/contract")
}

fn committed(name: &str) -> Value {
    let path = schemas_dir().join(name);
    let text = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "missing committed artifact {}: {error}; run `cargo run -p api --bin export-schema`",
            path.display()
        )
    });
    serde_json::from_str(&text).expect("committed artifact parses as JSON")
}

fn assert_validates(bundle: &Value, definition: &str, instance: &Value) {
    let schema = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "$ref": format!("#/definitions/{definition}"),
        "definitions": bundle["definitions"].clone(),
    });
    let validator = jsonschema::validator_for(&schema).expect("schema compiles");
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
fn serialized_fixtures_validate_against_exported_schemas() {
    let bundle = api::export_schemas().schema_bundle;

    let params = RunStartParams {
        session_id: "session_1".to_owned(),
        source: RunStartSource::Input {
            items: vec![InputItem::Text {
                text: "hello".to_owned(),
            }],
        },
        submission_id: Some("retry_1".to_owned()),
        config: None,
    };
    let value = serde_json::to_value(&params).expect("serialize");
    assert_validates(&bundle, "RunStartParams", &value);

    let run = RunView {
        id: "run_1".to_owned(),
        status: RunStatus::Completed,
        source: RunViewSource::Input {
            items: vec![InputItem::Text {
                text: "hello".to_owned(),
            }],
        },
        entries: Vec::new(),
        tool_batches: Vec::new(),
    };
    let outcome = AgentApiOutcome::with_notifications(
        RunStartResponse { run: run.clone() },
        vec![AgentNotification::RunStarted {
            session_id: "session_1".to_owned(),
            run,
        }],
    );
    let value = serde_json::to_value(&outcome).expect("serialize");
    assert_validates(&bundle, "AgentApiOutcomeOfRunStartResponse", &value);

    let params = SessionEventsReadParams {
        session_id: "session_1".to_owned(),
        after: Some(EventCursor { seq: 42 }),
        limit: Some(100),
        wait_ms: Some(10_000),
    };
    let value = serde_json::to_value(&params).expect("serialize");
    assert_validates(&bundle, "SessionEventsReadParams", &value);
}

#[test]
fn committed_schema_artifacts_are_current() {
    let exported = api::export_schemas();
    let artifacts = [
        ("api.schema.json", &exported.schema_bundle),
        ("methods.json", &exported.methods),
        ("openrpc.json", &exported.openrpc),
    ];
    for (name, current) in artifacts {
        assert_eq!(
            &committed(name),
            current,
            "interop/contract/{name} is stale; run `cargo run -p api --bin export-schema` and commit the result"
        );
    }
}
