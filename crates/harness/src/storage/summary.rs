//! Pure display projection over committed events and bounded, resolved text.
use super::session::{SessionActivity, SessionRecord};
use crate::{
    ContextEntry, ContextEntryKind, ContextEntrySource, ContextEvent, ContextMessageRole,
    CoreAgentCodec, CoreAgentEvent, CoreAgentLifecycleEvent, EnvironmentEvent, RunEvent,
    RunFailureKind, RunId, ToolEvent, WorkspaceAttachmentTarget, event_kind,
    session::StoredSessionEntry,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionSummary {
    pub activity_phase: Option<String>,
    pub activity_since_ms: Option<u64>,
    pub last_run: Option<LastRun>,
    pub execution_failure: Option<ExecutionFailure>,
    pub execution_failure_at_ms: Option<u64>,
    pub last_message_preview: Option<String>,
    pub last_message_role: Option<ContextMessageRole>,
    pub last_message_at_ms: Option<u64>,
    pub last_message_origin: Option<String>,
    pub active_environment_id: Option<String>,
    pub attachments: Attachments,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecutionFailure {
    Failed,
    Terminated,
    TimedOut,
    Missing,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachments {
    pub workspaces: Vec<String>,
    pub environments: Vec<String>,
    pub mcp_servers: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastRun {
    pub run_id: RunId,
    pub status: crate::RunStatus,
    pub failure_kind: Option<RunFailureKind>,
    pub finished_at_ms: u64,
}

/// Only these events need decoding or payload resolution on the append path.
fn needs_payload(kind: &str) -> bool {
    matches!(
        kind,
        event_kind::LIFECYCLE_OPENED
            | event_kind::LIFECYCLE_CONFIG_CHANGED
            | event_kind::CONTEXT_ENTRIES_APPLIED
            | event_kind::CONTEXT_KEY_PREFIX_REPLACED
            | event_kind::ENVIRONMENT_ACTIVE_SET
            | event_kind::ENVIRONMENT_ACTIVE_CLEARED
            | event_kind::RUN_COMPLETED
            | event_kind::RUN_FAILED
            | event_kind::RUN_CANCELLED
            | event_kind::RUN_FORCE_CANCELLED
            | event_kind::RUN_QUEUED_CANCELLED
            | event_kind::TOOL_BATCH_PARKED
            | event_kind::TOOL_BATCH_UNPARKED
    )
}

pub fn message_entries(entry: &StoredSessionEntry) -> Vec<ContextEntry> {
    if !matches!(
        entry.event.kind.as_str(),
        event_kind::CONTEXT_ENTRIES_APPLIED | event_kind::CONTEXT_KEY_PREFIX_REPLACED
    ) {
        return Vec::new();
    }
    match CoreAgentCodec.decode_event(&entry.event) {
        Ok(CoreAgentEvent::Context(
            ContextEvent::EntriesApplied { entries, .. }
            | ContextEvent::KeyPrefixReplaced { entries, .. },
        )) => entries,
        _ => Vec::new(),
    }
}

pub fn is_user_text(entry: &ContextEntry) -> bool {
    matches!(
        entry.kind,
        ContextEntryKind::Message {
            role: ContextMessageRole::User
        }
    ) && !matches!(entry.source, ContextEntrySource::Runtime { .. })
        && entry.content.provider_kind.is_none()
        && entry
            .content
            .media_type
            .as_deref()
            .is_none_or(|mime| mime.starts_with("text/"))
        && !entry
            .preview
            .as_deref()
            .is_some_and(|preview| preview.starts_with("[document"))
}

/// Whether an append needs the previous display projection. Other log events
/// can advance the session head without reading or writing the summary row.
pub fn affects_projection(kind: &str) -> bool {
    needs_payload(kind)
        || matches!(
            kind,
            event_kind::LIFECYCLE_CLOSED
                | event_kind::RUN_STARTED
                | event_kind::APPROVAL_RUN_PARKED
                | event_kind::APPROVAL_RUN_UNPARKED
                | event_kind::TURN_STARTED
                | event_kind::TURN_GENERATION_REQUESTED
                | event_kind::TOOL_CALL_STARTED
                | event_kind::WORKFLOW_TOOL_CONFIG_DECLARED_BINDINGS_ADMITTED
        )
}

pub fn project_summary(
    record: &mut SessionRecord,
    entry: &StoredSessionEntry,
    text: impl Fn(&ContextEntry) -> Option<String>,
) {
    if !affects_projection(entry.event.kind.as_str()) {
        return;
    }
    let before = (
        record.activity,
        record.lifecycle_status,
        record.lifecycle_controlled,
        record.summary.clone(),
    );
    super::session::apply_lifecycle_projection(record, entry);
    if needs_payload(entry.event.kind.as_str())
        && let Ok(event) = CoreAgentCodec.decode_event(&entry.event)
    {
        match event {
            CoreAgentEvent::Lifecycle(
                CoreAgentLifecycleEvent::Opened { config }
                | CoreAgentLifecycleEvent::ConfigChanged { config, .. },
            ) => {
                record.summary.attachments = Attachments {
                    workspaces: config
                        .features
                        .vfs
                        .iter()
                        .flat_map(|vfs| &vfs.workspaces)
                        .filter_map(|attachment| match &attachment.target {
                            WorkspaceAttachmentTarget::Workspace { workspace_id } => {
                                Some(workspace_id.clone())
                            }
                            _ => None,
                        })
                        .collect(),
                    environments: config
                        .features
                        .environments
                        .iter()
                        .flat_map(|env| &env.environments)
                        .map(|attachment| attachment.environment_id.clone())
                        .collect(),
                    mcp_servers: config
                        .features
                        .mcp
                        .iter()
                        .flat_map(|mcp| &mcp.servers)
                        .map(|attachment| attachment.server_id.clone())
                        .collect(),
                };
                record.summary.attachments.workspaces.sort();
                record.summary.attachments.workspaces.dedup();
                record.summary.attachments.environments.sort();
                record.summary.attachments.environments.dedup();
                record.summary.attachments.mcp_servers.sort();
                record.summary.attachments.mcp_servers.dedup();
            }
            CoreAgentEvent::Environment(EnvironmentEvent::ActiveEnvironmentSet {
                environment_id,
            }) => record.summary.active_environment_id = Some(environment_id.to_string()),
            CoreAgentEvent::Environment(EnvironmentEvent::ActiveEnvironmentCleared) => {
                record.summary.active_environment_id = None
            }
            CoreAgentEvent::Tool(ToolEvent::BatchParked { .. }) => {
                record.summary.activity_phase = Some("awaiting".into())
            }
            CoreAgentEvent::Tool(ToolEvent::BatchUnparked { .. }) => {
                record.summary.activity_phase = None
            }
            CoreAgentEvent::Run(run) => {
                let (run_id, status, failure_kind) = match run {
                    RunEvent::Completed { run_id, .. } => {
                        (run_id, crate::RunStatus::Completed, None)
                    }
                    RunEvent::Failed { run_id, failure } => {
                        (run_id, crate::RunStatus::Failed, Some(failure.kind))
                    }
                    RunEvent::Cancelled { run_id }
                    | RunEvent::ForceCancelled { run_id }
                    | RunEvent::QueuedCancelled { run_id, .. } => {
                        (run_id, crate::RunStatus::Cancelled, None)
                    }
                    _ => return,
                };
                record.summary.last_run = Some(LastRun {
                    run_id,
                    status,
                    failure_kind,
                    finished_at_ms: entry.observed_at_ms,
                });
            }
            CoreAgentEvent::Context(
                ContextEvent::EntriesApplied { entries, .. }
                | ContextEvent::KeyPrefixReplaced { entries, .. },
            ) => {
                for message in entries {
                    let ContextEntryKind::Message { role } = message.kind else {
                        continue;
                    };
                    if matches!(message.source, ContextEntrySource::Runtime { .. }) {
                        continue;
                    }
                    let content = match role {
                        ContextMessageRole::Assistant => message.preview.clone(),
                        ContextMessageRole::User if is_user_text(&message) => text(&message),
                        _ => None,
                    };
                    let Some(preview) = content.and_then(|content| plain_preview(&content)) else {
                        continue;
                    };
                    record.summary.last_message_preview = Some(preview);
                    record.summary.last_message_role = Some(role);
                    record.summary.last_message_at_ms = Some(entry.observed_at_ms);
                    record.summary.last_message_origin = message.origin;
                }
            }
            _ => {}
        }
    }
    if matches!(
        entry.event.kind.as_str(),
        event_kind::RUN_STARTED
            | event_kind::RUN_COMPLETED
            | event_kind::RUN_FAILED
            | event_kind::RUN_CANCELLED
            | event_kind::RUN_FORCE_CANCELLED
            | event_kind::LIFECYCLE_CLOSED
    ) {
        record.summary.execution_failure = None;
        record.summary.execution_failure_at_ms = None;
        record.summary.activity_phase = None;
    }
    if record.activity == SessionActivity::Idle {
        record.summary.activity_since_ms = None;
    } else if before.0 != record.activity
        || before.3.activity_phase != record.summary.activity_phase
    {
        record.summary.activity_since_ms = Some(entry.observed_at_ms);
    }
    if before
        != (
            record.activity,
            record.lifecycle_status,
            record.lifecycle_controlled,
            record.summary.clone(),
        )
    {
        record.revision = record.revision.saturating_add(1);
    }
}

/// Same bounded plain-line policy as the UI: skip fenced code, retain link
/// labels and snake_case, strip markup and collapse whitespace.
pub fn plain_preview(markdown: &str) -> Option<String> {
    let mut without_code = String::new();
    for (index, part) in markdown.split("```").enumerate() {
        if index % 2 == 0 {
            without_code.push_str(part);
            without_code.push(' ');
        }
    }
    let mut words = String::new();
    for line in without_code.lines() {
        let trimmed = line.trim_start();
        let indent = line[..line.len() - trimmed.len()].chars().count();
        let mut line = line;
        if indent <= 3 {
            line = trimmed;
            let headings = line.bytes().take_while(|byte| *byte == b'#').count();
            if (1..=6).contains(&headings) && line[headings..].starts_with(char::is_whitespace) {
                line = line[headings..].trim_start();
            } else if let Some(quoted) = line.strip_prefix('>') {
                line = quoted.trim_start();
            } else if line.starts_with(['-', '*', '+'])
                && line[1..].starts_with(char::is_whitespace)
            {
                line = line[1..].trim_start();
            } else {
                let digits = line.bytes().take_while(u8::is_ascii_digit).count();
                if digits > 0
                    && line[digits..].starts_with(['.', ')'])
                    && line[digits + 1..].starts_with(char::is_whitespace)
                {
                    line = line[digits + 1..].trim_start();
                }
            }
        }
        let mut rest = line;
        while !rest.is_empty() {
            let label_start = if rest.starts_with("![") {
                Some(2)
            } else if rest.starts_with('[') {
                Some(1)
            } else {
                None
            };
            if let Some(start) = label_start
                && let Some(end) = rest.find("](")
                && let Some(close) = rest[end + 2..].find(')')
            {
                words.push_str(&rest[start..end]);
                rest = &rest[end + 3 + close..];
                continue;
            }
            if let Some(after) = rest.strip_prefix("~~") {
                rest = after;
                continue;
            }
            let ch = rest.chars().next().expect("nonempty remainder");
            if !matches!(ch, '*' | '`') {
                words.push(ch);
            }
            rest = &rest[ch.len_utf8()..];
        }
        words.push(' ');
    }
    let text = words.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        None
    } else if text.chars().count() > 200 {
        Some(format!(
            "{}…",
            text.chars().take(199).collect::<String>().trim_end()
        ))
    } else {
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "current_thread")]
    async fn projection_replays_activity_terminal_state_and_preserves_unrelated_failures() {
        use crate::storage::{CreateSession, InMemorySessionStore, SessionStore};
        use crate::{ApprovalEvent, EnvironmentId, RunStatus, SessionId};
        let initial = InMemorySessionStore::new()
            .create_session(CreateSession {
                session_id: SessionId::new("summary-replay"),
                display_name: None,
                metadata: Default::default(),
                origin: None,
                delete_after_close_ms: None,
                created_at_ms: 1,
            })
            .await
            .unwrap();
        let run_id = RunId::new(1);
        let events = [
            CoreAgentEvent::Run(RunEvent::Started { run_id }),
            CoreAgentEvent::Environment(EnvironmentEvent::ActiveEnvironmentSet {
                environment_id: EnvironmentId::new("machine"),
            }),
            CoreAgentEvent::Approval(ApprovalEvent::RunParked { run_id }),
            CoreAgentEvent::Approval(ApprovalEvent::RunUnparked { run_id }),
            CoreAgentEvent::Run(RunEvent::QueuedCancelled {
                run_id: RunId::new(2),
                requested_by: None,
            }),
            CoreAgentEvent::Run(RunEvent::Completed {
                run_id,
                output: None,
            }),
        ];
        let entries: Vec<_> = events
            .into_iter()
            .enumerate()
            .map(|(index, event)| {
                let encoded = CoreAgentCodec
                    .encode_uncommitted(&crate::UncommittedCoreAgentEvent {
                        observed_at_ms: 10 + index as u64,
                        joins: Default::default(),
                        event,
                    })
                    .unwrap();
                StoredSessionEntry {
                    position: crate::SessionPosition {
                        seq: crate::session::EventSeq::new(index as u64 + 1),
                    },
                    observed_at_ms: encoded.observed_at_ms,
                    joins: encoded.joins,
                    event: encoded.event,
                }
            })
            .collect();
        let mut record = initial.clone();
        record.summary.execution_failure = Some(ExecutionFailure::Missing);
        project_summary(&mut record, &entries[0], |_| None);
        assert_eq!(record.summary.execution_failure, None);
        assert_eq!(record.summary.activity_since_ms, Some(10));
        project_summary(&mut record, &entries[1], |_| None);
        project_summary(&mut record, &entries[2], |_| None);
        assert_eq!(record.activity, SessionActivity::Waiting);
        assert_eq!(record.summary.activity_since_ms, Some(12));
        project_summary(&mut record, &entries[3], |_| None);
        assert_eq!(record.activity, SessionActivity::Working);
        assert_eq!(record.summary.activity_since_ms, Some(13));
        project_summary(&mut record, &entries[4], |_| None);
        assert_eq!(
            record.activity,
            SessionActivity::Working,
            "cancelling a queued run must not hide the active run"
        );
        project_summary(&mut record, &entries[5], |_| None);
        assert_eq!(record.activity, SessionActivity::Idle);
        assert_eq!(record.summary.activity_since_ms, None);
        assert_eq!(
            record.summary.last_run.as_ref().unwrap().status,
            RunStatus::Completed
        );
        let mut replayed = initial;
        for entry in entries {
            project_summary(&mut replayed, &entry, |_| None);
        }
        assert_eq!(record, replayed);
        record.summary.execution_failure = Some(ExecutionFailure::Failed);
        let unrelated = StoredSessionEntry {
            position: crate::SessionPosition {
                seq: crate::session::EventSeq::new(7),
            },
            observed_at_ms: 20,
            joins: Default::default(),
            event: crate::StoredEvent {
                kind: "unrelated".into(),
                version: 1,
                payload: serde_json::json!({}),
            },
        };
        let before = record.clone();
        project_summary(&mut record, &unrelated, |_| None);
        assert_eq!(record, before);
    }

    #[test]
    fn preview_reduces_markup_and_bounds_unicode() {
        assert_eq!(
            plain_preview(
                "# Hello **world**\n- [snake_case](https://example.test)\n```rust\nsecret\n```"
            ),
            Some("Hello world snake_case".into())
        );
        assert_eq!(plain_preview("```rust\nonly code"), None);
        assert_eq!(
            plain_preview("#tag ~home ~~gone~~ snake_case"),
            Some("#tag ~home gone snake_case".into())
        );
        let long = plain_preview(&"界".repeat(250)).unwrap();
        assert_eq!(long.chars().count(), 200);
        assert!(long.ends_with('…'));
    }
}
