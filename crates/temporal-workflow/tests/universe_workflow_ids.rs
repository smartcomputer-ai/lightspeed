//! Every runtime-owned execution must be discoverable by a universe-prefix query.
use temporal_workflow::*;
use uuid::Uuid;

#[test]
fn every_execution_composer_uses_the_universe_prefix() {
    let universe = Uuid::from_u128(42);
    let transcription = TranscriptionWorkflowArgs {
        universe_id: universe,
        transcription_id: "transcription_one".into(),
        created_by: api::Attribution::Local,
        request: api::TranscriptionStartParams {
            idempotency_key: "request".into(),
            audio: api::TranscriptionAudio {
                blob_ref: harness::BlobRef::from_bytes(b"audio").to_string(),
                mime: "audio/wav".into(),
                name: "audio.wav".into(),
            },
            model: None,
            language: None,
            prompt: None,
        },
        model: api::ModelConfig {
            provider_id: "provider".into(),
            api_kind: "transcription".into(),
            model: "model".into(),
        },
        created_at_ms: 0,
    };
    let ids = [
        compose_workflow_id(universe, &harness::SessionId::new("session")),
        compose_environment_job_workflow_id(universe, "environment", "group"),
        compose_workflow_tool_execution_id(universe, "wtx:sha256:execution"),
        ::bots::bot_controller_workflow_id(universe, &api::BotId::new("bot")),
        ::bots::bot_trigger_fire_workflow_id(
            universe,
            &api::BotId::new("bot"),
            &api::BotTriggerId::new("trigger"),
        ),
        transcription_workflow_id(&transcription),
    ];
    for id in ids {
        assert!(
            id.starts_with(&format!("{universe}/")),
            "execution is outside the universe: {id}"
        );
    }
}

#[test]
fn logical_execution_identity_is_preserved_inside_the_temporal_prefix() {
    let universe = Uuid::from_u128(42);
    let logical = harness::workflow_tool_execution_id(
        &harness::WorkflowToolInvocationId::new(format!("wti:sha256:{}", "a".repeat(64))),
        "recipe",
    );
    assert_eq!(
        compose_workflow_tool_execution_id(universe, &logical),
        format!("{universe}/{logical}")
    );
    assert_ne!(
        compose_workflow_tool_execution_id(universe, &logical),
        compose_workflow_tool_execution_id(Uuid::from_u128(43), &logical)
    );
}
