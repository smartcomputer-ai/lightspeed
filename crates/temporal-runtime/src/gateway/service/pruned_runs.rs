//! Runs reduced state has pruned, read back from the session's log. State keeps a
//! recent window of terminal run records, and these serve the pages and lookups
//! older than that window. Submission dedupe and workflow status stay on the window.
use std::collections::BTreeMap;

use api::AgentApiError;
use api_projection::{decode_stored_entry, map_session_store_error};
use harness::storage::{ReadRunRecordEntries, SessionStore};
use harness::{CoreAgentCodec, CoreAgentEntry, RunId, RunRecord, RunsState, SessionId};

/// What the log says about the `limit` highest run ids below `before`. A run that is
/// not terminal yet has no record here.
pub(super) async fn read_pruned_runs(
    store: &dyn SessionStore,
    session_id: &SessionId,
    before: RunId,
    limit: usize,
) -> Result<Vec<RunRecord>, AgentApiError> {
    let stored = store
        .read_run_record_entries(ReadRunRecordEntries {
            session_id: session_id.clone(),
            before,
            limit,
        })
        .await
        .map_err(map_session_store_error)?;
    let mut by_run = BTreeMap::<RunId, Vec<CoreAgentEntry>>::new();
    for entry in &stored {
        let entry = decode_stored_entry(&CoreAgentCodec, entry)?;
        if let Some(run_id) = entry.joins.run_id {
            by_run.entry(run_id).or_default().push(entry);
        }
    }
    Ok(by_run
        .values()
        .filter_map(|entries| harness::run_record_from_entries(entries))
        .collect())
}

/// The pruned runs a page of `limit` needs below `cursor`, once state has run out of
/// runs newer than the pruned ones. It reads one more run than the page can hold, and
/// that is how the page knows older runs remain.
pub(super) async fn pruned_runs_for_page(
    store: &dyn SessionStore,
    session_id: &SessionId,
    runs: &RunsState,
    cursor: Option<RunId>,
    limit: usize,
) -> Result<Vec<RunRecord>, AgentApiError> {
    let Some(pruned_through) = runs.pruned_through else {
        return Ok(Vec::new());
    };
    let newer = runs
        .finished
        .iter()
        .map(|run| run.run_id)
        .chain(runs.active.iter().map(|run| run.run_id))
        .chain(runs.queued.iter().map(|run| run.run_id))
        .filter(|run_id| *run_id > pruned_through && cursor.is_none_or(|cursor| *run_id < cursor))
        .count();
    if newer > limit {
        return Ok(Vec::new());
    }
    let below_pruned = RunId::new(pruned_through.as_u64() + 1);
    read_pruned_runs(
        store,
        session_id,
        cursor.map_or(below_pruned, |cursor| cursor.min(below_pruned)),
        limit + 1 - newer,
    )
    .await
}

#[cfg(test)]
mod tests {
    use api::RunStatus as ApiRunStatus;
    use api_projection::CoreAgentProjector;
    use harness::storage::{
        AppendSessionEvents, BlobStore, CreateSession, InMemoryBlobStore, InMemorySessionStore,
    };
    use harness::{
        BlobRef, CompactionPolicy, ContentRef, ContextConfig, ContextEntryInput, ContextEntryKind,
        ContextMessageRole, CoreAgentAction, CoreAgentCommand, CoreAgentDrive, CoreAgentState,
        LlmFinish, LlmGenerationFacts, LlmGenerationResult, LlmGenerationStatus, ModelSelection,
        ProviderApiKind, RunConfig, RunRequestCommand, RunRequestSource, RunStatus, SessionConfig,
        SubmissionId,
    };

    use super::*;

    struct Session {
        store: InMemorySessionStore,
        blobs: InMemoryBlobStore,
        drive: CoreAgentDrive,
        now: u64,
    }

    impl Session {
        async fn open() -> Self {
            let session_id = SessionId::new("3f2c1e9a-7b1d-4f55-9a8e-2c6c1d9f4a10");
            let store = InMemorySessionStore::new();
            store
                .create_session(CreateSession {
                    metadata: Default::default(),
                    session_id: session_id.clone(),
                    display_name: None,
                    origin: None,
                    delete_after_close_ms: None,
                    created_at_ms: 1,
                })
                .await
                .expect("create session");
            let mut session = Self {
                store,
                blobs: InMemoryBlobStore::new(),
                drive: CoreAgentDrive::from_replayed(session_id, CoreAgentState::new()),
                now: 1_000,
            };
            session
                .admit(CoreAgentCommand::OpenSession {
                    config: SessionConfig {
                        model: ModelSelection {
                            api_kind: ProviderApiKind::OpenAiResponses,
                            provider_id: "openai".to_owned(),
                            model: "gpt-test".to_owned(),
                        },
                        generation: Default::default(),
                        limits: Default::default(),
                        context: ContextConfig {
                            input_limit_tokens: None,
                            compaction: Some(CompactionPolicy::Disabled),
                        },
                        features: Default::default(),
                    },
                })
                .await;
            session
        }

        async fn commit(&mut self, action: CoreAgentAction) {
            let CoreAgentAction::AppendEvents {
                expected_head,
                events,
            } = action
            else {
                panic!("expected an append, got {action:?}");
            };
            let appended = self
                .store
                .append(AppendSessionEvents {
                    session_id: self.drive.session_id().clone(),
                    expected_head,
                    events,
                })
                .await
                .expect("append");
            self.drive
                .resume_appended(appended.entries)
                .expect("resume");
        }

        async fn admit(&mut self, command: CoreAgentCommand) {
            self.now += 1;
            let action = self.drive.admit_command(command, self.now).expect("admit");
            self.commit(action).await;
        }

        async fn chat_run(&mut self, index: u64) {
            let content_ref = self
                .blobs
                .put_bytes(format!("message {index}").into_bytes())
                .await
                .expect("put");
            self.admit(CoreAgentCommand::RequestRun(RunRequestCommand {
                requested_by: None,
                notify_on_terminal: Vec::new(),
                submission_id: Some(SubmissionId::new(format!("submission-{index}"))),
                source: RunRequestSource::Input {
                    input: vec![ContextEntryInput {
                        kind: ContextEntryKind::Message {
                            role: ContextMessageRole::User,
                        },
                        content: ContentRef {
                            content_ref,
                            media_type: Some("text/plain".to_owned()),
                            provider_kind: None,
                        },
                        preview: None,
                        origin: None,
                        provenance_ref: None,
                        token_estimate: None,
                    }],
                },
                run_config: RunConfig::default(),
                input_limit_tokens: None,
            }))
            .await;
            loop {
                self.now += 1;
                match self.drive.next_action_unbounded(self.now).expect("next") {
                    action @ CoreAgentAction::AppendEvents { .. } => self.commit(action).await,
                    CoreAgentAction::GenerateLlm { request } => {
                        let action = self
                            .drive
                            .resume_generation(
                                LlmGenerationResult {
                                    run_id: request.run_id,
                                    turn_id: request.turn_id,
                                    status: LlmGenerationStatus::Succeeded,
                                    failure_ref: None,
                                    context_entries: Vec::new(),
                                    facts: LlmGenerationFacts {
                                        duration_ms: None,
                                        provider_response_id: None,
                                        finish: LlmFinish::Stop,
                                        usage: None,
                                        tool_calls: Vec::new(),
                                        approval_requests: Vec::new(),
                                        context_token_estimate: None,
                                    },
                                },
                                self.now,
                            )
                            .expect("generate");
                        self.commit(action).await;
                    }
                    CoreAgentAction::Idle => return,
                    other => panic!("unexpected {other:?}"),
                }
            }
        }

        /// Every run id `session/runs/list` hands out, page by page, as the gateway
        /// assembles them.
        async fn listed_runs(&self, limit: usize) -> Vec<api::RunSummaryView> {
            let state = self.drive.state();
            let projector = CoreAgentProjector::new(&self.blobs);
            let mut listed = Vec::new();
            let mut cursor = None;
            loop {
                let pruned = pruned_runs_for_page(
                    &self.store,
                    self.drive.session_id(),
                    &state.runs,
                    cursor,
                    limit,
                )
                .await
                .expect("pruned runs");
                let (page, next_cursor, has_older_runs) = projector
                    .project_run_summaries(state, &pruned, cursor, limit)
                    .await
                    .expect("page");
                assert!(page.len() <= limit);
                assert_eq!(next_cursor.is_some(), has_older_runs);
                if has_older_runs {
                    assert_eq!(page.len(), limit, "a short page means nothing older");
                }
                listed.extend(page);
                let Some(next_cursor) = next_cursor else {
                    return listed;
                };
                cursor = Some(api_projection::parse_api_run_id(&next_cursor).expect("cursor"));
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_pages_are_whole_across_the_boundary_of_what_state_retains() {
        let mut session = Session::open().await;
        let runs = harness::MAX_RETAINED_TERMINAL_RUNS as u64 + 30;
        for index in 1..=runs {
            session.chat_run(index).await;
        }
        assert_eq!(
            session.drive.state().runs.pruned_through,
            Some(RunId::new(30))
        );

        for limit in [1, 7, 100] {
            let listed = session.listed_runs(limit).await;
            let ids = listed.iter().map(|run| run.id.clone()).collect::<Vec<_>>();
            let expected = (1..=runs)
                .rev()
                .map(|id| api_projection::api_run_id(RunId::new(id)))
                .collect::<Vec<_>>();
            assert_eq!(ids, expected, "limit {limit}");
            assert!(
                listed
                    .iter()
                    .all(|run| run.status == ApiRunStatus::Completed)
            );
        }

        // A summary from the log says what one from state does, apart from usage.
        let listed = session.listed_runs(100).await;
        let from_log = &listed[listed.len() - 1];
        assert!(from_log.finished_at_ms.is_some() && from_log.started_at_ms.is_some());
        let api::RunSummarySourceView::Input { preview, .. } = &from_log.source;
        assert_eq!(preview.as_deref(), Some("message 1"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_run_held_in_state_is_listed_once_and_in_order_among_pruned_ones() {
        let mut session = Session::open().await;
        // Run 1 stays active while every later run is cancelled in its queue, so the
        // oldest run is still in state after newer ones have been pruned.
        let content_ref = BlobRef::from_bytes(b"holding");
        session
            .blobs
            .put_bytes(b"holding".to_vec())
            .await
            .expect("put");
        let request = |index: u64| {
            CoreAgentCommand::RequestRun(RunRequestCommand {
                requested_by: None,
                notify_on_terminal: Vec::new(),
                submission_id: Some(SubmissionId::new(format!("submission-{index}"))),
                source: RunRequestSource::Input {
                    input: vec![ContextEntryInput {
                        kind: ContextEntryKind::Message {
                            role: ContextMessageRole::User,
                        },
                        content: ContentRef {
                            content_ref: content_ref.clone(),
                            media_type: Some("text/plain".to_owned()),
                            provider_kind: None,
                        },
                        preview: None,
                        origin: None,
                        provenance_ref: None,
                        token_estimate: None,
                    }],
                },
                run_config: RunConfig::default(),
                input_limit_tokens: None,
            })
        };
        session.admit(request(1)).await;
        session.now += 1;
        loop {
            session.now += 1;
            match session
                .drive
                .next_action_unbounded(session.now)
                .expect("next")
            {
                action @ CoreAgentAction::AppendEvents { .. } => session.commit(action).await,
                CoreAgentAction::GenerateLlm { .. } => break,
                other => panic!("unexpected {other:?}"),
            }
        }
        let runs = harness::MAX_RETAINED_TERMINAL_RUNS as u64 + 5;
        for index in 2..=runs {
            session.admit(request(index)).await;
            session
                .admit(CoreAgentCommand::CancelRun {
                    run_id: RunId::new(index),
                    requested_by: None,
                })
                .await;
        }
        let state = session.drive.state();
        assert!(!state.runs.is_pruned(RunId::new(1)));
        assert!(state.runs.is_pruned(RunId::new(2)));

        let listed = session.listed_runs(50).await;
        assert_eq!(listed.len(), runs as usize);
        let oldest = listed.last().expect("run 1");
        assert_eq!(oldest.id, api_projection::api_run_id(RunId::new(1)));
        assert_eq!(oldest.status, ApiRunStatus::Running);
        assert!(
            listed[..listed.len() - 1]
                .iter()
                .all(|run| run.status == ApiRunStatus::Cancelled)
        );
    }

    /// A record rebuilt from the log has to match the one state kept.
    #[tokio::test(flavor = "current_thread")]
    async fn a_pruned_record_is_the_record_state_kept() {
        let mut session = Session::open().await;
        session.chat_run(1).await;
        let kept = session.drive.state().runs.finished[0].clone();
        let rebuilt =
            read_pruned_runs(&session.store, session.drive.session_id(), RunId::new(2), 1)
                .await
                .expect("read");
        assert_eq!(rebuilt, vec![kept.clone()]);
        assert_eq!(kept.status, RunStatus::Completed);
    }
}
