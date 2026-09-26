//! Controller-facing session operations: create/reconcile a bot's managed
//! session, read status and usage, start/steer/append deliveries, close
//! sessions and count descendants, read tool invocations and blobs.
//!
//! Everything goes through the universe's in-process `GatewayAgentApi` at
//! the same boundaries the JSON-RPC methods expose (`session/managed/start`,
//! `session/runs/start`, `session/context/append`, ...), so a bot session is
//! an ordinary managed session whose lifecycle controller — and the receiver
//! of every pushed `bot_*` invocation — is the bot controller workflow.

use std::collections::{BTreeMap, BTreeSet};

use api::{
    AgentApiError, AgentApiErrorKind, AgentApiService as _, AgentProfile, ContextAppendEntry,
    ContextAppendParams, ContextAppendResponse, ContextAppendStatus, EventCursor,
    InlineAgentProfile, InputItem, ManagedSessionStartParams, ManagedSessionWorkflowToolsInput,
    ProfileApplyParams, ProfileDocument, ProfileInstructions, ProfileSource, RunStartParams,
    RunStartSource, RunStatus, RunSteerParams, RunTerminalNotificationInput, SessionCloseParams,
    SessionEventKindView, SessionEventView, SessionEventsReadParams, SessionLifecycleStatus,
    SessionListParams, SessionReadParams, SessionRenameParams, SessionStatus, SessionSummaryView,
    SessionView, WorkflowEndpointInput, WorkflowToolDeclarationInput,
};
use bots::{
    ids::appended_event_context_key,
    tools::{
        BOT_TOOL_DESCRIPTION_NAMES, BOT_TOOL_NAMES, BOT_TOOL_SCHEMA_NAMES, BotSessionInstructions,
        bot_instructions, bot_tool_description, bot_tool_schema, bot_workflow_tool_declarations,
        compose_instructions,
    },
    views::{delivery_input_items, steer_input_items},
};
use engine::{
    BlobRef,
    storage::{BlobStore, BlobStoreError},
};
use profiles::{ProfileError, ProfileStore as _};
use temporal_workflow::bots::*;
use temporalio_common::error::ApplicationFailure;
use temporalio_sdk::activities::ActivityError;

use crate::gateway::GatewayAgentApi;

/// Managed-session workflow-tool declaration version bot sessions are
/// created with (the only version the core admits).
const MANAGED_TOOLS_VERSION: u32 = 1;
/// Page size and page bound when walking a session's sub-agent lineage.
const DESCENDANT_PAGE_LIMIT: u32 = 200;
const DESCENDANT_MAX_PAGES: usize = 10;
/// Page size when pulling workflow-tool invocations from the session log.
const EVENT_PAGE_LIMIT: u32 = 500;
/// `session/context/append` accepts at most this many entries per call.
const CONTEXT_APPEND_BATCH: usize = 64;

// ── Error classification ────────────────────────────────────────────────────

/// A failure no retry heals: malformed input, a missing referent.
pub(super) fn non_retryable(message: impl std::fmt::Display) -> ActivityError {
    ActivityError::application(ApplicationFailure::non_retryable(anyhow::anyhow!(
        "{message}"
    )))
}

/// A failure the activity retry policy absorbs: store, workflow, transport.
pub(super) fn retryable(message: impl std::fmt::Display) -> ActivityError {
    ActivityError::application(ApplicationFailure::new(anyhow::anyhow!("{message}")))
}

/// Classify a core API error. Invalid requests, missing referents and
/// refusals of the bot's own work never heal on retry: the failure is
/// recorded at once. A rejection (a busy
/// session), a conflict (a lost race), or an internal / transport failure
/// may heal.
pub(super) fn activity_error(context: &str, error: AgentApiError) -> ActivityError {
    let message = format!("{context}: {error}");
    if matches!(
        error.kind,
        AgentApiErrorKind::InvalidRequest
            | AgentApiErrorKind::NotFound
            | AgentApiErrorKind::Forbidden
    ) {
        non_retryable(message)
    } else {
        retryable(message)
    }
}

/// The session exists under another immutable tool declaration (an older
/// `BOT_TOOLS_REVISION`, another controller): the core refuses the managed
/// start with a fingerprint conflict.
fn is_declaration_mismatch(error: &AgentApiError) -> bool {
    error.kind == AgentApiErrorKind::Conflict
        && (error.message.contains("fingerprint")
            || error
                .message
                .contains("managed-session controller, receiver, or tool declaration conflicts"))
}

/// The engine refused the profile's config for this session in a way no
/// retry fixes: an invalid document, or a command rejection of the
/// provider-compatibility kind (the rejection kind leads the message).
fn is_profile_unapplicable(error: &AgentApiError) -> bool {
    match error.kind {
        AgentApiErrorKind::InvalidRequest => true,
        AgentApiErrorKind::Rejected => {
            const KIND: &str = "ProviderCompatibility";
            error.message.starts_with(KIND)
                && !error.message[KIND.len()..]
                    .chars()
                    .next()
                    .is_some_and(|next| next.is_ascii_alphanumeric() || next == '_')
        }
        _ => false,
    }
}

// ── Shared helpers ──────────────────────────────────────────────────────────

async fn read_session_view(
    api: &GatewayAgentApi,
    session_id: &str,
) -> Result<SessionView, AgentApiError> {
    Ok(api
        .read_session(SessionReadParams {
            session_id: session_id.to_owned(),
            run_limit: None,
        })
        .await?
        .result
        .session)
}

async fn read_blob(blobs: &dyn BlobStore, blob_ref: &str) -> Result<Vec<u8>, ActivityError> {
    let parsed = BlobRef::parse(blob_ref)
        .map_err(|error| non_retryable(format!("invalid blob ref {blob_ref}: {error}")))?;
    blobs
        .read_bytes(&parsed)
        .await
        .map_err(|error| match error {
            BlobStoreError::NotFound { .. } => non_retryable(error),
            BlobStoreError::Store { .. } => retryable(error),
        })
}

/// A context append reports per-entry outcomes; a failed entry fails the
/// activity (the append is idempotent per key, so the retry is safe).
pub(super) fn check_context_append(response: &ContextAppendResponse) -> Result<(), ActivityError> {
    let failures: Vec<String> = response
        .results
        .iter()
        .filter(|result| result.status == ContextAppendStatus::Failed)
        .map(|result| {
            let reason = result
                .failure
                .as_ref()
                .map(|failure| failure.message.clone())
                .unwrap_or_else(|| "admission failed".to_owned());
            format!("{}: {reason}", result.key)
        })
        .collect();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(retryable(format!(
            "context append failed for {}",
            failures.join("; ")
        )))
    }
}

/// The controller's view of a session's status.
///
/// - `Idle` / `Busy` are what the projection says about an open session;
///   `Busy` names the running run when there is one (a cancelling run still
///   occupies the session but takes no steering, so it reports no id).
/// - `NotLoaded` — a session row whose workflow has not opened it yet — reads
///   as idle: the controller's next step (a run start) is what opens it.
/// - `Error` reads as busy without a run: the workflow reported itself
///   broken, so the controller waits for the next status read instead of
///   starting work into it.
pub(super) fn session_status_from_view(session: &SessionView) -> BotSessionStatus {
    match session.status {
        SessionStatus::Idle | SessionStatus::NotLoaded => BotSessionStatus::Idle,
        SessionStatus::Active => BotSessionStatus::Busy {
            run_id: running_run(session).map(|run| run.id.clone()),
        },
        SessionStatus::Error => BotSessionStatus::Busy { run_id: None },
        SessionStatus::Closed => BotSessionStatus::Closed,
    }
}

/// The run steering can land on: the session's `running` run. Read from the
/// dedicated `active_run` fact, never the paged `runs` window — the newest
/// page can omit the executing run behind newer queued runs.
fn running_run(session: &SessionView) -> Option<&api::RunSummaryView> {
    session
        .active_run
        .as_ref()
        .filter(|run| run.status == RunStatus::Running)
}

/// One bound-tool invocation off the session log — every bound tool, not
/// only the controller's own: the caller correlates resolves and recognizes
/// carried tools by id.
fn tool_invocation(event: &SessionEventView) -> Option<BotToolInvocationRef> {
    match &event.kind {
        SessionEventKindView::WorkflowToolEmitted {
            invocation_id,
            tool_id,
            run_id,
            arguments_ref,
            ..
        } => Some(BotToolInvocationRef {
            invocation_id: invocation_id.clone(),
            tool_id: tool_id.clone(),
            run_id: run_id.clone(),
            arguments_ref: arguments_ref.clone(),
        }),
        _ => None,
    }
}

/// Whether a session created at `created_at_ms` counts against a window
/// starting at `since_ms` (a negative window start counts everything).
fn created_since(created_at_ms: u64, since_ms: i64) -> bool {
    u64::try_from(since_ms).is_ok_and(|since| created_at_ms >= since) || since_ms < 0
}

/// The sub-agent lineage under a root, bounded so a runaway tree cannot
/// stall the controller.
async fn list_descendants(
    api: &GatewayAgentApi,
    root_session_id: &str,
) -> Result<Vec<SessionSummaryView>, ActivityError> {
    let mut sessions = Vec::new();
    let mut cursor = None;
    for _ in 0..DESCENDANT_MAX_PAGES {
        let page = api
            .list_sessions(SessionListParams {
                cursor: cursor.take(),
                limit: Some(DESCENDANT_PAGE_LIMIT),
                root_session_id: Some(root_session_id.to_owned()),
                ..Default::default()
            })
            .await
            .map_err(|error| activity_error("list descendant sessions", error))?
            .result;
        sessions.extend(page.sessions);
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    Ok(sessions)
}

// ── ensure_session ──────────────────────────────────────────────────────────

/// The profile's base instructions: inline text, or the CAS blob it points
/// at; a profile without instructions contributes nothing.
async fn read_profile_instructions(
    blobs: &dyn BlobStore,
    profile: &AgentProfile,
) -> Result<String, ActivityError> {
    match &profile.document.instructions {
        None => Ok(String::new()),
        Some(ProfileInstructions::Text { text }) => Ok(text.clone()),
        Some(ProfileInstructions::TextRef { blob_ref }) => {
            let bytes = read_blob(blobs, blob_ref).await?;
            String::from_utf8(bytes).map_err(|error| {
                non_retryable(format!(
                    "profile {} instructions are not UTF-8: {error}",
                    profile.profile_id
                ))
            })
        }
    }
}

/// Descriptive metadata every bot-created session carries (routed ones
/// included), so a `bot=<id>` filter finds a bot's sessions.
fn bot_session_metadata(bot_id: &bots::BotId) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("source".to_owned(), "bot".to_owned()),
        ("bot".to_owned(), bot_id.to_string()),
    ])
}

/// The profile the bot's session runs: the catalog profile's creation defaults
/// and environment, with the composed (profile + bot) instructions inline so
/// the session pins exactly what the controller resolved.
fn resolve_bot_profile(profile: &AgentProfile, instructions: String) -> InlineAgentProfile {
    InlineAgentProfile {
        display_name: profile.display_name.clone(),
        description: profile.description.clone(),
        document: ProfileDocument {
            metadata: profile.document.metadata.clone(),
            retention: profile.document.retention.clone(),
            config: profile.document.config.clone(),
            instructions: Some(ProfileInstructions::Text { text: instructions }),
        },
    }
}

fn proposed_api_kind(profile: &InlineAgentProfile) -> Option<&str> {
    profile
        .document
        .config
        .as_ref()?
        .model
        .as_ref()
        .map(|model| model.api_kind.as_str())
}

fn pinned_api_kind(session: &SessionView) -> Option<&str> {
    session
        .config
        .as_ref()?
        .model
        .as_ref()
        .map(|model| model.api_kind.as_str())
}

/// Every tool asset the declarations reference, as the bytes stored in the
/// CAS: schemas as JSON, descriptions as UTF-8 text. Keyed by the schema /
/// description names [`bot_workflow_tool_declarations`] looks up.
#[allow(clippy::type_complexity)]
fn tool_asset_bytes() -> Result<(Vec<(&'static str, Vec<u8>)>, Vec<(&'static str, Vec<u8>)>), String>
{
    let schemas = BOT_TOOL_SCHEMA_NAMES
        .iter()
        .map(|name| {
            let schema =
                bot_tool_schema(name).ok_or_else(|| format!("missing bot tool schema {name}"))?;
            let bytes = serde_json::to_vec(schema)
                .map_err(|error| format!("encode bot tool schema {name}: {error}"))?;
            Ok((*name, bytes))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let descriptions = BOT_TOOL_DESCRIPTION_NAMES
        .iter()
        .map(|name| {
            let description = bot_tool_description(name)
                .ok_or_else(|| format!("missing bot tool description {name}"))?;
            Ok((*name, description.as_bytes().to_vec()))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok((schemas, descriptions))
}

#[derive(Default)]
struct ToolAssetRefs {
    schemas: BTreeMap<&'static str, String>,
    descriptions: BTreeMap<&'static str, String>,
}

/// Store the tool assets and return their refs. Content-addressed, so a
/// repeat is a no-op on the store and the declarations stay byte-stable
/// across ensures (their fingerprint must, or every ensure would rotate).
async fn put_tool_assets(blobs: &dyn BlobStore) -> Result<ToolAssetRefs, ActivityError> {
    let (schemas, descriptions) = tool_asset_bytes().map_err(non_retryable)?;
    let mut refs = ToolAssetRefs::default();
    for (name, bytes) in schemas {
        let blob_ref = blobs
            .put_bytes(bytes)
            .await
            .map_err(|error| retryable(format!("store bot tool schema {name}: {error}")))?;
        refs.schemas.insert(name, blob_ref.to_string());
    }
    for (name, bytes) in descriptions {
        let blob_ref = blobs
            .put_bytes(bytes)
            .await
            .map_err(|error| retryable(format!("store bot tool description {name}: {error}")))?;
        refs.descriptions.insert(name, blob_ref.to_string());
    }
    Ok(refs)
}

/// Carried declarations are opaque data authored by the admitting source
/// (a chat conversation's `message_*` tools). The only checks here are the
/// ones that would otherwise fail inside the core with a worse message: a
/// name collision with `bot_*` or among themselves.
fn validate_carried_declarations(
    declarations: &[WorkflowToolDeclarationInput],
) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for declaration in declarations {
        let name = declaration.definition.tool.name.as_str();
        if BOT_TOOL_NAMES.contains(&name) || !seen.insert(name) {
            return Err(format!("carried tool {name} collides with a declared tool"));
        }
    }
    Ok(())
}

async fn read_carried_declarations(
    blobs: &dyn BlobStore,
    tools_ref: &str,
) -> Result<Vec<WorkflowToolDeclarationInput>, ActivityError> {
    let bytes = read_blob(blobs, tools_ref).await?;
    let declarations: Vec<WorkflowToolDeclarationInput> =
        serde_json::from_slice(&bytes).map_err(|error| {
            non_retryable(format!("carried tool declarations are malformed: {error}"))
        })?;
    validate_carried_declarations(&declarations).map_err(non_retryable)?;
    Ok(declarations)
}

pub async fn ensure_session(
    api: &GatewayAgentApi,
    request: BotEnsureSessionRequest,
) -> Result<BotEnsureSessionResult, ActivityError> {
    let store = api.store();
    let profile = match store.read_agent_profile(&request.profile_id).await {
        Ok(profile) => profile,
        Err(error @ ProfileError::NotFound { .. }) => return Err(non_retryable(error)),
        Err(error) => return Err(retryable(error)),
    };
    let blobs: &dyn BlobStore = store.as_ref();
    let base_instructions = read_profile_instructions(blobs, &profile).await?;
    let instructions = compose_instructions(
        &base_instructions,
        &bot_instructions(
            &request.bot_id,
            BotSessionInstructions {
                session_id: &request.session_id,
                session_key: request.session_key.as_deref(),
                label: request.session_label.as_deref(),
            },
            request.brief.as_deref(),
            request.emit,
        ),
    );
    let resolved = resolve_bot_profile(&profile, instructions);

    let controller = WorkflowEndpointInput {
        workflow_id: request.controller.workflow_id.clone(),
        workflow_kind: request.controller.workflow_kind.clone(),
    };
    let assets = put_tool_assets(blobs).await?;
    let mut tools = bot_workflow_tool_declarations(
        controller.clone(),
        &assets.schemas,
        &assets.descriptions,
        request.self_config,
        request.emit,
    )
    .map_err(non_retryable)?;
    let carried = match request.tools_ref.as_deref() {
        Some(tools_ref) => read_carried_declarations(blobs, tools_ref).await?,
        None => Vec::new(),
    };
    let carried_tool_ids: Vec<String> = carried
        .iter()
        .map(|declaration| declaration.definition.tool_id.clone())
        .collect();
    tools.extend(carried);

    // The display name rides on creation only: an existing session keeps
    // its label (renames are a separate, label-only operation).
    if let Err(error) = api
        .start_managed_session(ManagedSessionStartParams {
            access: None,
            session_id: Some(request.session_id.clone()),
            display_name: request.display_name.clone(),
            metadata: bot_session_metadata(&request.bot_id),
            config: None,
            profile: Some(ProfileSource::Inline {
                profile: Box::new(resolved.clone()),
            }),
            delete_after_close_ms: None,
            workflow_tools: ManagedSessionWorkflowToolsInput {
                version: MANAGED_TOOLS_VERSION,
                lifecycle_controller: Some(controller),
                tools,
            },
        })
        .await
    {
        // Declarations are immutable per session: a session created under
        // an older tool revision cannot be upgraded in place. The controller
        // rotates to a successor instead of retrying forever.
        if is_declaration_mismatch(&error) {
            return Ok(BotEnsureSessionResult::DeclarationMismatch {
                message: format!(
                    "session {} was created under another tool declaration: {}",
                    request.session_id, error.message
                ),
            });
        }
        return Err(activity_error("start managed session", error));
    }

    if request.applied_profile_revision != Some(profile.revision) {
        // A session's provider api kind is pinned for its lifetime. A
        // profile that moved to another kind is valid for a fresh session
        // but not for this one: report that as unapplicable so the
        // controller rotates, rather than retrying into a degraded bot.
        // Checked structurally first; the engine's rejection is the backstop.
        if let Some(proposed) = proposed_api_kind(&resolved) {
            let current = read_session_view(api, &request.session_id)
                .await
                .map_err(|error| activity_error("read session", error))?;
            if let Some(pinned) = pinned_api_kind(&current)
                && pinned != proposed
            {
                return Ok(BotEnsureSessionResult::ProfileUnapplicable {
                    message: format!(
                        "session {} is pinned to provider api kind {pinned}; profile revision {} needs {proposed}",
                        request.session_id, profile.revision
                    ),
                });
            }
        }
        if let Err(error) = api
            .apply_profile(ProfileApplyParams {
                session_id: request.session_id.clone(),
                profile: ProfileSource::Inline {
                    profile: Box::new(resolved),
                },
                expected_config_revision: None,
                expected_tools_revision: None,
            })
            .await
        {
            if is_profile_unapplicable(&error) {
                return Ok(BotEnsureSessionResult::ProfileUnapplicable {
                    message: format!(
                        "session {} cannot take profile revision {}: {}",
                        request.session_id, profile.revision, error.message
                    ),
                });
            }
            // A busy session (`rejected`: no run may be active) is transient
            // and stays retryable.
            return Err(activity_error("apply profile", error));
        }
    }

    Ok(BotEnsureSessionResult::Ready {
        profile_revision: profile.revision,
        carried_tool_ids,
    })
}

// ── Reads ───────────────────────────────────────────────────────────────────

pub async fn rename_session(
    api: &GatewayAgentApi,
    request: BotRenameSessionRequest,
) -> Result<(), ActivityError> {
    api.rename_session(SessionRenameParams {
        session_id: request.session_id,
        display_name: request.display_name,
    })
    .await
    .map(|_| ())
    .map_err(|error| activity_error("rename session", error))
}

pub async fn read_session_status(
    api: &GatewayAgentApi,
    request: BotSessionRequest,
) -> Result<BotSessionStatus, ActivityError> {
    match read_session_view(api, &request.session_id).await {
        Ok(session) => Ok(session_status_from_view(&session)),
        Err(error) if error.kind == AgentApiErrorKind::NotFound => Ok(BotSessionStatus::Missing),
        Err(error) => Err(activity_error("read session", error)),
    }
}

pub async fn read_run_usage(
    api: &GatewayAgentApi,
    request: BotReadRunUsageRequest,
) -> Result<BotReadRunUsageResult, ActivityError> {
    let session = read_session_view(api, &request.session_id)
        .await
        .map_err(|error| activity_error("read session", error))?;
    // Absent when the provider reported nothing: a run without prompt
    // tokens carries no signal for the cache-hit accounting.
    let usage = session
        .runs
        .iter()
        .find(|run| run.id == request.run_id)
        .and_then(|run| run.usage.clone())
        .filter(|usage| usage.input_tokens.is_some_and(|tokens| tokens > 0));
    Ok(BotReadRunUsageResult { usage })
}

// ── Deliveries ──────────────────────────────────────────────────────────────

pub async fn start_run(
    api: &GatewayAgentApi,
    request: BotStartRunRequest,
) -> Result<BotStartRunResult, ActivityError> {
    // The terminal notification's destination is derived by the gateway
    // from the session's immutable lifecycle controller — the controller
    // itself — exactly as for a public `session/runs/start`.
    match api
        .start_run(RunStartParams {
            session_id: request.session_id,
            source: RunStartSource::Input {
                items: delivery_input_items(&request.events),
            },
            submission_id: Some(request.submission_id),
            config: None,
            notify_on_terminal: Some(RunTerminalNotificationInput {
                token: request.terminal_token,
            }),
        })
        .await
    {
        Ok(response) => Ok(BotStartRunResult::Started {
            run_id: response.result.run.id,
        }),
        Err(error)
            if matches!(
                error.kind,
                AgentApiErrorKind::Rejected | AgentApiErrorKind::Conflict
            ) =>
        {
            Ok(BotStartRunResult::Rejected {
                message: error.message,
            })
        }
        Err(error) => Err(activity_error("start run", error)),
    }
}

pub async fn steer_run(
    api: &GatewayAgentApi,
    request: BotSteerRunRequest,
) -> Result<BotSteerRunResult, ActivityError> {
    let session = read_session_view(api, &request.session_id)
        .await
        .map_err(|error| activity_error("read session", error))?;
    let Some(active) = running_run(&session) else {
        return Ok(BotSteerRunResult::NotRunning);
    };
    let run_id = active.id.clone();
    match api
        .steer_run(RunSteerParams {
            session_id: request.session_id,
            run_id: run_id.clone(),
            items: steer_input_items(&request.events),
        })
        .await
    {
        Ok(_) => Ok(BotSteerRunResult::Steered { run_id }),
        // The run reached terminal (or started cancelling) between the read
        // and the steer; the lane falls back to an ordinary run.
        Err(error)
            if matches!(
                error.kind,
                AgentApiErrorKind::Rejected | AgentApiErrorKind::NotFound
            ) =>
        {
            Ok(BotSteerRunResult::NotRunning)
        }
        Err(error) => Err(activity_error("steer run", error)),
    }
}

pub async fn append_context(
    api: &GatewayAgentApi,
    request: BotAppendContextRequest,
) -> Result<(), ActivityError> {
    let entries: Vec<ContextAppendEntry> = request
        .events
        .iter()
        .map(|event| ContextAppendEntry {
            key: appended_event_context_key(&event.id),
            item: InputItem::TextRef {
                origin: Some("event".to_owned()),
                blob_ref: event
                    .prompt_ref
                    .clone()
                    .unwrap_or_else(|| event.document_ref.clone()),
            },
        })
        .collect();
    for batch in entries.chunks(CONTEXT_APPEND_BATCH) {
        let response = api
            .append_context(ContextAppendParams {
                session_id: request.session_id.clone(),
                entries: batch.to_vec(),
            })
            .await
            .map_err(|error| activity_error("append event context", error))?;
        check_context_append(&response.result)?;
    }
    Ok(())
}

// ── Close and lineage ───────────────────────────────────────────────────────

pub async fn close_session(
    api: &GatewayAgentApi,
    request: BotCloseSessionRequest,
) -> Result<BotCloseSessionResult, ActivityError> {
    let force = request.force;
    // Descendants first: the bot cannot see below its own sessions except
    // through lineage, and a routed session's sub-agents have no other
    // owner once it goes.
    let mut descendants_closed = 0;
    for child in list_descendants(api, &request.session_id).await? {
        if child.lifecycle_status == SessionLifecycleStatus::Closed {
            continue;
        }
        match api
            .close_session(SessionCloseParams {
                session_id: child.id.clone(),
                force,
            })
            .await
        {
            Ok(_) => descendants_closed += 1,
            // Gone already: nothing left to close under this id.
            Err(error) if error.kind == AgentApiErrorKind::NotFound => {}
            // Active work (non-force) or a lost race: leave the tree alone
            // and let the sweep try again later.
            Err(error)
                if matches!(
                    error.kind,
                    AgentApiErrorKind::Rejected | AgentApiErrorKind::Conflict
                ) =>
            {
                return Ok(BotCloseSessionResult {
                    closed: false,
                    descendants_closed,
                });
            }
            Err(error) => return Err(activity_error("close descendant session", error)),
        }
    }
    let closed = match api
        .close_session(SessionCloseParams {
            session_id: request.session_id.clone(),
            force,
        })
        .await
    {
        // An already-closed session closes as a no-op inside the core, so
        // a retried teardown converges here.
        Ok(_) => true,
        // No such session: there is nothing to close, and reporting it open
        // would only make the caller retry a close that can never land.
        Err(error) if error.kind == AgentApiErrorKind::NotFound => true,
        Err(error)
            if matches!(
                error.kind,
                AgentApiErrorKind::Rejected | AgentApiErrorKind::Conflict
            ) =>
        {
            false
        }
        Err(error) => return Err(activity_error("close session", error)),
    };
    Ok(BotCloseSessionResult {
        closed,
        descendants_closed,
    })
}

pub async fn count_descendants(
    api: &GatewayAgentApi,
    request: BotCountDescendantsRequest,
) -> Result<BotCountDescendantsResult, ActivityError> {
    let mut count: u32 = 0;
    for root in &request.session_ids {
        let created = list_descendants(api, root)
            .await?
            .into_iter()
            .filter(|session| created_since(session.created_at_ms, request.since_ms))
            .count();
        count = count.saturating_add(u32::try_from(created).unwrap_or(u32::MAX));
    }
    Ok(BotCountDescendantsResult { count })
}

// ── Pushed tools ────────────────────────────────────────────────────────────

pub async fn read_tool_invocations(
    api: &GatewayAgentApi,
    request: BotReadToolInvocationsRequest,
) -> Result<BotReadToolInvocationsResult, ActivityError> {
    let mut cursor = request.after_seq;
    let mut invocations = Vec::new();
    loop {
        let page = api
            .read_session_events(SessionEventsReadParams {
                direction: Default::default(),
                before: None,
                session_id: request.session_id.clone(),
                after: Some(EventCursor { seq: cursor }),
                limit: Some(EVENT_PAGE_LIMIT),
                wait_ms: None,
            })
            .await
            .map_err(|error| activity_error("read session events", error))?
            .result;
        for event in &page.events {
            cursor = cursor.max(event.cursor.seq);
            invocations.extend(tool_invocation(event));
        }
        if let Some(next) = page.next_cursor {
            cursor = cursor.max(next.seq);
        }
        if page.complete || page.next_cursor.is_none() {
            break;
        }
    }
    Ok(BotReadToolInvocationsResult {
        next_seq: cursor,
        invocations,
    })
}

pub async fn read_json_blob(
    api: &GatewayAgentApi,
    request: BotReadJsonBlobRequest,
) -> Result<serde_json::Value, ActivityError> {
    let bytes = read_blob(api.store().as_ref(), &request.blob_ref).await?;
    serde_json::from_slice(&bytes).map_err(|error| {
        non_retryable(format!(
            "blob {} is not valid JSON: {error}",
            request.blob_ref
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::{
        BoundWorkflowToolDispatchInput, EventJoinsView, RunSummarySourceView, ToolParallelismView,
        WorkflowToolCompletionInput, WorkflowToolDefinitionInput, WorkflowToolKindInput,
        WorkflowToolSpecInput, WorkflowToolTargetInput,
    };

    fn session_view(status: SessionStatus, runs: Vec<api::RunSummaryView>) -> SessionView {
        // Mirror the projector: the executing run is surfaced as the
        // dedicated `active_run` fact, independent of the page window.
        let active_run = runs
            .iter()
            .find(|run| matches!(run.status, RunStatus::Running | RunStatus::Parked))
            .cloned();
        SessionView {
            access: api::ResourceAccessSummary {
                visibility: api::Visibility::Universe,
                created_by: Some(api::Attribution::Local),
            },
            metadata: Default::default(),
            id: "bot:v1:triage".to_owned(),
            display_name: None,
            status,
            managed: true,
            config_revision: 1,
            config: None,
            created_at_ms: 0,
            updated_at_ms: 0,
            closed_at_ms: None,
            retention: api::SessionRetentionView {
                root_session_id: "bot:v1:triage".to_owned(),
                delete_after_close_ms: None,
                delete_at_ms: None,
            },
            runs,
            active_run,
            active_context: Default::default(),
            active_tools: Default::default(),
            active_environment_id: None,
            management: None,
            origin: None,
        }
    }

    fn run_view(id: &str, status: RunStatus) -> api::RunSummaryView {
        api::RunSummaryView {
            id: id.to_owned(),
            status,
            accepted_at_ms: 0,
            started_at_ms: None,
            completed_at_ms: None,
            source: RunSummarySourceView::Input {
                content_ref: None,
                preview: None,
                preview_truncated: false,
            },
            usage: None,
            pending_approvals: Vec::new(),
        }
    }

    fn declaration(name: &str, tool_id: &str) -> WorkflowToolDeclarationInput {
        WorkflowToolDeclarationInput {
            definition: WorkflowToolDefinitionInput {
                tool_id: tool_id.to_owned(),
                revision: 1,
                semantic_type: tool_id.to_owned(),
                tool: WorkflowToolSpecInput {
                    name: name.to_owned(),
                    kind: WorkflowToolKindInput::Function {
                        description_ref: None,
                        input_schema_ref: "sha256:00".to_owned(),
                        output_schema_ref: None,
                        strict: None,
                        provider_options_ref: None,
                    },
                    parallelism: ToolParallelismView::Exclusive,
                },
            },
            target: WorkflowToolTargetInput::Bound {
                receiver: WorkflowEndpointInput {
                    workflow_id: "wf".to_owned(),
                    workflow_kind: "ChannelConversationWorkflow".to_owned(),
                },
                dispatch: BoundWorkflowToolDispatchInput::Push,
            },
            completion: WorkflowToolCompletionInput::Accepted,
        }
    }

    #[test]
    fn status_mapping_is_explicit() {
        assert_eq!(
            session_status_from_view(&session_view(SessionStatus::Idle, Vec::new())),
            BotSessionStatus::Idle
        );
        assert_eq!(
            session_status_from_view(&session_view(SessionStatus::NotLoaded, Vec::new())),
            BotSessionStatus::Idle,
            "an unopened session is idle: the run start opens it"
        );
        assert_eq!(
            session_status_from_view(&session_view(
                SessionStatus::Active,
                vec![
                    run_view("run_1", RunStatus::Completed),
                    run_view("run_2", RunStatus::Running),
                ]
            )),
            BotSessionStatus::Busy {
                run_id: Some("run_2".to_owned())
            }
        );
        assert_eq!(
            session_status_from_view(&session_view(
                SessionStatus::Active,
                vec![run_view("run_3", RunStatus::Cancelling)]
            )),
            BotSessionStatus::Busy { run_id: None },
            "a cancelling run occupies the session but takes no steering"
        );
        assert_eq!(
            session_status_from_view(&session_view(SessionStatus::Error, Vec::new())),
            BotSessionStatus::Busy { run_id: None }
        );
        assert_eq!(
            session_status_from_view(&session_view(SessionStatus::Closed, Vec::new())),
            BotSessionStatus::Closed
        );
    }

    #[test]
    fn profile_unapplicable_is_invalid_or_provider_compatibility() {
        assert!(is_profile_unapplicable(&AgentApiError::invalid_request(
            "bad document"
        )));
        assert!(is_profile_unapplicable(&AgentApiError::rejected(
            "ProviderCompatibility: model x needs anthropic_messages"
        )));
        assert!(!is_profile_unapplicable(&AgentApiError::rejected(
            "ActiveWork: session config can only change while no run is active"
        )));
        assert!(!is_profile_unapplicable(&AgentApiError::rejected(
            "ProviderCompatibilityX: not the kind"
        )));
        assert!(!is_profile_unapplicable(&AgentApiError::conflict(
            "ProviderCompatibility: wrong kind of error"
        )));
    }

    #[test]
    fn declaration_mismatch_is_a_fingerprint_conflict() {
        assert!(is_declaration_mismatch(&AgentApiError::conflict(
            "managed-session controller, receiver, or tool declaration conflicts with durable creation state"
        )));
        assert!(is_declaration_mismatch(&AgentApiError::conflict(
            "creation fingerprint differs"
        )));
        assert!(!is_declaration_mismatch(&AgentApiError::conflict(
            "existing standalone session cannot be reopened as a managed session"
        )));
        assert!(!is_declaration_mismatch(&AgentApiError::rejected(
            "fingerprint"
        )));
    }

    fn is_non_retryable(error: &ActivityError) -> bool {
        matches!(error, ActivityError::Application(failure) if failure.is_non_retryable())
    }

    #[test]
    fn activity_errors_classify_by_kind() {
        assert!(is_non_retryable(&activity_error(
            "x",
            AgentApiError::invalid_request("bad")
        )));
        assert!(is_non_retryable(&activity_error(
            "x",
            AgentApiError::not_found("gone")
        )));
        assert!(is_non_retryable(&activity_error(
            "x",
            AgentApiError::forbidden()
        )));
        assert!(!is_non_retryable(&activity_error(
            "x",
            AgentApiError::rejected("busy")
        )));
        assert!(!is_non_retryable(&activity_error(
            "x",
            AgentApiError::conflict("lost race")
        )));
        assert!(!is_non_retryable(&activity_error(
            "x",
            AgentApiError::internal("store")
        )));
        assert!(is_non_retryable(&non_retryable("malformed")));
        assert!(!is_non_retryable(&retryable("transient")));
    }

    #[test]
    fn carried_declarations_reject_collisions() {
        assert!(validate_carried_declarations(&[]).is_ok());
        assert!(
            validate_carried_declarations(&[
                declaration("message_send", "lightspeed.channels.message.send.v1"),
                declaration("message_edit", "lightspeed.channels.message.edit.v1"),
            ])
            .is_ok()
        );
        let collides_with_bot_tool = validate_carried_declarations(&[declaration(
            "bot_emit",
            "lightspeed.channels.something.v1",
        )])
        .unwrap_err();
        assert!(
            collides_with_bot_tool.contains("bot_emit"),
            "{collides_with_bot_tool}"
        );
        let duplicate = validate_carried_declarations(&[
            declaration("message_send", "a"),
            declaration("message_send", "b"),
        ])
        .unwrap_err();
        assert!(duplicate.contains("message_send"), "{duplicate}");
    }

    #[test]
    fn tool_assets_cover_every_declared_name() {
        let (schemas, descriptions) = tool_asset_bytes().expect("assets");
        assert_eq!(schemas.len(), BOT_TOOL_SCHEMA_NAMES.len());
        assert_eq!(descriptions.len(), BOT_TOOL_DESCRIPTION_NAMES.len());
        for (name, bytes) in &schemas {
            let value: serde_json::Value = serde_json::from_slice(bytes).expect("schema is JSON");
            assert!(value.is_object(), "schema {name} is an object");
        }
        for (name, bytes) in &descriptions {
            assert!(!bytes.is_empty(), "description {name} is not empty");
            assert!(std::str::from_utf8(bytes).is_ok());
        }
        // The refs feed the declaration builder by these names.
        let schema_refs: BTreeMap<&str, String> = schemas
            .iter()
            .map(|(name, _)| (*name, "sha256:00".to_owned()))
            .collect();
        let description_refs: BTreeMap<&str, String> = descriptions
            .iter()
            .map(|(name, _)| (*name, "sha256:01".to_owned()))
            .collect();
        let declarations = bot_workflow_tool_declarations(
            WorkflowEndpointInput {
                workflow_id: "u/bot-triage".to_owned(),
                workflow_kind: "BotControllerWorkflow".to_owned(),
            },
            &schema_refs,
            &description_refs,
            true,
            true,
        )
        .expect("declarations");
        assert_eq!(declarations.len(), BOT_TOOL_NAMES.len());
    }

    #[test]
    fn tool_invocations_come_from_emitted_events_only() {
        let emitted = SessionEventView {
            cursor: EventCursor { seq: 7 },
            session_id: "bot:v1:triage".to_owned(),
            observed_at_ms: 0,
            joins: EventJoinsView::default(),
            kind: SessionEventKindView::WorkflowToolEmitted {
                invocation_id: "inv_1".to_owned(),
                tool_id: "lightspeed.bots.event.resolve.v1".to_owned(),
                semantic_type: "lightspeed.bots.event.resolve.v1".to_owned(),
                schema_revision: 12,
                binding_fingerprint: "fp".to_owned(),
                run_id: "run_4".to_owned(),
                turn_id: "turn_1".to_owned(),
                batch_id: "batch_1".to_owned(),
                call_id: "call_1".to_owned(),
                arguments_ref: "sha256:aa".to_owned(),
                completion_promises: None,
            },
        };
        assert_eq!(
            tool_invocation(&emitted),
            Some(BotToolInvocationRef {
                invocation_id: "inv_1".to_owned(),
                tool_id: "lightspeed.bots.event.resolve.v1".to_owned(),
                run_id: "run_4".to_owned(),
                arguments_ref: "sha256:aa".to_owned(),
            })
        );
        let other = SessionEventView {
            kind: SessionEventKindView::SessionOpened { model: None },
            ..emitted
        };
        assert_eq!(tool_invocation(&other), None);
    }

    #[test]
    fn created_since_window() {
        assert!(created_since(100, 100));
        assert!(created_since(101, 100));
        assert!(!created_since(99, 100));
        assert!(
            created_since(0, -1),
            "a negative window start counts everything"
        );
    }

    #[test]
    fn pinned_kind_comes_from_the_session_config() {
        let mut session = session_view(SessionStatus::Idle, Vec::new());
        assert_eq!(pinned_api_kind(&session), None);
        session.config = Some(api::SessionConfig {
            model: Some(api::ModelConfig {
                provider_id: "anthropic".to_owned(),
                api_kind: "anthropic_messages".to_owned(),
                model: "claude-opus-5".to_owned(),
            }),
            ..Default::default()
        });
        assert_eq!(pinned_api_kind(&session), Some("anthropic_messages"));
    }
}
