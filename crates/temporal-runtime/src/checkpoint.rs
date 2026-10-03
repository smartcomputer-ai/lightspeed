//! CAS-backed reducer checkpoint loading shared by gateway reads and storage
//! activities. Checkpoints are disposable accelerators; every rejected or
//! unreadable pointer falls back to the authoritative event log.

use harness::{
    EventSeq, SessionId,
    storage::{
        AdvanceSessionCheckpoint, BlobStore, ReadSessionEventRange, SessionCheckpoint,
        SessionRecord, SessionStore,
    },
};
use serde::{Deserialize, Serialize};
use std::time::Instant;
use temporal_workflow::{
    DEFAULT_BOOTSTRAP_PAYLOAD_BUDGET_BYTES, ReducedSession, reduce_session_entries,
    reduce_session_entries_from,
};

pub(crate) const SESSION_CHECKPOINT_FORMAT_VERSION: u32 = 1;
pub(crate) const CHECKPOINT_TAIL_EVENT_THRESHOLD: u64 = 512;
pub(crate) const CHECKPOINT_TAIL_BYTE_THRESHOLD: u64 = 2 * 1024 * 1024;
pub(crate) const CHECKPOINT_APPEND_BATCH_BYTE_THRESHOLD: u64 = 2 * 1024 * 1024;
pub(crate) const CHECKPOINT_TERMINAL_RUN_THRESHOLD: usize = 10;
pub(crate) const CHECKPOINT_LARGE_RUN_EVENT_THRESHOLD: u64 = 100;
const CHECKPOINT_READ_PAGE: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CheckpointReduction {
    core_state: harness::CoreAgentState,
    run_submissions: std::collections::BTreeMap<u64, Option<harness::SubmissionId>>,
}

impl From<CheckpointReduction> for ReducedSession {
    fn from(value: CheckpointReduction) -> Self {
        Self {
            core_state: value.core_state,
            run_submissions: value.run_submissions,
            replayed_event_count: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LoadedReduction {
    pub reduced: ReducedSession,
    pub fresh_session: bool,
    pub full_replay: bool,
    pub tail_event_count: u64,
    pub tail_encoded_bytes: u64,
}

pub(crate) async fn load_reduction(
    sessions: &dyn SessionStore,
    blobs: &dyn BlobStore,
    record: &SessionRecord,
) -> Result<LoadedReduction, anyhow::Error> {
    let started = Instant::now();
    let head_seq = record.head.as_ref().map(|head| head.seq);
    let fresh_session = head_seq.is_none();
    if fresh_session {
        return Ok(LoadedReduction {
            reduced: ReducedSession::default(),
            fresh_session: true,
            full_replay: false,
            tail_event_count: 0,
            tail_encoded_bytes: 0,
        });
    }
    let head_seq = head_seq.expect("checked non-empty head");

    let checkpoint = match sessions.load_checkpoint(&record.session_id).await {
        Ok(checkpoint) => checkpoint,
        Err(error) => {
            tracing::warn!(
                session_id = %record.session_id,
                reason = "pointer_read",
                error = %error,
                "session checkpoint rejected; replaying authoritative log"
            );
            None
        }
    };

    let checkpoint_was_present = checkpoint.is_some();
    if let Some(checkpoint) = checkpoint {
        match load_checkpoint_tail(sessions, blobs, record, head_seq, &checkpoint).await {
            Ok((reduced, tail_event_count, tail_encoded_bytes)) => {
                tracing::debug!(
                    session_id = %record.session_id,
                    through_seq = checkpoint.through_seq.as_u64(),
                    lag = head_seq.as_u64().saturating_sub(checkpoint.through_seq.as_u64()),
                    tail_event_count,
                    tail_encoded_bytes,
                    duration_ms = started.elapsed().as_millis() as u64,
                    "loaded session reducer checkpoint"
                );
                return Ok(LoadedReduction {
                    reduced,
                    fresh_session: false,
                    full_replay: false,
                    tail_event_count,
                    tail_encoded_bytes,
                });
            }
            Err(error) => tracing::warn!(
                session_id = %record.session_id,
                reason = checkpoint_rejection_reason(&error),
                error = %error,
                "session checkpoint rejected; replaying authoritative log"
            ),
        }
    }

    let entries =
        read_entries_through(sessions, &record.session_id, EventSeq::new(0), head_seq).await?;
    let encoded_bytes = encoded_entry_bytes(&entries);
    let reduced = reduce_session_entries(&entries)?;
    validate_reduced_head(&reduced, head_seq)?;
    if entries.len() as u64 >= CHECKPOINT_TAIL_EVENT_THRESHOLD
        || encoded_bytes >= CHECKPOINT_TAIL_BYTE_THRESHOLD
    {
        tracing::warn!(
            session_id = %record.session_id,
            reason = if checkpoint_was_present { "fallback" } else { "miss" },
            event_count = entries.len() as u64,
            encoded_bytes,
            "large authoritative session replay"
        );
    }
    tracing::debug!(
        session_id = %record.session_id,
        event_count = entries.len() as u64,
        encoded_bytes,
        duration_ms = started.elapsed().as_millis() as u64,
        "replayed full authoritative session log"
    );
    Ok(LoadedReduction {
        reduced,
        fresh_session: false,
        full_replay: true,
        tail_event_count: entries.len() as u64,
        tail_encoded_bytes: encoded_bytes,
    })
}

async fn load_checkpoint_tail(
    sessions: &dyn SessionStore,
    blobs: &dyn BlobStore,
    record: &SessionRecord,
    head_seq: EventSeq,
    checkpoint: &SessionCheckpoint,
) -> Result<(ReducedSession, u64, u64), anyhow::Error> {
    anyhow::ensure!(
        checkpoint.format_version == SESSION_CHECKPOINT_FORMAT_VERSION,
        "unsupported checkpoint format {}",
        checkpoint.format_version
    );
    anyhow::ensure!(
        checkpoint.lineage_source_session_id == record.source_session_id
            && checkpoint.lineage_source_seq == record.source_seq,
        "checkpoint lineage mismatch"
    );
    anyhow::ensure!(
        checkpoint.through_seq <= head_seq,
        "checkpoint sequence {} exceeds fenced head {}",
        checkpoint.through_seq,
        head_seq
    );
    let bytes = blobs.read_bytes(&checkpoint.state_ref).await?;
    anyhow::ensure!(
        bytes.len() as u64 == checkpoint.byte_len,
        "checkpoint byte length mismatch"
    );
    let checkpoint_state: CheckpointReduction = serde_json::from_slice(&bytes)?;
    let reduced_at = checkpoint_state
        .core_state
        .reduced_to
        .as_ref()
        .map(|position| position.seq);
    anyhow::ensure!(
        reduced_at == Some(checkpoint.through_seq),
        "checkpoint reduced position does not match pointer"
    );
    let entries = read_entries_through(
        sessions,
        &record.session_id,
        checkpoint.through_seq,
        head_seq,
    )
    .await?;
    let tail_encoded_bytes = encoded_entry_bytes(&entries);
    let reduced = reduce_session_entries_from(checkpoint_state.into(), &entries)?;
    validate_reduced_head(&reduced, head_seq)?;
    Ok((reduced, entries.len() as u64, tail_encoded_bytes))
}

async fn read_entries_through(
    sessions: &dyn SessionStore,
    session_id: &SessionId,
    mut after: EventSeq,
    through: EventSeq,
) -> Result<Vec<harness::storage::StoredSessionEntry>, anyhow::Error> {
    let mut entries = Vec::new();
    while after < through {
        let page = sessions
            .read_range(ReadSessionEventRange {
                session_id: session_id.clone(),
                after,
                through,
                limit: CHECKPOINT_READ_PAGE,
            })
            .await?;
        if page.entries.is_empty() {
            anyhow::bail!("effective session log has a gap after seq {after}");
        }
        after = page
            .entries
            .last()
            .expect("checked non-empty page")
            .position
            .seq;
        entries.extend(page.entries);
        if page.complete {
            break;
        }
    }
    anyhow::ensure!(
        after == through,
        "effective session log ended at {after}, expected {through}"
    );
    Ok(entries)
}

fn validate_reduced_head(reduced: &ReducedSession, head: EventSeq) -> anyhow::Result<()> {
    let reduced_to = reduced
        .core_state
        .reduced_to
        .as_ref()
        .map(|position| position.seq);
    anyhow::ensure!(
        reduced_to == Some(head),
        "reduced state does not reach fenced head"
    );
    Ok(())
}

pub(crate) fn checkpoint_due(loaded: &LoadedReduction) -> bool {
    loaded.full_replay
        || loaded.tail_event_count >= CHECKPOINT_TAIL_EVENT_THRESHOLD
        || loaded.tail_encoded_bytes >= CHECKPOINT_TAIL_BYTE_THRESHOLD
}

/// Decide whether newly terminal runs justify advancing the checkpoint before
/// the general tail thresholds fire. Sequence-span length matches the range a
/// run-detail read must fetch, including the accepted and terminal entries.
pub(crate) fn terminal_runs_checkpoint_due(
    state: &harness::CoreAgentState,
    checkpoint_through_seq: Option<EventSeq>,
    newly_terminal_seqs: &[EventSeq],
) -> bool {
    let checkpoint_seq = checkpoint_through_seq.map_or(0, EventSeq::as_u64);
    let terminal_runs_since_checkpoint = state
        .runs
        .completed
        .iter()
        .filter(|run| run.terminal_seq.as_u64() > checkpoint_seq)
        .count();
    if terminal_runs_since_checkpoint >= CHECKPOINT_TERMINAL_RUN_THRESHOLD {
        return true;
    }

    state.runs.completed.iter().any(|run| {
        newly_terminal_seqs.contains(&run.terminal_seq)
            && run
                .terminal_seq
                .as_u64()
                .saturating_sub(run.first_seq.as_u64())
                .saturating_add(1)
                > CHECKPOINT_LARGE_RUN_EVENT_THRESHOLD
    })
}

pub(crate) async fn write_checkpoint(
    sessions: &dyn SessionStore,
    blobs: &dyn BlobStore,
    record: &SessionRecord,
    reduced: &ReducedSession,
    created_at_ms: u64,
) -> Result<bool, anyhow::Error> {
    let Some(through_seq) = reduced
        .core_state
        .reduced_to
        .as_ref()
        .map(|position| position.seq)
    else {
        return Ok(false);
    };
    let bytes = serde_json::to_vec(&CheckpointReduction {
        core_state: reduced.core_state.clone(),
        run_submissions: reduced.run_submissions.clone(),
    })?;
    anyhow::ensure!(
        bytes.len() as u64 <= DEFAULT_BOOTSTRAP_PAYLOAD_BUDGET_BYTES,
        "checkpoint exceeds bootstrap payload budget"
    );
    let byte_len = bytes.len() as u64;
    let state_ref = blobs.put_bytes(bytes).await?;
    let started = Instant::now();
    let advanced = sessions
        .advance_checkpoint(AdvanceSessionCheckpoint {
            checkpoint: SessionCheckpoint {
                session_id: record.session_id.clone(),
                through_seq,
                format_version: SESSION_CHECKPOINT_FORMAT_VERSION,
                state_ref,
                lineage_source_session_id: record.source_session_id.clone(),
                lineage_source_seq: record.source_seq,
                byte_len,
                created_at_ms,
            },
        })
        .await?;
    tracing::debug!(
        session_id = %record.session_id,
        through_seq = through_seq.as_u64(),
        byte_len,
        advanced,
        duration_ms = started.elapsed().as_millis() as u64,
        "wrote session reducer checkpoint"
    );
    Ok(advanced)
}

fn encoded_entry_bytes(entries: &[harness::storage::StoredSessionEntry]) -> u64 {
    entries
        .iter()
        .map(|entry| serde_json::to_vec(entry).map_or(0, |bytes| bytes.len() as u64))
        .sum()
}

fn checkpoint_rejection_reason(error: &anyhow::Error) -> &'static str {
    let message = error.to_string();
    if message.contains("format") {
        "format"
    } else if message.contains("lineage") {
        "lineage"
    } else if message.contains("sequence") || message.contains("position") {
        "sequence"
    } else if message.contains("deserialize") || message.contains("expected") {
        "decode"
    } else {
        "corrupt"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness::{
        CoreAgentCodec, CoreAgentEvent, CoreAgentJoins, CoreAgentLifecycleEvent, ModelSelection,
        ProviderApiKind, UncommittedCoreAgentEvent,
        storage::{
            AppendSessionEvents, BlobStore, CreateSession, InMemoryBlobStore, InMemorySessionStore,
            SessionStore,
        },
    };

    fn stored_event(
        event: CoreAgentEvent,
        observed_at_ms: u64,
    ) -> harness::storage::UncommittedStoredEvent {
        CoreAgentCodec
            .encode_uncommitted(&UncommittedCoreAgentEvent {
                observed_at_ms,
                joins: CoreAgentJoins::default(),
                event,
            })
            .expect("encode test event")
    }

    async fn session_with_open_event() -> (
        InMemorySessionStore,
        InMemoryBlobStore,
        SessionId,
        SessionRecord,
    ) {
        let sessions = InMemorySessionStore::new();
        let blobs = InMemoryBlobStore::new();
        let session_id = SessionId::new("checkpoint-test");
        sessions
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
        let config = temporal_workflow::default_session_config(ModelSelection {
            api_kind: ProviderApiKind::OpenAiResponses,
            provider_id: "test".to_owned(),
            model: "test".to_owned(),
        });
        sessions
            .append(AppendSessionEvents {
                session_id: session_id.clone(),
                expected_head: None,
                events: vec![stored_event(
                    CoreAgentEvent::Lifecycle(CoreAgentLifecycleEvent::Opened { config }),
                    2,
                )],
            })
            .await
            .expect("append open");
        let record = sessions
            .load_session(&session_id)
            .await
            .expect("load record")
            .expect("session exists");
        (sessions, blobs, session_id, record)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn checkpoint_plus_tail_matches_full_replay() {
        let (sessions, blobs, session_id, record) = session_with_open_event().await;
        let initial = load_reduction(&sessions, &blobs, &record)
            .await
            .expect("initial replay");
        assert!(initial.full_replay);
        assert!(
            write_checkpoint(&sessions, &blobs, &record, &initial.reduced, 3)
                .await
                .expect("write checkpoint")
        );

        sessions
            .append(AppendSessionEvents {
                session_id: session_id.clone(),
                expected_head: record.head,
                events: vec![stored_event(
                    CoreAgentEvent::Lifecycle(CoreAgentLifecycleEvent::Closed),
                    4,
                )],
            })
            .await
            .expect("append tail");
        let current = sessions
            .load_session(&session_id)
            .await
            .expect("load current record")
            .expect("session exists");
        let checkpointed = load_reduction(&sessions, &blobs, &current)
            .await
            .expect("checkpoint replay");
        assert!(!checkpointed.full_replay);
        assert_eq!(checkpointed.tail_event_count, 1);

        let all = read_entries_through(
            &sessions,
            &session_id,
            EventSeq::new(0),
            current.head.expect("head").seq,
        )
        .await
        .expect("read full log");
        let full = reduce_session_entries(&all).expect("full replay");
        assert_eq!(checkpointed.reduced.core_state, full.core_state);
        assert_eq!(checkpointed.reduced.run_submissions, full.run_submissions);
    }

    fn terminal_run(id: u64, first_seq: u64, terminal_seq: u64) -> harness::RunRecord {
        harness::RunRecord {
            run_id: harness::RunId::new(id),
            status: harness::RunStatus::Completed,
            submission_id: None,
            submission_digest: None,
            source: harness::RunSource::Input { input: Vec::new() },
            first_seq: EventSeq::new(first_seq),
            terminal_seq: EventSeq::new(terminal_seq),
            accepted_at_ms: first_seq,
            started_at_ms: Some(first_seq),
            completed_at_ms: terminal_seq,
            usage: None,
            output: None,
            failure: None,
            notify_on_terminal: Vec::new(),
        }
    }

    #[test]
    fn checkpoint_tail_thresholds_are_doubled() {
        let loaded = |tail_event_count, tail_encoded_bytes| LoadedReduction {
            reduced: ReducedSession::default(),
            fresh_session: false,
            full_replay: false,
            tail_event_count,
            tail_encoded_bytes,
        };

        assert!(!checkpoint_due(&loaded(511, 2 * 1024 * 1024 - 1)));
        assert!(checkpoint_due(&loaded(512, 0)));
        assert!(checkpoint_due(&loaded(0, CHECKPOINT_TAIL_BYTE_THRESHOLD)));
        assert_eq!(CHECKPOINT_APPEND_BATCH_BYTE_THRESHOLD, 2 * 1024 * 1024);
    }

    #[test]
    fn terminal_runs_checkpoint_every_ten_since_the_pointer() {
        let mut state = harness::CoreAgentState::default();
        for id in 1..=9 {
            state
                .runs
                .completed
                .push(terminal_run(id, 10 + id, 10 + id));
        }
        assert!(!terminal_runs_checkpoint_due(
            &state,
            Some(EventSeq::new(10)),
            &[EventSeq::new(19)],
        ));

        state.runs.completed.push(terminal_run(10, 20, 20));
        assert!(terminal_runs_checkpoint_due(
            &state,
            Some(EventSeq::new(10)),
            &[EventSeq::new(20)],
        ));
    }

    #[test]
    fn newly_terminal_run_over_one_hundred_events_checkpoints_immediately() {
        let mut state = harness::CoreAgentState::default();
        state.runs.completed.push(terminal_run(1, 1, 100));
        assert!(!terminal_runs_checkpoint_due(
            &state,
            None,
            &[EventSeq::new(100)],
        ));

        state.runs.completed.clear();
        state.runs.completed.push(terminal_run(2, 101, 201));
        assert!(!terminal_runs_checkpoint_due(&state, None, &[]));
        assert!(terminal_runs_checkpoint_due(
            &state,
            None,
            &[EventSeq::new(201)],
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn malformed_checkpoint_falls_back_to_authoritative_log() {
        let (sessions, blobs, session_id, record) = session_with_open_event().await;
        let malformed = b"not a checkpoint".to_vec();
        let state_ref = blobs
            .put_bytes(malformed.clone())
            .await
            .expect("store malformed blob");
        assert!(
            sessions
                .advance_checkpoint(AdvanceSessionCheckpoint {
                    checkpoint: SessionCheckpoint {
                        session_id,
                        through_seq: record.head.clone().expect("head").seq,
                        format_version: SESSION_CHECKPOINT_FORMAT_VERSION,
                        state_ref,
                        lineage_source_session_id: None,
                        lineage_source_seq: None,
                        byte_len: malformed.len() as u64,
                        created_at_ms: 3,
                    },
                })
                .await
                .expect("advance malformed checkpoint")
        );

        let loaded = load_reduction(&sessions, &blobs, &record)
            .await
            .expect("fallback replay");
        assert!(loaded.full_replay);
        assert_eq!(
            loaded.reduced.core_state.reduced_to.as_ref().map(|p| p.seq),
            record.head.map(|head| head.seq)
        );
    }
}
