//! What `session/runs/read` keeps of one run's events. Most runs fit whole. A run past
//! the limit keeps its opening, so its input renders, and its most recent turns.
use std::collections::VecDeque;

use api::AgentApiError;
use api_projection::{decode_stored_entry, map_session_store_error};
use harness::storage::{ReadSessionEventRange, SessionRecord, SessionStore};
use harness::{
    CoreAgentCodec, CoreAgentEntry, CoreAgentEvent, EventSeq, LlmUsage, RunId, TurnEvent,
};

/// A store page of a run's interval.
const RUN_EVENT_PAGE: usize = 512;

pub(super) struct RunEvents {
    pub(super) entries: Vec<CoreAgentEntry>,
    pub(super) earlier_events_omitted: bool,
    /// Summed over every event seen, including the ones the window let go.
    pub(super) usage: Option<LlmUsage>,
}

/// Read a run's sequence interval in pages. Only the run's own events count toward
/// `capacity`: one queued behind a long run shares that run's interval without being
/// long itself.
pub(super) async fn read_run_events(
    store: &dyn SessionStore,
    record: &SessionRecord,
    run_id: RunId,
    first_seq: EventSeq,
    terminal_seq: Option<EventSeq>,
    capacity: usize,
) -> Result<RunEvents, AgentApiError> {
    let through = terminal_seq
        .or_else(|| record.head.as_ref().map(|head| head.seq))
        .unwrap_or(first_seq);
    let mut after = EventSeq::new(first_seq.as_u64().saturating_sub(1));
    let run = run_id.as_u64().to_string();
    let mut window = RunEventWindow::new(capacity);
    loop {
        let page = store
            .read_range(ReadSessionEventRange {
                session_id: record.session_id.clone(),
                after,
                through,
                limit: RUN_EVENT_PAGE,
            })
            .await
            .map_err(map_session_store_error)?;
        for entry in &page.entries {
            if entry.joins.get("run_id") == Some(&run) {
                window.push(decode_stored_entry(&CoreAgentCodec, entry)?);
            }
        }
        match page.next_after {
            Some(next_after) if !page.complete => after = next_after,
            _ => return Ok(window.finish()),
        }
    }
}

struct RunEventWindow {
    opening: Vec<CoreAgentEntry>,
    opening_complete: bool,
    recent: VecDeque<CoreAgentEntry>,
    capacity: usize,
    omitted: bool,
    usage: Option<LlmUsage>,
}

fn starts_a_turn(entry: &CoreAgentEntry) -> bool {
    matches!(entry.event, CoreAgentEvent::Turn(TurnEvent::Started { .. }))
}

impl RunEventWindow {
    fn new(capacity: usize) -> Self {
        Self {
            opening: Vec::new(),
            opening_complete: false,
            recent: VecDeque::new(),
            capacity,
            omitted: false,
            usage: None,
        }
    }

    fn push(&mut self, entry: CoreAgentEntry) {
        harness::accumulate_run_usage(&mut self.usage, &entry);
        if !self.opening_complete {
            self.opening_complete = starts_a_turn(&entry);
            self.opening.push(entry);
            return;
        }
        if self.recent.len() == self.capacity {
            self.recent.pop_front();
            self.omitted = true;
        }
        self.recent.push_back(entry);
    }

    /// Projection is stateful across a turn's events, such as a call and its result. So
    /// what is kept starts on a turn, even if the most recent turn alone fills the window.
    fn finish(self) -> RunEvents {
        let Self {
            mut opening,
            recent,
            omitted,
            usage,
            ..
        } = self;
        if omitted {
            opening.extend(recent.into_iter().skip_while(|entry| !starts_a_turn(entry)));
        } else {
            opening.extend(recent);
        }
        RunEvents {
            entries: opening,
            earlier_events_omitted: omitted,
            usage,
        }
    }
}

#[cfg(test)]
mod tests {
    use harness::session::SessionPosition;
    use harness::storage::{AppendSessionEvents, CreateSession, InMemorySessionStore};
    use harness::{
        CoreAgentEventProposal, CoreAgentJoins, LlmFinish, LlmGenerationFacts, LlmGenerationStatus,
        RunEvent, SessionId, TurnId,
    };

    use super::*;

    fn entry(seq: u64, event: CoreAgentEvent) -> CoreAgentEntry {
        CoreAgentEntry {
            position: SessionPosition {
                seq: EventSeq::new(seq),
            },
            observed_at_ms: seq,
            joins: Default::default(),
            event,
        }
    }

    fn turn_started(seq: u64) -> CoreAgentEntry {
        entry(
            seq,
            CoreAgentEvent::Turn(TurnEvent::Started {
                run_id: RunId::new(1),
                turn_id: TurnId::new(seq),
            }),
        )
    }

    fn other(seq: u64) -> CoreAgentEntry {
        entry(
            seq,
            CoreAgentEvent::Run(RunEvent::Started {
                run_id: RunId::new(1),
            }),
        )
    }

    fn seqs(events: &RunEvents) -> Vec<u64> {
        events
            .entries
            .iter()
            .map(|entry| entry.position.seq.as_u64())
            .collect()
    }

    /// Two runs' events interleaved in one log, the way a queued run's interval spans
    /// the run ahead of it.
    #[tokio::test(flavor = "current_thread")]
    async fn only_a_runs_own_events_count_and_a_read_spans_pages() {
        let session_id = SessionId::new("3f2c1e9a-7b1d-4f55-9a8e-2c6c1d9f4a11");
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
            .expect("create");
        // One event in a hundred belongs to run 2, the rest to run 1.
        let events = (1..=RUN_EVENT_PAGE as u64 * 2)
            .map(|seq| {
                let run_id = RunId::new(if seq % 100 == 0 { 2 } else { 1 });
                CoreAgentCodec
                    .encode_uncommitted(
                        &CoreAgentEventProposal::new(
                            CoreAgentJoins {
                                run_id: Some(run_id),
                                ..Default::default()
                            },
                            CoreAgentEvent::Turn(TurnEvent::Started {
                                run_id,
                                turn_id: TurnId::new(seq),
                            }),
                        )
                        .into_uncommitted(seq),
                    )
                    .expect("encode")
            })
            .collect();
        store
            .append(AppendSessionEvents {
                session_id: session_id.clone(),
                expected_head: None,
                events,
            })
            .await
            .expect("append");
        let record = store
            .load_session(&session_id)
            .await
            .expect("load")
            .expect("session");
        let events = read_run_events(&store, &record, RunId::new(2), EventSeq::new(1), None, 50)
            .await
            .expect("read");
        assert!(
            !events.earlier_events_omitted,
            "only run 2's own events count"
        );
        assert_eq!(events.entries.len(), RUN_EVENT_PAGE * 2 / 100);
        assert!(
            events
                .entries
                .iter()
                .all(|entry| entry.position.seq.as_u64() % 100 == 0)
        );

        let events = read_run_events(&store, &record, RunId::new(1), EventSeq::new(1), None, 50)
            .await
            .expect("read");
        assert!(events.earlier_events_omitted);
        // The opening turn, then the last 50 of the run's own events.
        assert_eq!(events.entries.len(), 51);
        assert_eq!(events.entries[0].position.seq.as_u64(), 1);
    }

    #[test]
    fn a_run_that_fits_is_kept_whole() {
        let mut window = RunEventWindow::new(3);
        for event in [
            other(1),
            turn_started(2),
            other(3),
            turn_started(4),
            other(5),
        ] {
            window.push(event);
        }
        let events = window.finish();
        assert_eq!(seqs(&events), [1, 2, 3, 4, 5]);
        assert!(!events.earlier_events_omitted);
    }

    #[test]
    fn a_long_run_keeps_its_opening_and_recent_events_from_a_turn_boundary() {
        let mut window = RunEventWindow::new(4);
        for event in [
            other(1),
            turn_started(2),
            other(3),
            other(4),
            turn_started(5),
            other(6),
            other(7),
            turn_started(8),
            other(9),
        ] {
            window.push(event);
        }
        let events = window.finish();
        assert!(events.earlier_events_omitted);
        // Events 6 and 7 fit the window, but belong to a turn whose start did not.
        assert_eq!(seqs(&events), [1, 2, 8, 9]);
    }

    #[test]
    fn one_turn_past_the_limit_keeps_only_the_opening() {
        let mut window = RunEventWindow::new(2);
        for event in [other(1), turn_started(2), other(3), other(4), other(5)] {
            window.push(event);
        }
        let events = window.finish();
        assert!(events.earlier_events_omitted);
        assert_eq!(seqs(&events), [1, 2]);
    }

    #[test]
    fn usage_counts_the_events_the_window_let_go() {
        let generation = |seq, input_tokens| {
            entry(
                seq,
                CoreAgentEvent::Turn(TurnEvent::GenerationCompleted {
                    run_id: RunId::new(1),
                    turn_id: TurnId::new(1),
                    status: LlmGenerationStatus::Succeeded,
                    facts: LlmGenerationFacts {
                        duration_ms: None,
                        provider_response_id: None,
                        finish: LlmFinish::Stop,
                        usage: Some(LlmUsage {
                            input_tokens: Some(input_tokens),
                            output_tokens: None,
                            reasoning_tokens: None,
                            total_tokens: None,
                            cached_input_tokens: None,
                            cache_write_input_tokens: None,
                            cache_miss_input_tokens: None,
                        }),
                        tool_calls: Vec::new(),
                        approval_requests: Vec::new(),
                        context_token_estimate: None,
                    },
                }),
            )
        };
        let mut window = RunEventWindow::new(1);
        window.push(turn_started(1));
        for seq in 2..=4 {
            window.push(generation(seq, 10));
        }
        let events = window.finish();
        assert!(events.earlier_events_omitted);
        assert_eq!(events.usage.and_then(|usage| usage.input_tokens), Some(30));
    }
}
