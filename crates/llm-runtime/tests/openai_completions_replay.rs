//! Exercise response replay through actual engine-assigned context provenance.

use engine::{
    BlobRef, ContextEntry, ContextEntryId, ContextEntryInput, ContextEntryKind, ContextEntrySource,
    ContextMessageRole, ModelSelection, ProviderApiKind, RunId, SessionId, ToolCallId, TurnId,
    storage::InMemoryBlobStore,
};
use llm_clients::{ApiResponse, HeaderSnapshot};
use llm_runtime::openai_completions::{materialize_create_request, result_from_response};
use serde_json::{Value, json};

fn entry(
    id: u64,
    kind: ContextEntryKind,
    source: ContextEntrySource,
    content_ref: BlobRef,
) -> ContextEntry {
    ContextEntry {
        entry_id: ContextEntryId::new(id),
        key: None,
        kind,
        source,
        content: engine::ContentRef {
            content_ref,
            media_type: Some("text/plain".to_owned()),
            provider_kind: None,
        },
        preview: None,
        origin: None,
        provenance_ref: None,
        token_estimate: None,
        supersedes: None,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn engine_completion_history_reconstructs_one_message_and_preserves_request_prefixes() {
    fn commit(drive: &mut engine::CoreAgentDrive, action: engine::CoreAgentAction) {
        let engine::CoreAgentAction::AppendEvents {
            expected_head,
            events,
        } = action
        else {
            panic!("expected events");
        };
        let first = expected_head.map_or(1, |head| head.seq.as_u64() + 1);
        drive
            .resume_appended(
                events
                    .into_iter()
                    .enumerate()
                    .map(|(index, event)| engine::session::StoredSessionEntry {
                        position: engine::session::SessionPosition {
                            seq: engine::session::EventSeq::new(first + index as u64),
                        },
                        observed_at_ms: event.observed_at_ms,
                        joins: event.joins,
                        event: event.event,
                    })
                    .collect(),
            )
            .expect("commit events");
    }

    let blobs = InMemoryBlobStore::new();
    let mut drive = engine::CoreAgentDrive::from_replayed(
        SessionId::new("completion-replay"),
        engine::CoreAgentState::new(),
        None,
    );
    let action = drive
        .admit_command(
            engine::CoreAgentCommand::OpenSession {
                config: engine::SessionConfig {
                    model: ModelSelection {
                        api_kind: ProviderApiKind::OpenAiCompletions,
                        provider_id: "openai".to_owned(),
                        model: "gpt-5.1".to_owned(),
                    },
                    generation: Default::default(),
                    limits: Default::default(),
                    context: engine::ContextConfig { compaction: None },
                    features: Default::default(),
                },
            },
            1,
        )
        .expect("open session");
    commit(&mut drive, action);
    let action = drive
        .admit_command(
            engine::CoreAgentCommand::RequestRun(engine::RunRequestCommand {
                submission_id: None,
                notify_on_terminal: Vec::new(),
                run_config: Default::default(),
                source: engine::RunRequestSource::Input {
                    input: vec![ContextEntryInput {
                        kind: ContextEntryKind::Message {
                            role: ContextMessageRole::User,
                        },
                        content: engine::ContentRef::text(
                            blobs.insert_text("Inspect the workspace").await,
                        ),
                        preview: None,
                        origin: None,
                        provenance_ref: None,
                        token_estimate: None,
                    }],
                },
            }),
            2,
        )
        .expect("request run");
    commit(&mut drive, action);
    let mut generation = None;
    for tick in 3..30 {
        match drive.next_action(tick, 64).expect("drive") {
            engine::CoreAgentAction::GenerateLlm { request } => {
                generation = Some(request);
                break;
            }
            action => commit(&mut drive, action),
        }
    }
    let generation = generation.expect("generation request");
    let initial = materialize_create_request(&blobs, &generation.request)
        .await
        .expect("initial request");
    let tool = json!({"id":"call_1", "type":"function", "function": {
        "name":"lookup", "arguments":"{ \"path\": \"/\" }"
    }});

    // The request policy deliberately lowers visible content to text and
    // excludes response metadata. Reasoning/signatures and calls must still
    // rejoin that message exactly, using provenance assigned by the engine.
    for (content, refusal, expected_content, with_tools) in [
        (json!("Checking"), Value::Null, json!("Checking"), true),
        (
            json!([{"type":"text", "text":"Hello "}, {"type":"text", "text":"world"}]),
            Value::Null,
            json!("Hello world"),
            true,
        ),
        (
            Value::Null,
            json!("Cannot do that"),
            json!("Cannot do that"),
            false,
        ),
        (Value::Null, Value::Null, Value::Null, true),
        (json!("Done"), Value::Null, json!("Done"), false),
    ] {
        let mut native = json!({
            "role":"assistant", "content":content, "refusal":refusal,
            "reasoning_content":"exact reasoning", "reasoning":"compatible reasoning",
            "reasoning_details":[{"type":"reasoning.text", "text":"exact", "signature":"sig_1"}],
            "response_metadata":{"retained":true}
        });
        if !content.is_null() || !refusal.is_null() {
            native["annotations"] = json!([]);
        }
        if with_tools {
            native["tool_calls"] = json!([tool.clone()]);
        }
        let raw = json!({"id":"completion", "choices":[{
            "index":0, "finish_reason": if with_tools { "tool_calls" } else { "stop" },
            "message":native
        }]});
        let response = ApiResponse {
            parsed: serde_json::from_value(raw.clone()).expect("response"),
            raw_json: raw,
            status: 200,
            headers: HeaderSnapshot::default(),
        };
        let result = result_from_response(&blobs, &generation, &response)
            .await
            .expect("convert");
        let proposals =
            engine::generation_result_proposals(drive.state(), result).expect("engine provenance");
        let retained = proposals
            .into_iter()
            .find_map(|proposal| match proposal.event {
                engine::CoreAgentEvent::Context(engine::ContextEvent::EntriesApplied {
                    entries,
                    ..
                }) => Some(entries),
                _ => None,
            })
            .expect("context entries");
        assert!(
            retained
                .iter()
                .any(|entry| matches!(entry.source, ContextEntrySource::Reasoning { .. }))
        );
        let mut followup = generation.request.clone();
        followup.context.entries.extend(retained.clone());
        let replay = materialize_create_request(&blobs, &followup)
            .await
            .expect("reconstruct");
        assert_eq!(
            &replay.messages[..initial.messages.len()],
            &initial.messages
        );
        assert_eq!(replay.messages.len(), initial.messages.len() + 1);
        let mut expected = json!({
            "role":"assistant", "content":expected_content,
            "reasoning_content":native["reasoning_content"], "reasoning":native["reasoning"],
            "reasoning_details":native["reasoning_details"]
        });
        if !refusal.is_null() {
            expected["refusal"] = refusal;
        }
        if with_tools {
            expected["tool_calls"] = native["tool_calls"].clone();
        }
        assert_eq!(
            serde_json::to_value(replay.messages.last().unwrap()).unwrap(),
            expected
        );

        let (kind, source) = if with_tools {
            (
                ContextEntryKind::ToolResult {
                    call_id: ToolCallId::new("call_1"),
                    is_error: false,
                },
                ContextEntrySource::Tool {
                    run_id: generation.run_id,
                    turn_id: generation.turn_id,
                    batch_id: None,
                },
            )
        } else {
            (
                ContextEntryKind::Message {
                    role: ContextMessageRole::User,
                },
                ContextEntrySource::RunInput {
                    run_id: RunId::new(2),
                    input_index: 0,
                },
            )
        };
        followup.context.entries.push(entry(
            100,
            kind,
            source,
            blobs.insert_text("Continue").await,
        ));
        let with_result = materialize_create_request(&blobs, &followup)
            .await
            .expect("tool followup");
        assert_eq!(
            &with_result.messages[..replay.messages.len()],
            &replay.messages
        );

        // Adjacent turns (including a new run with a reused turn number)
        // must never absorb one another's reasoning or tool calls.
        for (run_id, turn_id) in [
            (generation.run_id, TurnId::new(2)),
            (RunId::new(2), generation.turn_id),
        ] {
            let mut adjacent = retained.clone();
            adjacent.extend(retained.iter().cloned().map(|mut entry| {
                entry.source = match entry.kind {
                    ContextEntryKind::ReasoningState => {
                        ContextEntrySource::Reasoning { run_id, turn_id }
                    }
                    _ => ContextEntrySource::AssistantOutput { run_id, turn_id },
                };
                entry
            }));
            let mut adjacent_request = generation.request.clone();
            adjacent_request.context.entries = adjacent;
            let replay = materialize_create_request(&blobs, &adjacent_request)
                .await
                .expect("adjacent turns");
            assert_eq!(
                serde_json::to_value(replay.messages).unwrap(),
                json!([expected.clone(), expected.clone()])
            );
        }
    }
}
