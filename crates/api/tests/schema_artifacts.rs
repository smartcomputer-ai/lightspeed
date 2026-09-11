//! Asserts the committed `crates/api/contract/` artifacts match the current wire types.
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("contract")
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

fn committed_text(name: &str) -> String {
    let path = schemas_dir().join(name);
    fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "missing committed artifact {}: {error}; run `cargo run -p api --bin export-schema`",
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
        notify_on_terminal: None,
        session_id: "session_1".to_owned(),
        source: RunStartSource::Input {
            items: vec![InputItem::Text {
                origin: None,
                text: "hello".to_owned(),
            }],
        },
        submission_id: Some("retry_1".to_owned()),
        config: None,
    };
    let value = serde_json::to_value(&params).expect("serialize");
    assert_validates(&bundle, "RunStartParams", &value);

    let run = RunView {
        output: None,
        output_text: None,
        id: "run_1".to_owned(),
        status: RunStatus::Completed,
        started_at_ms: Some(10),
        completed_at_ms: Some(20),
        source: RunViewSource::Input {
            items: vec![InputItem::Text {
                origin: None,
                text: "hello".to_owned(),
            }],
        },
        entries: Vec::new(),
        tool_batches: Vec::new(),
        usage: None,
        pending_approvals: Vec::new(),
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
        direction: Default::default(),
        before: None,
        session_id: "session_1".to_owned(),
        after: Some(EventCursor { seq: 42 }),
        limit: Some(100),
        wait_ms: Some(10_000),
    };
    let value = serde_json::to_value(&params).expect("serialize");
    assert_validates(&bundle, "SessionEventsReadParams", &value);
    assert!(
        value.get("direction").is_none(),
        "forward wire shape stays compatible"
    );
    assert!(value.get("before").is_none());
    let old_client: SessionEventsReadParams =
        serde_json::from_value(value).expect("old forward client");
    assert!(old_client.direction.is_forward());
    let backward = SessionEventsReadParams {
        session_id: "session_1".to_owned(),
        direction: api::SessionEventDirection::Backward,
        before: Some(EventCursor { seq: 42 }),
        after: None,
        limit: Some(100),
        wait_ms: None,
    };
    let value = serde_json::to_value(&backward).expect("backward request");
    assert_validates(&bundle, "SessionEventsReadParams", &value);
    assert_eq!(value["direction"], "backward");
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
            "crates/api/contract/{name} is stale; run `cargo run -p api --bin export-schema` and commit the result"
        );
    }
    assert_eq!(
        committed_text("api-reference.md"),
        exported.api_reference,
        "crates/api/contract/api-reference.md is stale; run `cargo run -p api --bin export-schema` and commit the result"
    );
}

#[test]
fn compaction_schema_exposes_editor_field_names() {
    let bundle = committed("api.schema.json");
    let policy = &bundle["definitions"]["CompactionPolicy"];
    let variants = policy["oneOf"].as_array().expect("compaction variants");
    let mut thresholds = 0;
    let mut targets = 0;
    for variant in variants {
        let properties = variant["properties"]
            .as_object()
            .expect("variant properties");
        assert!(!properties.contains_key("compact_threshold_tokens"));
        assert!(!properties.contains_key("target_tokens"));
        thresholds += usize::from(properties.contains_key("compactThresholdTokens"));
        targets += usize::from(properties.contains_key("targetTokens"));
    }
    assert_eq!(thresholds, 2);
    assert_eq!(targets, 1);
    assert_validates(
        &bundle,
        "CompactionPolicy",
        &json!({
            "mode": "providerStandalone", "compactThresholdTokens": 250000, "targetTokens": 100000
        }),
    );
}

#[test]
fn vfs_skills_support_default_roots_and_nonempty_overrides() {
    let bundle = api::export_schemas().schema_bundle;
    let schema = json!({
        "$ref": "#/definitions/VfsSkillsConfig",
        "definitions": bundle["definitions"].clone(),
    });
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(!validator.is_valid(&json!({"roots": []})));
    for enabled in [json!({}), json!({"roots": null})] {
        assert!(validator.is_valid(&enabled));
        let config: api::VfsSkillsConfig = serde_json::from_value(enabled).unwrap();
        assert!(config.roots.is_none());
        assert_eq!(serde_json::to_value(config).unwrap(), json!({}));
    }
    let enabled = json!({"roots": ["/workspace/team-skills"]});
    assert!(validator.is_valid(&enabled));
    let config: api::VfsSkillsConfig = serde_json::from_value(enabled.clone()).unwrap();
    assert_eq!(serde_json::to_value(config).unwrap(), enabled);
    let feature: api::VfsFeature = serde_json::from_value(json!({"tools": "edit"})).unwrap();
    assert!(feature.skills.is_none());
}

#[test]
fn environment_sources_use_domain_directory_and_reject_old_scope_fields() {
    let bundle = api::export_schemas().schema_bundle;
    let config =
        json!({"workingDirectory":"/project", "skills":{}, "prompts":{"roots":["./prompts"]}});
    assert_validates(&bundle, "EnvironmentsFeature", &config);
    let feature: api::EnvironmentsFeature = serde_json::from_value(config).unwrap();
    assert_eq!(feature.working_directory.as_deref(), Some("/project"));
    assert!(feature.skills.unwrap().roots.is_none());
    assert_eq!(feature.prompts.unwrap().roots.unwrap(), vec!["./prompts"]);
    for name in ["EnvironmentPromptsConfig", "EnvironmentSkillsConfig"] {
        assert_validates(&bundle, name, &json!({}));
        assert_validates(&bundle, name, &json!({"roots":["./custom"]}));
    }
    for old in [
        json!({"workingDirectory":"/project"}),
        json!({"projectRoot":"/project"}),
        json!({"additionalRoots":["/team"]}),
    ] {
        assert!(serde_json::from_value::<api::EnvironmentPromptsConfig>(old.clone()).is_err());
        assert!(serde_json::from_value::<api::EnvironmentSkillsConfig>(old).is_err());
    }
}

#[test]
fn environment_tool_grants_are_explicit_and_independent() {
    let bundle = api::export_schemas().schema_bundle;
    let empty: api::EnvironmentsFeature = serde_json::from_value(json!({})).unwrap();
    assert!(empty.tools.is_none());
    assert!(!empty.commands);
    for surface in ["readOnly", "edit"] {
        let value = json!({"tools":surface,"commands":true,"jobs":false,"prompts":{},"skills":{}});
        assert_validates(&bundle, "EnvironmentsFeature", &value);
        let config: api::EnvironmentsFeature = serde_json::from_value(value).unwrap();
        assert!(config.commands);
        assert!(!config.jobs);
        assert!(config.prompts.is_some());
        assert!(config.skills.is_some());
    }
}
