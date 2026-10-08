//! Bounded history windows. This path never loads or replays reducer state.

use api::{AgentApiError, EventCursor, SessionEventsReadParams, SessionEventsReadResponse};
use api_projection::{
    CoreAgentProjector, decode_stored_entry, event_page_limit, map_session_store_error,
};
use harness::{
    CoreAgentCodec, EventSeq, SessionId,
    storage::{BlobStore, ReadSessionEventRange, SessionStore},
};

pub(super) async fn read(
    sessions: &dyn SessionStore,
    blobs: &dyn BlobStore,
    params: SessionEventsReadParams,
) -> Result<SessionEventsReadResponse, AgentApiError> {
    if params.after.is_some() || params.wait_ms.unwrap_or(0) > 0 {
        return Err(AgentApiError::invalid_request(
            "backward reads cannot use after or waitMs",
        ));
    }
    let session_id = SessionId::try_new(params.session_id)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid session id: {error}")))?;
    let limit = event_page_limit(params.limit)?;
    if params.before.is_some_and(|cursor| cursor.seq == 0) {
        return Err(AgentApiError::invalid_request("before must be positive"));
    }
    let record = sessions
        .load_session(&session_id)
        .await
        .map_err(map_session_store_error)?
        .ok_or_else(|| AgentApiError::not_found(format!("session not found: {session_id}")))?;
    // The log's sequence is contiguous, including inherited fork prefixes.
    // Fence before reading: concurrent appends belong to the forward tail.
    let head = record.head.as_ref().map_or(0, |head| head.seq.as_u64());
    let through = params
        .before
        .map_or(head, |cursor| head.min(cursor.seq - 1));
    let after = through.saturating_sub(limit as u64);
    let page = sessions
        .read_range(ReadSessionEventRange {
            session_id: session_id.clone(),
            after: EventSeq::new(after),
            through: EventSeq::new(through),
            limit,
        })
        .await
        .map_err(map_session_store_error)?;
    // Never turn an unavailable interval into a successful, silently gapped page.
    if page.entries.len() as u64 != through - after
        || page
            .entries
            .iter()
            .enumerate()
            .any(|(index, entry)| entry.position.seq.as_u64() != after + index as u64 + 1)
    {
        return Err(AgentApiError::internal(
            "transcript event range is incomplete",
        ));
    }
    let projector = CoreAgentProjector::new(blobs);
    let mut events = Vec::with_capacity(page.entries.len());
    for stored in &page.entries {
        let entry = decode_stored_entry(&CoreAgentCodec, stored)?;
        events.push(projector.project_entry(&session_id, &entry).await?);
    }
    Ok(SessionEventsReadResponse {
        events,
        next_cursor: (after > 0).then_some(EventCursor { seq: after + 1 }),
        complete: after == 0,
        gap: None,
        head_cursor: Some(EventCursor { seq: head }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness::{
        CoreAgentEvent, CoreAgentJoins, CoreAgentLifecycleEvent, UncommittedCoreAgentEvent,
        storage::{AppendSessionEvents, CreateSession, InMemoryBlobStore, InMemorySessionStore},
    };

    async fn append(store: &InMemorySessionStore, id: &SessionId, count: usize) {
        let record = store.load_session(id).await.unwrap().unwrap();
        let event = CoreAgentCodec
            .encode_uncommitted(&UncommittedCoreAgentEvent {
                observed_at_ms: 1,
                joins: CoreAgentJoins::default(),
                event: CoreAgentEvent::Lifecycle(CoreAgentLifecycleEvent::Closed),
            })
            .unwrap();
        store
            .append(AppendSessionEvents {
                session_id: id.clone(),
                expected_head: record.head,
                events: vec![event; count],
            })
            .await
            .unwrap();
    }

    async fn setup(count: usize) -> (InMemorySessionStore, InMemoryBlobStore, SessionId) {
        let store = InMemorySessionStore::new();
        let id = SessionId::new("transcript-window");
        store
            .create_session(CreateSession {
                session_id: id.clone(),
                metadata: Default::default(),
                display_name: None,
                origin: None,
                delete_after_close_ms: None,
                created_at_ms: 1,
            })
            .await
            .unwrap();
        if count > 0 {
            append(&store, &id, count).await;
        }
        (store, InMemoryBlobStore::new(), id)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn code_tool_progress_preserves_contiguous_pagination_without_becoming_model_calls() {
        let (store, blobs, id) = setup(1).await;
        let record = store.load_session(&id).await.unwrap().unwrap();
        let event = CoreAgentCodec
            .encode_uncommitted(&UncommittedCoreAgentEvent {
                observed_at_ms: 2,
                joins: CoreAgentJoins::default(),
                event: CoreAgentEvent::CodeTool(harness::CodeToolEvent::ScopeClosed {
                    execution_id: "execution-hidden".to_owned(),
                    cancel: false,
                }),
            })
            .unwrap();
        store
            .append(AppendSessionEvents {
                session_id: id.clone(),
                expected_head: record.head,
                events: vec![event],
            })
            .await
            .unwrap();
        let params = SessionEventsReadParams {
            direction: api::SessionEventDirection::Backward,
            after: None,
            wait_ms: None,
            session_id: id.to_string(),
            before: None,
            limit: Some(1),
        };
        let recent = read(&store, &blobs, params.clone()).await.unwrap();
        assert_eq!(recent.events.len(), 1);
        assert_eq!(recent.events[0].cursor.seq, 2);
        assert_eq!(
            recent.events[0].kind,
            api::SessionEventKindView::CodeToolProgress {
                execution_id: "execution-hidden".to_owned(),
                request_id: None,
                phase: api::CodeToolProgressPhase::ScopeClosed,
                status: None,
            }
        );
        assert!(!recent.complete);
        assert_eq!(recent.head_cursor, Some(EventCursor { seq: 2 }));
        assert_eq!(recent.next_cursor, Some(EventCursor { seq: 2 }));
        let prior = read(
            &store,
            &blobs,
            SessionEventsReadParams {
                before: recent.next_cursor,
                ..params
            },
        )
        .await
        .unwrap();
        assert!(prior.complete);
        assert_eq!(prior.events.len(), 1);
        assert_eq!(prior.events[0].cursor.seq, 1);
        assert_eq!(
            prior.events[0].kind,
            api::SessionEventKindView::SessionClosed
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn recent_window_and_backward_pages_cover_large_log_during_appends() {
        // These independently projectable events intentionally do not form a
        // replayable reducer history: transcript reads must never replay it.
        let (store, blobs, id) = setup(12_005).await;
        let mut before = None;
        let mut sequences = Vec::new();
        loop {
            let page = read(
                &store,
                &blobs,
                SessionEventsReadParams {
                    direction: api::SessionEventDirection::Backward,
                    after: None,
                    wait_ms: None,
                    session_id: id.to_string(),
                    before,
                    limit: Some(500),
                },
            )
            .await
            .unwrap();
            assert!(page.events.len() <= 500);
            if before.is_none() {
                assert_eq!(page.head_cursor.unwrap().seq, 12_005);
                assert_eq!(page.events.first().unwrap().cursor.seq, 11_506);
                append(&store, &id, 3).await;
            }
            sequences.extend(page.events.iter().map(|event| event.cursor.seq));
            if page.complete {
                assert!(page.next_cursor.is_none());
                break;
            }
            assert!(
                page.next_cursor.unwrap().seq < before.map_or(u64::MAX, |c: EventCursor| c.seq)
            );
            before = page.next_cursor;
        }
        sequences.sort_unstable();
        assert_eq!(sequences, (1..=12_005).collect::<Vec<_>>());
        let tail = store
            .read_after(harness::storage::ReadSessionEvents {
                session_id: id,
                after: Some(EventSeq::new(12_005)),
                limit: 500,
            })
            .await
            .unwrap();
        assert_eq!(tail.entries.len(), 3);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn empty_history_and_invalid_bounds_are_explicit() {
        let (store, blobs, id) = setup(0).await;
        let page = read(
            &store,
            &blobs,
            SessionEventsReadParams {
                direction: api::SessionEventDirection::Backward,
                after: None,
                wait_ms: None,
                session_id: id.to_string(),
                before: None,
                limit: Some(10),
            },
        )
        .await
        .unwrap();
        assert_eq!(page.head_cursor.unwrap().seq, 0);
        assert!(page.complete);
        assert!(page.events.is_empty());
        for (before, limit) in [(Some(EventCursor { seq: 0 }), Some(10)), (None, Some(0))] {
            let error = read(
                &store,
                &blobs,
                SessionEventsReadParams {
                    direction: api::SessionEventDirection::Backward,
                    after: None,
                    wait_ms: None,
                    session_id: id.to_string(),
                    before,
                    limit,
                },
            )
            .await
            .unwrap_err();
            assert_eq!(error.kind, api::AgentApiErrorKind::InvalidRequest);
        }
        for (after, wait_ms) in [(Some(EventCursor { seq: 0 }), None), (None, Some(1))] {
            let error = read(
                &store,
                &blobs,
                SessionEventsReadParams {
                    direction: api::SessionEventDirection::Backward,
                    session_id: id.to_string(),
                    before: None,
                    after,
                    wait_ms,
                    limit: None,
                },
            )
            .await
            .unwrap_err();
            assert_eq!(error.kind, api::AgentApiErrorKind::InvalidRequest);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn backward_window_crosses_a_forks_inherited_prefix() {
        let (store, blobs, parent) = setup(10).await;
        let child = SessionId::new("history-fork");
        store
            .create_forked_session(harness::storage::CreateForkedSession {
                source_session_id: parent,
                session_id: child.clone(),
                source_seq: EventSeq::new(7),
                created_at_ms: 2,
            })
            .await
            .unwrap();
        append(&store, &child, 3).await;
        let page = read(
            &store,
            &blobs,
            SessionEventsReadParams {
                direction: api::SessionEventDirection::Backward,
                session_id: child.to_string(),
                before: None,
                after: None,
                wait_ms: None,
                limit: Some(5),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            page.events
                .iter()
                .map(|event| event.cursor.seq)
                .collect::<Vec<_>>(),
            vec![6, 7, 8, 9, 10]
        );
        assert_eq!(page.next_cursor.unwrap().seq, 6);
        assert!(
            page.events
                .iter()
                .all(|event| event.session_id == child.as_str())
        );
    }
}
