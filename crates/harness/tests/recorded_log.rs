//! A session log outlives every release that does not purge the database, and a log that
//! does not decode or reduce cannot load: nothing else can rebuild that session. This holds
//! a small log recorded earlier against today's types.
use harness::session::StoredSessionEntry;
use harness::*;
use serde_json::json;

const RECORDED_LOG: &str = include_str!("fixtures/recorded_session_log.json");

const WHAT_TO_DO: &str = "\
A log recorded by an earlier build stopped loading, and every stored session like it would stop \
too. Change the type instead of this fixture: a new event field gets a serde default, removing a \
field is harmless, and an event variant is never renamed or removed while logs hold it. The \
reducer checks structure only, so it must not refuse what an earlier build accepted. Replace the \
fixture only for a breaking release that purges stored sessions.";

fn recorded_log() -> Vec<StoredSessionEntry> {
    serde_json::from_str(RECORDED_LOG).unwrap_or_else(|error| {
        panic!("the recorded log's envelope does not decode: {error}\n\n{WHAT_TO_DO}")
    })
}

#[test]
fn a_log_recorded_by_an_earlier_build_still_decodes_and_reduces() {
    let mut state = CoreAgentState::new();
    for entry in &recorded_log() {
        let label = format!("seq {} ({})", entry.position.seq, entry.event.kind);
        let decoded = CoreAgentCodec
            .decode_entry(entry)
            .unwrap_or_else(|error| panic!("{label} does not decode: {error}\n\n{WHAT_TO_DO}"));
        apply_event(&mut state, &decoded)
            .unwrap_or_else(|error| panic!("{label} does not reduce: {error}\n\n{WHAT_TO_DO}"));
    }

    assert_eq!(
        state.lifecycle.status,
        CoreAgentStatus::Closed,
        "{WHAT_TO_DO}"
    );
    let finished: Vec<_> = state.runs.finished.iter().map(|run| run.status).collect();
    assert_eq!(
        finished,
        [RunStatus::Completed, RunStatus::Failed],
        "{WHAT_TO_DO}"
    );
    assert_eq!(
        state.runs.finished[1]
            .failure
            .as_ref()
            .map(|failure| &failure.kind),
        Some(&RunFailureKind::ModelFailure),
        "{WHAT_TO_DO}"
    );
    assert!(
        state.tooling.tools.contains_key(&ToolId::new("read_file")),
        "{WHAT_TO_DO}"
    );
}

/// Skipping what it does not know would load a session with missing history and no sign of
/// it. A rollback to a build that predates an event fails loudly instead.
#[test]
fn an_unknown_event_kind_is_a_decode_error_and_never_skipped() {
    let unknown = StoredEvent::new(
        "lightspeed.core.future.happened",
        1,
        json!({ "future": { "kind": "happened" } }),
    );
    assert!(CoreAgentCodec.decode_event(&unknown).is_err());

    let unknown_variant = StoredEvent::new(
        "lightspeed.core.run.happened",
        1,
        json!({ "run": { "kind": "happened" } }),
    );
    assert!(CoreAgentCodec.decode_event(&unknown_variant).is_err());
}
