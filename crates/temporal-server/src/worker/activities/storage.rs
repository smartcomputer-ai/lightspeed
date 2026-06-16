use engine::{
    AgentHandle, BlobRef, SessionId, SessionPosition,
    storage::{
        AppendSessionEvents, AppendSessionEventsResult, CreateSession, DynamicSessionEntry,
        DynamicUncommittedSessionEvent, ReadSessionEvents, SessionStore, SessionStoreError,
    },
};
use temporal_workflow::{
    DEFAULT_BOOTSTRAP_PAYLOAD_BUDGET_BYTES, SessionBootstrapPayloadTooLarge, reduce_session_entries,
};
use temporalio_sdk::activities::ActivityError;

use crate::worker::{
    AppendEventsRequest, CreateOrLoadSessionRequest, CreateOrLoadSessionResult, PutBlobRequest,
    ReadBlobRequest, ReadBlobResult,
};

use super::{common::activity_error, state::StorageActivityDeps};

pub(super) async fn create_or_load_session(
    deps: &StorageActivityDeps,
    request: CreateOrLoadSessionRequest,
) -> Result<CreateOrLoadSessionResult, ActivityError> {
    engine::storage::ensure_engine_blobs(deps.blobs.as_ref())
        .await
        .map_err(activity_error)?;
    let record = match deps
        .sessions
        .create_session(CreateSession {
            session_id: request.session_id.clone(),
            agent_handle: AgentHandle::new("lightspeed.agent"),
            created_at_ms: request.observed_at_ms,
        })
        .await
    {
        Ok(record) => record,
        Err(SessionStoreError::SessionAlreadyExists { .. }) => deps
            .sessions
            .load_session(&request.session_id)
            .await
            .map_err(activity_error)?
            .ok_or_else(|| {
                activity_error(anyhow::anyhow!(
                    "session disappeared after create conflict: {}",
                    request.session_id
                ))
            })?,
        Err(error) => return Err(activity_error(error)),
    };

    // Reduce the durable log *inside the activity* and return only the compact
    // state, so the full event log never crosses the activity boundary into
    // Temporal history.
    let entries = read_all_session_events(deps.sessions.as_ref(), &request.session_id).await?;
    let reduced = reduce_session_entries(&entries).map_err(activity_error)?;
    let head = record.head.clone();
    let (core_state, replayed_event_count) = if reduced.replayed_event_count == 0 {
        (None, 0)
    } else {
        (Some(reduced.core_state), reduced.replayed_event_count)
    };

    let result = CreateOrLoadSessionResult {
        record,
        core_state,
        run_submissions: reduced.run_submissions,
        head,
        replayed_event_count,
    };

    guard_bootstrap_payload_size(&request.session_id, &result)?;
    Ok(result)
}

/// Fail with a typed, diagnosable error if the compact bootstrap result would
/// still exceed the Temporal payload budget — instead of letting Temporal
/// reject the activity completion with an opaque `Complete result exceeds size
/// limit`.
fn guard_bootstrap_payload_size(
    session_id: &SessionId,
    result: &CreateOrLoadSessionResult,
) -> Result<(), ActivityError> {
    guard_bootstrap_payload_size_with_budget(
        session_id,
        result,
        DEFAULT_BOOTSTRAP_PAYLOAD_BUDGET_BYTES,
    )
}

fn guard_bootstrap_payload_size_with_budget(
    session_id: &SessionId,
    result: &CreateOrLoadSessionResult,
    budget_bytes: u64,
) -> Result<(), ActivityError> {
    let serialized = serde_json::to_vec(result).map_err(activity_error)?;
    let reduced_state_bytes = serialized.len() as u64;
    if reduced_state_bytes > budget_bytes {
        return Err(activity_error(SessionBootstrapPayloadTooLarge {
            session_id: session_id.clone(),
            reduced_state_bytes,
            budget_bytes,
            replayed_event_count: result.replayed_event_count,
        }));
    }
    Ok(())
}

pub(super) async fn put_blob(
    deps: &StorageActivityDeps,
    request: PutBlobRequest,
) -> Result<BlobRef, ActivityError> {
    deps.blobs
        .put_bytes(request.bytes)
        .await
        .map_err(activity_error)
}

pub(super) async fn read_blob(
    deps: &StorageActivityDeps,
    request: ReadBlobRequest,
) -> Result<ReadBlobResult, ActivityError> {
    let bytes = deps
        .blobs
        .read_bytes(&request.blob_ref)
        .await
        .map_err(activity_error)?;
    Ok(ReadBlobResult { bytes })
}

pub(super) async fn append_events(
    deps: &StorageActivityDeps,
    request: AppendEventsRequest,
) -> Result<AppendSessionEventsResult, ActivityError> {
    let append = AppendSessionEvents {
        session_id: request.session_id.clone(),
        expected_head: request.expected_head.clone(),
        events: request.events.clone(),
    };
    match deps.sessions.append(append).await {
        Ok(result) => Ok(result),
        Err(error @ SessionStoreError::ExpectedHeadMismatch { .. })
            if !request.events.is_empty() =>
        {
            confirm_existing_append(deps.sessions.as_ref(), &request, error)
                .await
                .map_err(activity_error)
        }
        Err(error) => Err(activity_error(error)),
    }
}

async fn confirm_existing_append(
    store: &dyn SessionStore,
    request: &AppendEventsRequest,
    original_error: SessionStoreError,
) -> Result<AppendSessionEventsResult, SessionStoreError> {
    let page = store
        .read_after(ReadSessionEvents {
            session_id: request.session_id.clone(),
            after: request.expected_head.as_ref().map(|position| position.seq),
            limit: request.events.len(),
        })
        .await?;
    if !committed_entries_match_request(&request.expected_head, &page.entries, &request.events) {
        return Err(original_error);
    }

    Ok(AppendSessionEventsResult {
        head: page.entries.last().map(|entry| entry.position.clone()),
        entries: page.entries,
    })
}

fn committed_entries_match_request(
    expected_head: &Option<SessionPosition>,
    entries: &[DynamicSessionEntry],
    events: &[DynamicUncommittedSessionEvent],
) -> bool {
    if entries.len() != events.len() {
        return false;
    }

    let mut previous_seq = expected_head
        .as_ref()
        .map(|position| position.seq.as_u64())
        .unwrap_or(0);
    entries.iter().zip(events).all(|(entry, event)| {
        let expected_seq = previous_seq.saturating_add(1);
        let matches = entry.position.seq.as_u64() == expected_seq
            && entry.observed_at_ms == event.observed_at_ms
            && entry.joins == event.joins
            && entry.event == event.event;
        previous_seq = expected_seq;
        matches
    })
}

async fn read_all_session_events(
    store: &dyn SessionStore,
    session_id: &SessionId,
) -> Result<Vec<DynamicSessionEntry>, ActivityError> {
    let mut after = None;
    let mut entries = Vec::new();
    loop {
        let page = store
            .read_after(ReadSessionEvents {
                session_id: session_id.clone(),
                after,
                limit: 512,
            })
            .await
            .map_err(activity_error)?;
        after = page.next_after;
        entries.extend(page.entries);
        if page.complete {
            return Ok(entries);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc};

    use engine::{
        DynamicEvent,
        storage::{BlobStore, InMemoryBlobStore, InMemorySessionStore, SessionPage, SessionRecord},
    };
    use serde_json::json;

    use super::*;

    fn test_event(
        observed_at_ms: u64,
        joins: impl IntoIterator<Item = (&'static str, &'static str)>,
        payload: serde_json::Value,
    ) -> DynamicUncommittedSessionEvent {
        DynamicUncommittedSessionEvent {
            observed_at_ms,
            joins: joins
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect::<BTreeMap<_, _>>(),
            event: DynamicEvent::new("lightspeed.test.event", 1, payload),
        }
    }

    async fn create_test_session(store: &InMemorySessionStore) -> SessionId {
        let session_id = SessionId::new("session-a");
        store
            .create_session(CreateSession {
                session_id: session_id.clone(),
                agent_handle: AgentHandle::new("lightspeed.test"),
                created_at_ms: 1,
            })
            .await
            .expect("create session");
        session_id
    }

    async fn read_all(store: &InMemorySessionStore, session_id: &SessionId) -> SessionPage {
        store
            .read_after(ReadSessionEvents {
                session_id: session_id.clone(),
                after: None,
                limit: 16,
            })
            .await
            .expect("read session events")
    }

    /// Minimal LLM stub: the bootstrap-volume test never requests a run, so
    /// generation must never be reached.
    struct UnreachableLlm;

    #[async_trait::async_trait]
    impl engine::CoreAgentLlm for UnreachableLlm {
        async fn generate(
            &self,
            _request: engine::LlmGenerationRequest,
        ) -> Result<engine::LlmGenerationResult, engine::CoreAgentIoError> {
            panic!("bootstrap-volume test must not generate")
        }
    }

    fn volume_session_config() -> engine::SessionConfig {
        temporal_workflow::default_session_config(engine::ModelSelection {
            api_kind: engine::ProviderApiKind::OpenAiResponses,
            provider_id: "openai".to_owned(),
            model: "gpt-test".to_owned(),
        })
    }

    /// Regression: a session whose durable log is far larger than the compact
    /// bootstrap budget rehydrates successfully through the compact path, and
    /// the full event log never appears in the activity result.
    #[tokio::test(flavor = "current_thread")]
    async fn bootstrap_returns_compact_state_for_large_log() {
        use test_support::{DriveCommand, RunnerStores, SessionRunner};

        let store = Arc::new(InMemorySessionStore::new());
        let blobs: Arc<dyn engine::storage::BlobStore> = Arc::new(InMemoryBlobStore::new());
        let session_id = SessionId::new("bridge_large_session");
        store
            .create_session(CreateSession {
                session_id: session_id.clone(),
                agent_handle: AgentHandle::new("lightspeed.agent"),
                created_at_ms: 1,
            })
            .await
            .expect("create session");

        let runner = SessionRunner::new(
            RunnerStores::new(store.clone(), blobs.clone()),
            Arc::new(UnreachableLlm),
        );
        runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms: 10,
                command: engine::CoreAgentCommand::OpenSession {
                    config: volume_session_config(),
                },
                max_steps: None,
            })
            .await
            .expect("open session");

        // Repeatedly upsert the SAME keyed entry with sizeable previews. Each
        // upsert appends a context-applied event to the durable log but replaces
        // the prior active entry for that key, so the log grows without bound
        // while active context stays at a single entry: a long-lived session
        // whose durable log dwarfs its reduced state.
        let big_preview = "x".repeat(2_048);
        let upsert_count = 600u64;
        for index in 0..upsert_count {
            runner
                .drive_command(DriveCommand {
                    session_id: session_id.clone(),
                    observed_at_ms: 100 + index,
                    command: engine::CoreAgentCommand::UpsertContext {
                        key: engine::ContextEntryKey::new("note.live"),
                        entry: engine::ContextEntryInput {
                            kind: engine::ContextEntryKind::ProviderOpaque,
                            content_ref: engine::BlobRef::from_bytes(
                                format!("note-content-{index}").as_bytes(),
                            ),
                            media_type: Some("application/json".to_owned()),
                            preview: Some(big_preview.clone()),
                            provider_kind: None,
                            provider_item_id: None,
                            token_estimate: None,
                        },
                    },
                    max_steps: None,
                })
                .await
                .expect("upsert context");
        }

        let raw_log = read_all_session_events(store.as_ref(), &session_id)
            .await
            .expect("read raw log");
        let raw_log_bytes = serde_json::to_vec(&raw_log)
            .expect("serialize raw log")
            .len();

        let deps = storage_deps(store.clone());
        let result = create_or_load_session(
            &deps,
            CreateOrLoadSessionRequest {
                session_id: session_id.clone(),
                observed_at_ms: 2,
            },
        )
        .await
        .expect("cold bootstrap succeeds via compact path");

        // The activity result must be far smaller than the raw event log it was
        // reduced from.
        let result_bytes = serde_json::to_vec(&result).expect("serialize result").len();

        // The compact result carries reduced state, not the raw log.
        let core_state = result.core_state.expect("reduced state present");
        assert!(result.replayed_event_count >= upsert_count);
        // Active context stays tiny (the single replaced keyed entry) even
        // though the log accumulated hundreds of applied events.
        assert!(
            core_state.context.entries.len() < 8,
            "active context should stay small, got {}",
            core_state.context.entries.len()
        );
        assert!(
            result_bytes * 4 < raw_log_bytes,
            "compact result ({result_bytes} bytes) should be far smaller than raw log \
             ({raw_log_bytes} bytes)"
        );
        // And it stays under the bootstrap budget.
        assert!((result_bytes as u64) < temporal_workflow::DEFAULT_BOOTSTRAP_PAYLOAD_BUDGET_BYTES,);
    }

    /// The size guard fires with a typed error when the reduced result would
    /// exceed the budget, instead of letting Temporal reject it opaquely.
    #[test]
    fn bootstrap_size_guard_rejects_oversized_result() {
        let session_id = SessionId::new("oversized");
        let result = CreateOrLoadSessionResult {
            record: SessionRecord {
                session_id: session_id.clone(),
                agent_handle: AgentHandle::new("lightspeed.agent"),
                head: None,
                created_at_ms: 1,
                updated_at_ms: 1,
            },
            core_state: Some(engine::CoreAgentState::new()),
            run_submissions: Default::default(),
            head: None,
            replayed_event_count: 0,
        };

        let serialized = serde_json::to_vec(&result).unwrap().len() as u64;
        assert!(serialized > 1, "fixture should be non-trivially sized");

        // Within budget: passes.
        guard_bootstrap_payload_size_with_budget(&session_id, &result, serialized + 1)
            .expect("within-budget result should pass");

        // Budget below serialized size: typed rejection.
        let err = guard_bootstrap_payload_size_with_budget(&session_id, &result, serialized - 1)
            .expect_err("oversized result should be rejected");
        match err {
            ActivityError::Application(failure) => {
                let typed = failure
                    .source_error()
                    .downcast_ref::<SessionBootstrapPayloadTooLarge>()
                    .expect("expected typed SessionBootstrapPayloadTooLarge");
                assert_eq!(typed.session_id, session_id);
                assert_eq!(typed.budget_bytes, serialized - 1);
            }
            other => panic!("expected application failure, got {other:?}"),
        }
    }

    fn storage_deps(store: Arc<InMemorySessionStore>) -> StorageActivityDeps {
        let sessions: Arc<dyn SessionStore> = store;
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        StorageActivityDeps { sessions, blobs }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn append_events_returns_existing_entries_after_retry() {
        let store = Arc::new(InMemorySessionStore::new());
        let deps = storage_deps(store.clone());
        let session_id = create_test_session(store.as_ref()).await;
        let request = AppendEventsRequest {
            session_id: session_id.clone(),
            expected_head: None,
            events: vec![
                test_event(10, [("turn", "1")], json!({"value": "first"})),
                test_event(11, [("turn", "1")], json!({"value": "second"})),
            ],
        };

        let first = append_events(&deps, request.clone())
            .await
            .expect("append first batch");
        let retried = append_events(&deps, request)
            .await
            .expect("confirm retried batch");

        assert_eq!(retried, first);
        let page = read_all(store.as_ref(), &session_id).await;
        assert_eq!(page.entries, first.entries);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn append_events_preserves_payload_conflict() {
        let store = Arc::new(InMemorySessionStore::new());
        let deps = storage_deps(store.clone());
        let session_id = create_test_session(store.as_ref()).await;
        let first = AppendEventsRequest {
            session_id: session_id.clone(),
            expected_head: None,
            events: vec![test_event(10, [("turn", "1")], json!({"value": "first"}))],
        };
        append_events(&deps, first)
            .await
            .expect("append first batch");

        let error = append_events(
            &deps,
            AppendEventsRequest {
                session_id,
                expected_head: None,
                events: vec![test_event(
                    10,
                    [("turn", "1")],
                    json!({"value": "different"}),
                )],
            },
        )
        .await
        .expect_err("different payload remains a conflict");

        assert_expected_head_mismatch(error);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn append_events_preserves_observed_time_and_join_conflict() {
        let store = Arc::new(InMemorySessionStore::new());
        let deps = storage_deps(store.clone());
        let session_id = create_test_session(store.as_ref()).await;
        let first = AppendEventsRequest {
            session_id: session_id.clone(),
            expected_head: None,
            events: vec![test_event(10, [("turn", "1")], json!({"value": "same"}))],
        };
        append_events(&deps, first)
            .await
            .expect("append first batch");

        let error = append_events(
            &deps,
            AppendEventsRequest {
                session_id,
                expected_head: None,
                events: vec![test_event(11, [("turn", "2")], json!({"value": "same"}))],
            },
        )
        .await
        .expect_err("different observed time and joins remain a conflict");

        assert_expected_head_mismatch(error);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn append_events_keeps_empty_append_as_noop() {
        let store = Arc::new(InMemorySessionStore::new());
        let deps = storage_deps(store.clone());
        let session_id = create_test_session(store.as_ref()).await;

        let result = append_events(
            &deps,
            AppendEventsRequest {
                session_id: session_id.clone(),
                expected_head: None,
                events: Vec::new(),
            },
        )
        .await
        .expect("empty append");

        assert!(result.entries.is_empty());
        assert_eq!(result.head, None);
        assert!(
            read_all(store.as_ref(), &session_id)
                .await
                .entries
                .is_empty()
        );
    }

    fn assert_expected_head_mismatch(error: ActivityError) {
        let ActivityError::Application(failure) = error else {
            panic!("expected application failure");
        };
        assert!(matches!(
            failure.source_error().downcast_ref::<SessionStoreError>(),
            Some(SessionStoreError::ExpectedHeadMismatch { .. })
        ));
    }
}
