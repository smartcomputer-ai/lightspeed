use engine::{
    AgentHandle, BlobRef, SessionId, SessionPosition,
    storage::{
        AppendSessionEvents, AppendSessionEventsResult, CreateSession, DynamicSessionEntry,
        DynamicUncommittedSessionEvent, ReadSessionEvents, SessionStore, SessionStoreError,
    },
};
use temporalio_sdk::activities::ActivityError;

use crate::{
    AppendEventsRequest, CreateOrLoadSessionRequest, CreateOrLoadSessionResult, PutBlobRequest,
    ReadBlobRequest, ReadBlobResult,
};

use super::{common::activity_error, state::StorageActivityDeps};

pub(super) async fn create_or_load_session(
    deps: &StorageActivityDeps,
    request: CreateOrLoadSessionRequest,
) -> Result<CreateOrLoadSessionResult, ActivityError> {
    let record = match deps
        .sessions
        .create_session(CreateSession {
            session_id: request.session_id.clone(),
            agent_handle: AgentHandle::new("forge.agent"),
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
    let entries = read_all_session_events(deps.sessions.as_ref(), &request.session_id).await?;
    Ok(CreateOrLoadSessionResult { record, entries })
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
        storage::{BlobStore, InMemoryBlobStore, InMemorySessionStore, SessionPage},
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
            event: DynamicEvent::new("forge.test.event", 1, payload),
        }
    }

    async fn create_test_session(store: &InMemorySessionStore) -> SessionId {
        let session_id = SessionId::new("session-a");
        store
            .create_session(CreateSession {
                session_id: session_id.clone(),
                agent_handle: AgentHandle::new("forge.test"),
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
