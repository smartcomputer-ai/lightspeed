//! Hosted sub-agent delegation service: the activities behind
//! `SubagentExecutionWorkflow`. Prepare validates the pinned grant, reserves
//! the root-scoped tree slot by creating the child session row, creates the
//! child from the pinned profile, and starts its run with a notify intent
//! back to the execution; resolve builds the result envelope and closes the
//! child; close force-closes a cancelled child.

use std::sync::Arc;

use api::{
    AgentApiError, AgentApiService, AgentProfile, InlineAgentProfile, InputItem, ProfileId,
    ProfileReadParams, ProfileSource, SessionCloseParams, SessionReadParams,
};
use async_trait::async_trait;
use engine::{
    BlobRef, PromiseResolution, RunStatus, RunTerminalNotifyIntent, SessionId, SubmissionId,
    media::MediaDescriptor,
    storage::{
        BlobGraphStore, BlobStore, CreateSession, SessionOrigin, SessionOriginKind, SessionStore,
        SessionStoreError,
    },
};
use temporal_workflow::{
    SubagentChildRef, SubagentPrepareActivityResult, SubagentTerminal, WorkflowToolStartArgs,
};
use tools::{
    concurrency::{AwaitArgs, AwaitModeArg},
    subagents::{
        AgentCallArgs, SubagentExecutionContextV1, SubagentResultEnvelope, SubagentResultStatus,
        SubagentToolKind,
    },
};

use crate::gateway::GatewayAgentApi;

const MAX_SUBAGENT_OUTPUT_BYTES: usize = 512 * 1024;

/// The child-side operations the service needs from the hosted API. A
/// trait so the service can be exercised against a counting fake.
#[async_trait]
pub trait SubagentChildRuntime: Send + Sync {
    async fn read_profile(&self, profile_id: ProfileId) -> Result<AgentProfile, AgentApiError>;

    async fn start_session(
        &self,
        session_id: &SessionId,
        profile: ProfileSource,
    ) -> Result<(), AgentApiError>;

    async fn start_run(
        &self,
        session_id: &SessionId,
        input: Vec<InputItem>,
        submission_id: SubmissionId,
        notify_on_terminal: Vec<RunTerminalNotifyIntent>,
    ) -> Result<String, AgentApiError>;

    async fn close_session(&self, session_id: &SessionId, force: bool)
    -> Result<(), AgentApiError>;

    /// The media the child's session holds in its active context: what the
    /// child was shown and may therefore hand up by handle.
    async fn session_media(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<MediaDescriptor>, AgentApiError>;
}

#[derive(Clone)]
pub struct AgentApiSubagentRuntime {
    api: Arc<GatewayAgentApi>,
}

impl AgentApiSubagentRuntime {
    pub fn new(api: Arc<GatewayAgentApi>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl SubagentChildRuntime for AgentApiSubagentRuntime {
    async fn read_profile(&self, profile_id: ProfileId) -> Result<AgentProfile, AgentApiError> {
        Ok(self
            .api
            .read_profile(ProfileReadParams { profile_id })
            .await?
            .result
            .profile)
    }

    async fn start_session(
        &self,
        session_id: &SessionId,
        profile: ProfileSource,
    ) -> Result<(), AgentApiError> {
        self.api
            .start_session_for_subagent(session_id, profile)
            .await
    }

    async fn start_run(
        &self,
        session_id: &SessionId,
        input: Vec<InputItem>,
        submission_id: SubmissionId,
        notify_on_terminal: Vec<RunTerminalNotifyIntent>,
    ) -> Result<String, AgentApiError> {
        self.api
            .start_run_for_subagent(session_id, input, submission_id, notify_on_terminal)
            .await
    }

    async fn close_session(
        &self,
        session_id: &SessionId,
        force: bool,
    ) -> Result<(), AgentApiError> {
        self.api
            .close_session(SessionCloseParams {
                session_id: session_id.as_str().to_owned(),
                force,
            })
            .await
            .map(|_| ())
    }

    async fn session_media(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<MediaDescriptor>, AgentApiError> {
        let session = self
            .api
            .read_session(SessionReadParams {
                session_id: session_id.as_str().to_owned(),
                run_limit: Some(1),
            })
            .await?
            .result
            .session;
        Ok(session
            .active_context
            .entries
            .iter()
            .filter(|entry| entry.content.media_handle.is_some())
            .filter_map(|entry| {
                let content_ref = BlobRef::parse(entry.content.content_ref.clone()).ok()?;
                MediaDescriptor::new(
                    content_ref,
                    entry.content.media_type.as_deref()?,
                    engine::media::media_preview_name(entry.preview.as_deref()).as_deref(),
                )
            })
            .collect())
    }
}

#[derive(Clone)]
pub struct SubagentService {
    sessions: Arc<dyn SessionStore>,
    blobs: Arc<dyn BlobStore>,
    blob_graph: Option<Arc<dyn BlobGraphStore>>,
    runtime: Arc<dyn SubagentChildRuntime>,
}

impl SubagentService {
    pub fn new(
        sessions: Arc<dyn SessionStore>,
        blobs: Arc<dyn BlobStore>,
        runtime: Arc<dyn SubagentChildRuntime>,
    ) -> Self {
        Self {
            sessions,
            blobs,
            blob_graph: None,
            runtime,
        }
    }

    /// Record containment edges from result envelopes to the media they
    /// hand up, so the bytes outlive the child's close until the parent's
    /// context entry roots them.
    pub fn with_blob_graph(mut self, blob_graph: Option<Arc<dyn BlobGraphStore>>) -> Self {
        self.blob_graph = blob_graph;
        self
    }

    /// Step A of the execution. Every expected failure (limit, unlisted
    /// agent, missing profile) is `Rejected` so the parent's `reply`
    /// promise fails cleanly; only infrastructure failures are errors.
    pub async fn prepare(
        &self,
        start: WorkflowToolStartArgs,
        now_ms: u64,
    ) -> Result<SubagentPrepareActivityResult, AgentApiError> {
        let invocation = &start.invocation;
        let expected_holder =
            temporal_workflow::compose_workflow_id(start.universe_id, &invocation.session_id);
        if start.execution_id.is_empty()
            || start.universe_id != invocation.session_universe_id
            || start.holder_workflow_id != expected_holder
        {
            return Err(AgentApiError::invalid_request(
                "subagent workflow-tool start identity is invalid",
            ));
        }
        let Some(reply_promise_id) = invocation
            .completion_promises
            .as_ref()
            .and_then(|promises| promises.get(engine::REPLY_COMPLETION_KEY))
        else {
            return Err(AgentApiError::invalid_request(
                "subagent invocation is missing its reply completion promise",
            ));
        };
        if SubagentToolKind::from_binding(
            invocation.tool_id.as_str(),
            invocation.semantic_type.as_str(),
        )
        .is_none()
        {
            return Err(AgentApiError::invalid_request(format!(
                "unsupported subagent workflow tool {} ({})",
                invocation.tool_id, invocation.semantic_type
            )));
        }
        let args: AgentCallArgs = self.read_json(&invocation.arguments_ref).await?;
        if let Err(error) = args.validate() {
            return self.rejected(error.to_string()).await;
        }
        let context_ref = invocation.execution_context_ref.as_ref().ok_or_else(|| {
            AgentApiError::invalid_request("subagent invocation is missing its execution context")
        })?;
        let context: SubagentExecutionContextV1 = self.read_json(context_ref).await?;
        if context.version != SubagentExecutionContextV1::VERSION {
            return Err(AgentApiError::invalid_request(format!(
                "unsupported subagent execution context version {}",
                context.version
            )));
        }
        if context.agent_profile_id != args.agent {
            return self
                .rejected(format!(
                    "agent {} does not match the admitted agent {}",
                    args.agent, context.agent_profile_id
                ))
                .await;
        }
        let parent_session_id = SessionId::try_new(context.parent_session_id.clone())
            .map_err(|error| AgentApiError::internal(format!("invalid parent id: {error}")))?;
        let parent = self
            .sessions
            .load_session(&parent_session_id)
            .await
            .map_err(api_projection::map_session_store_error)?
            .ok_or_else(|| {
                AgentApiError::not_found(format!("parent session not found: {parent_session_id}"))
            })?;
        let (root_session_id, depth, limits) = match parent.origin.as_ref() {
            Some(origin) => (
                origin.root_session_id.clone(),
                origin.depth.saturating_add(1),
                context.grant_limits.attenuated_by(origin.limits),
            ),
            None => (parent_session_id.clone(), 1, context.grant_limits),
        };
        let profile_id = match ProfileId::try_new(args.agent.clone()) {
            Ok(profile_id) => profile_id,
            Err(error) => {
                return self
                    .rejected(format!(
                        "invalid agent profile id {:?}: {error}",
                        args.agent
                    ))
                    .await;
            }
        };
        let profile = match self.runtime.read_profile(profile_id.clone()).await {
            Ok(profile) => profile,
            Err(error) if is_not_found(&error) => {
                return self
                    .rejected(format!("agent profile does not exist: {profile_id}"))
                    .await;
            }
            Err(error) => return Err(error),
        };
        let child_session_id = child_session_id(&start.execution_id);
        let display_name = args.label.clone().or_else(|| {
            profile
                .display_name
                .clone()
                .or_else(|| Some(profile_id.as_str().to_owned()))
        });
        let origin = SessionOrigin {
            kind: SessionOriginKind::Subagent,
            parent_session_id: parent_session_id.clone(),
            parent_run_id: context.parent_run_id,
            root_session_id,
            depth,
            invocation_id: invocation.invocation_id.as_str().to_owned(),
            profile_id: profile_id.as_str().to_owned(),
            profile_revision: profile.revision,
            limits,
        };
        match self
            .sessions
            .create_session(CreateSession {
                session_id: child_session_id.clone(),
                display_name,
                // Copied once at spawn so a filter on the parent's campaign
                // catches its descendants; later puts do not propagate.
                metadata: parent.metadata.clone(),
                origin: Some(origin.clone()),
                delete_after_close_ms: None,
                created_at_ms: now_ms,
            })
            .await
        {
            Ok(_) => {}
            Err(SessionStoreError::SessionAlreadyExists { .. }) => {
                // Retry of this execution: the row is ours iff it names this
                // invocation; anything else is an identity collision.
                let existing = self
                    .sessions
                    .load_session(&child_session_id)
                    .await
                    .map_err(api_projection::map_session_store_error)?;
                let ours = existing.as_ref().is_some_and(|record| {
                    record
                        .origin
                        .as_ref()
                        .is_some_and(|existing| existing.invocation_id == origin.invocation_id)
                });
                if !ours {
                    return Err(AgentApiError::conflict(format!(
                        "subagent child session id collides with an unrelated session: {child_session_id}"
                    )));
                }
            }
            Err(error @ SessionStoreError::OriginLimitExceeded { .. }) => {
                return self.rejected(error.to_string()).await;
            }
            Err(error) => return Err(api_projection::map_session_store_error(error)),
        }
        // The profile is applied inline so the child runs the revision that
        // was pinned on its origin, not whatever the registry holds later.
        // A profile that cannot be applied (an `inherit` without a parent
        // environment, a missing binding, ...) is a rejected delegation the
        // parent must see, not an activity retry: a retried start finds the
        // child workflow already running and would skip the profile.
        if let Err(error) = self
            .runtime
            .start_session(
                &child_session_id,
                ProfileSource::Inline {
                    profile: Box::new(InlineAgentProfile {
                        display_name: profile.display_name.clone(),
                        description: profile.description.clone(),
                        document: profile.document.clone(),
                    }),
                },
            )
            .await
        {
            if is_caller_error(&error) {
                let _ = self.close(child_session_id.as_str()).await;
                return self
                    .rejected(format!("agent {profile_id} could not be started: {error}"))
                    .await;
            }
            return Err(error);
        }
        let submission_id = SubmissionId::new(format!(
            "subagent_run_{}",
            digest_suffix(&start.execution_id)
        ));
        let run_id = self
            .runtime
            .start_run(
                &child_session_id,
                vec![InputItem::Text {
                    origin: None,
                    text: args.input,
                }],
                submission_id,
                vec![RunTerminalNotifyIntent {
                    holder_workflow_id: start.execution_id.clone(),
                    token: reply_promise_id.as_str().to_owned(),
                }],
            )
            .await?;
        let run_id = parse_api_run_id(&run_id)?;
        Ok(SubagentPrepareActivityResult::Prepared {
            child: SubagentChildRef {
                session_id: child_session_id.as_str().to_owned(),
                run_id,
                agent_profile_id: profile_id.as_str().to_owned(),
            },
            deadline_ms: limits.deadline_ms,
        })
    }

    /// The media `text` links by handle that the child session actually
    /// holds, in first-link order, capped like a tool result's media.
    async fn linked_media(
        &self,
        session_id: &str,
        text: &str,
    ) -> Result<Vec<MediaDescriptor>, AgentApiError> {
        let handles = engine::media::find_media_handles(text);
        if handles.is_empty() {
            return Ok(Vec::new());
        }
        let session_id = SessionId::try_new(session_id.to_owned())
            .map_err(|error| AgentApiError::internal(error.to_string()))?;
        let available = self.runtime.session_media(&session_id).await?;
        Ok(handles
            .iter()
            .filter_map(|handle| available.iter().find(|item| &item.handle == handle))
            .take(engine::media::MAX_TOOL_MEDIA_ITEMS)
            .cloned()
            .collect())
    }

    /// Step C: the envelope the parent sees, then the child is closed.
    pub async fn resolve(
        &self,
        child: SubagentChildRef,
        terminal: SubagentTerminal,
    ) -> Result<PromiseResolution, AgentApiError> {
        let (status, output, error, media) = match terminal {
            SubagentTerminal::Run {
                status,
                output,
                failure_message_ref,
            } => {
                let output = match output.as_ref() {
                    Some(output) => {
                        api_projection::project_content_text(self.blobs.as_ref(), output)
                            .await?
                            .map(|text| truncate_utf8(text, MAX_SUBAGENT_OUTPUT_BYTES))
                    }
                    None => None,
                };
                // Media the child linked by handle in its answer is what it
                // hands up: resolve each handle against what the child saw,
                // in link order. Unknown handles stay plain text.
                let media = match output.as_deref() {
                    Some(text) if status == RunStatus::Completed => {
                        self.linked_media(&child.session_id, text).await?
                    }
                    _ => Vec::new(),
                };
                let failure = match failure_message_ref.as_ref() {
                    Some(failure_ref) => Some(self.read_text(failure_ref).await?),
                    None => None,
                };
                match status {
                    RunStatus::Completed => (SubagentResultStatus::Completed, output, None, media),
                    RunStatus::Cancelled => (
                        SubagentResultStatus::Cancelled,
                        output,
                        Some("sub-agent run was cancelled".to_owned()),
                        Vec::new(),
                    ),
                    RunStatus::Failed
                    | RunStatus::Active
                    | RunStatus::Parked
                    | RunStatus::Cancelling => (
                        SubagentResultStatus::Failed,
                        output,
                        Some(failure.unwrap_or_else(|| "sub-agent run failed".to_owned())),
                        Vec::new(),
                    ),
                }
            }
            SubagentTerminal::Deadline => (
                SubagentResultStatus::Deadline,
                None,
                Some("sub-agent run exceeded the grant deadline".to_owned()),
                Vec::new(),
            ),
        };
        let envelope = SubagentResultEnvelope {
            agent: child.agent_profile_id.clone(),
            session_id: child.session_id.clone(),
            run_id: Some(format!("run_{}", child.run_id)),
            status,
            output,
            error,
            media,
        };
        let payload_ref = self
            .blobs
            .put_bytes(serde_json::to_vec(&envelope).map_err(|error| {
                AgentApiError::internal(format!("encode subagent result: {error}"))
            })?)
            .await
            .map_err(|error| AgentApiError::internal(error.to_string()))?;
        engine::storage::record_contains_edges(
            self.blob_graph.as_deref(),
            &payload_ref,
            envelope.media.iter().map(|item| item.content_ref.clone()),
        )
        .await
        .map_err(|error| AgentApiError::internal(error.to_string()))?;
        self.close(&child.session_id).await?;
        Ok(match status {
            SubagentResultStatus::Completed => PromiseResolution::Resolved {
                payload_ref: Some(payload_ref),
            },
            SubagentResultStatus::Failed
            | SubagentResultStatus::Cancelled
            | SubagentResultStatus::Deadline => PromiseResolution::Failed {
                error_ref: Some(payload_ref),
            },
        })
    }

    /// Force-close the child; an already-closed or missing child is fine.
    pub async fn close(&self, session_id: &str) -> Result<(), AgentApiError> {
        let session_id = SessionId::try_new(session_id.to_owned())
            .map_err(|error| AgentApiError::internal(format!("invalid child id: {error}")))?;
        match self.runtime.close_session(&session_id, true).await {
            Ok(()) => Ok(()),
            Err(error) if is_not_found(&error) || is_already_closed(&error) => Ok(()),
            Err(error) => Err(error),
        }
    }

    async fn rejected(
        &self,
        message: String,
    ) -> Result<SubagentPrepareActivityResult, AgentApiError> {
        let error_ref = self
            .blobs
            .put_bytes(
                serde_json::to_vec(&serde_json::json!({ "error": message }))
                    .map_err(|error| AgentApiError::internal(error.to_string()))?,
            )
            .await
            .map_err(|error| AgentApiError::internal(error.to_string()))?;
        Ok(SubagentPrepareActivityResult::Rejected { error_ref })
    }

    async fn read_json<T: serde::de::DeserializeOwned>(
        &self,
        blob_ref: &BlobRef,
    ) -> Result<T, AgentApiError> {
        let bytes = self
            .blobs
            .read_bytes(blob_ref)
            .await
            .map_err(|error| AgentApiError::internal(error.to_string()))?;
        serde_json::from_slice(&bytes)
            .map_err(|error| AgentApiError::invalid_request(format!("invalid JSON blob: {error}")))
    }

    /// Failure payloads may carry a JSON string or raw diagnostic text. Run
    /// output uses the content descriptor projection instead.
    async fn read_text(&self, blob_ref: &BlobRef) -> Result<String, AgentApiError> {
        let bytes = self
            .blobs
            .read_bytes(blob_ref)
            .await
            .map_err(|error| AgentApiError::internal(error.to_string()))?;
        let text = match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(serde_json::Value::String(text)) => text,
            Ok(value) => value.to_string(),
            Err(_) => String::from_utf8_lossy(&bytes).into_owned(),
        };
        Ok(truncate_utf8(text, MAX_SUBAGENT_OUTPUT_BYTES))
    }
}

/// Generic `await` argument evaluation shared by the session tool runtime.
pub fn await_spec_from_args(
    args: AwaitArgs,
    now_ms: u64,
) -> Result<engine::AwaitSpec, AgentApiError> {
    let promise_ids = args
        .validated_promise_ids()
        .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
    Ok(engine::AwaitSpec {
        promise_ids,
        mode: match args.mode {
            AwaitModeArg::All => engine::AwaitMode::All,
            AwaitModeArg::Any => engine::AwaitMode::Any,
        },
        deadline_at_ms: args
            .timeout_ms
            .map(|timeout| now_ms.saturating_add(timeout)),
    })
}

pub fn child_session_id(execution_id: &str) -> SessionId {
    SessionId::new(format!("agent_{}", digest_suffix(execution_id)))
}

fn digest_suffix(value: &str) -> String {
    let digest = BlobRef::from_bytes(value.as_bytes());
    digest
        .as_str()
        .strip_prefix("sha256:")
        .unwrap_or(digest.as_str())
        .chars()
        .take(32)
        .collect()
}

fn parse_api_run_id(run_id: &str) -> Result<u64, AgentApiError> {
    run_id
        .strip_prefix("run_")
        .and_then(|rest| rest.parse::<u64>().ok())
        .ok_or_else(|| {
            AgentApiError::internal(format!(
                "subagent runtime returned malformed run id: {run_id}"
            ))
        })
}

fn truncate_utf8(mut text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    let mut cut = max_bytes;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    text.push_str("\n…[truncated]");
    text
}

fn is_not_found(error: &AgentApiError) -> bool {
    matches!(error.kind, api::AgentApiErrorKind::NotFound)
}

/// Errors that describe the request rather than the runtime: surfaced to
/// the parent as a rejected delegation instead of retried.
fn is_caller_error(error: &AgentApiError) -> bool {
    matches!(
        error.kind,
        api::AgentApiErrorKind::NotFound
            | api::AgentApiErrorKind::InvalidRequest
            | api::AgentApiErrorKind::Rejected
            | api::AgentApiErrorKind::Conflict
    )
}

fn is_already_closed(error: &AgentApiError) -> bool {
    matches!(error.kind, api::AgentApiErrorKind::Rejected) && error.to_string().contains("closed")
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
    };

    use api::{AgentApiErrorKind, ProfileDocument};
    use engine::{
        PromiseId, RunId, SessionId, SubagentLimits, ToolBatchId, ToolCallId, TurnId,
        WorkflowToolId, WorkflowToolInvocation, WorkflowToolInvocationId,
        storage::{
            AppendSessionEvents, InMemoryBlobStore, InMemorySessionStore, SessionLifecycleStatus,
            SessionOrigin, SessionOriginKind, SessionStore,
        },
    };
    use temporal_workflow::{SubagentChildRef, SubagentTerminal};
    use tools::subagents::{
        AGENT_RUN_WORKFLOW_SEMANTIC_TYPE, AGENT_RUN_WORKFLOW_TOOL_ID, SubagentResultEnvelope,
        SubagentResultStatus,
    };

    use super::*;

    const UNIVERSE: uuid::Uuid = uuid::Uuid::from_u128(11);

    type StartedRun = (
        String,
        Vec<InputItem>,
        SubmissionId,
        Vec<RunTerminalNotifyIntent>,
    );

    /// Counting fake of the hosted child API.
    #[derive(Default)]
    struct FakeChildRuntime {
        profiles: Mutex<BTreeMap<String, AgentProfile>>,
        start_session_error: Mutex<Option<AgentApiError>>,
        close_error: Mutex<Option<AgentApiError>>,
        started_sessions: Mutex<Vec<(String, ProfileSource)>>,
        started_runs: Mutex<Vec<StartedRun>>,
        closed: Mutex<Vec<(String, bool)>>,
        media: Mutex<Vec<MediaDescriptor>>,
    }

    impl FakeChildRuntime {
        fn with_profile(profile_id: &str, revision: u64) -> Arc<Self> {
            let runtime = Self::default();
            runtime.profiles.lock().unwrap().insert(
                profile_id.to_owned(),
                AgentProfile {
                    profile_id: ProfileId::new(profile_id),
                    display_name: Some("Reviewer".to_owned()),
                    description: Some("Reviews things".to_owned()),
                    revision,
                    document: ProfileDocument {
                        metadata: Default::default(),
                        retention: None,
                        config: None,
                        instructions: None,
                        environment: None,
                    },
                    created_at_ms: 1,
                    updated_at_ms: 1,
                },
            );
            Arc::new(runtime)
        }
    }

    #[async_trait]
    impl SubagentChildRuntime for FakeChildRuntime {
        async fn read_profile(&self, profile_id: ProfileId) -> Result<AgentProfile, AgentApiError> {
            self.profiles
                .lock()
                .unwrap()
                .get(profile_id.as_str())
                .cloned()
                .ok_or_else(|| AgentApiError::not_found(format!("profile {profile_id}")))
        }

        async fn start_session(
            &self,
            session_id: &SessionId,
            profile: ProfileSource,
        ) -> Result<(), AgentApiError> {
            if let Some(error) = self.start_session_error.lock().unwrap().clone() {
                return Err(error);
            }
            self.started_sessions
                .lock()
                .unwrap()
                .push((session_id.as_str().to_owned(), profile));
            Ok(())
        }

        async fn start_run(
            &self,
            session_id: &SessionId,
            input: Vec<InputItem>,
            submission_id: SubmissionId,
            notify_on_terminal: Vec<RunTerminalNotifyIntent>,
        ) -> Result<String, AgentApiError> {
            self.started_runs.lock().unwrap().push((
                session_id.as_str().to_owned(),
                input,
                submission_id,
                notify_on_terminal,
            ));
            Ok("run_1".to_owned())
        }

        async fn close_session(
            &self,
            session_id: &SessionId,
            force: bool,
        ) -> Result<(), AgentApiError> {
            self.closed
                .lock()
                .unwrap()
                .push((session_id.as_str().to_owned(), force));
            match self.close_error.lock().unwrap().clone() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }

        async fn session_media(
            &self,
            _session_id: &SessionId,
        ) -> Result<Vec<MediaDescriptor>, AgentApiError> {
            Ok(self.media.lock().unwrap().clone())
        }
    }

    struct Harness {
        service: SubagentService,
        sessions: Arc<InMemorySessionStore>,
        blobs: Arc<InMemoryBlobStore>,
        runtime: Arc<FakeChildRuntime>,
    }

    async fn harness(runtime: Arc<FakeChildRuntime>) -> Harness {
        let sessions = Arc::new(InMemorySessionStore::new());
        let blobs = Arc::new(InMemoryBlobStore::new());
        sessions
            .create_session(CreateSession {
                metadata: Default::default(),
                session_id: SessionId::new("parent"),
                display_name: None,
                origin: None,
                delete_after_close_ms: None,
                created_at_ms: 1,
            })
            .await
            .expect("create parent");
        let service = SubagentService::new(
            sessions.clone(),
            blobs.clone(),
            runtime.clone() as Arc<dyn SubagentChildRuntime>,
        );
        Harness {
            service,
            sessions,
            blobs,
            runtime,
        }
    }

    fn reply_promise() -> PromiseId {
        PromiseId::new("promise_7")
    }

    /// A start for `agent_run` of `agent` by `parent`, admitted for
    /// `admitted_agent` with `limits`.
    async fn start_args(
        blobs: &InMemoryBlobStore,
        execution_id: &str,
        parent: &str,
        agent: &str,
        admitted_agent: &str,
        limits: SubagentLimits,
    ) -> WorkflowToolStartArgs {
        let arguments_ref = blobs
            .put_bytes(
                serde_json::to_vec(&AgentCallArgs {
                    agent: agent.to_owned(),
                    input: "review the change".to_owned(),
                    label: Some("reviewer: change".to_owned()),
                })
                .unwrap(),
            )
            .await
            .expect("put arguments");
        let context_ref = blobs
            .put_bytes(
                serde_json::to_vec(&SubagentExecutionContextV1::new(
                    parent.to_owned(),
                    3,
                    admitted_agent.to_owned(),
                    limits,
                ))
                .unwrap(),
            )
            .await
            .expect("put context");
        let parent_id = SessionId::new(parent);
        WorkflowToolStartArgs {
            universe_id: UNIVERSE,
            holder_workflow_id: temporal_workflow::compose_workflow_id(UNIVERSE, &parent_id),
            execution_id: execution_id.to_owned(),
            invocation: WorkflowToolInvocation {
                invocation_id: WorkflowToolInvocationId::new(format!(
                    "wti:sha256:{}",
                    execution_id
                        .chars()
                        .last()
                        .unwrap_or('a')
                        .to_string()
                        .repeat(64)
                )),
                tool_id: WorkflowToolId::new(AGENT_RUN_WORKFLOW_TOOL_ID),
                semantic_type: AGENT_RUN_WORKFLOW_SEMANTIC_TYPE.to_owned(),
                schema_revision: 1,
                binding_fingerprint: "binding:v1:test".to_owned(),
                session_universe_id: UNIVERSE,
                session_id: parent_id,
                run_id: RunId::new(3),
                turn_id: TurnId::new(1),
                tool_batch_id: ToolBatchId::new(1),
                tool_call_id: ToolCallId::new("call_agent"),
                arguments_ref,
                execution_context_ref: Some(context_ref),
                completion_promises: Some(BTreeMap::from([(
                    engine::REPLY_COMPLETION_KEY.to_owned(),
                    reply_promise(),
                )])),
            },
        }
    }

    fn prepared(result: SubagentPrepareActivityResult) -> (SubagentChildRef, u64) {
        match result {
            SubagentPrepareActivityResult::Prepared { child, deadline_ms } => (child, deadline_ms),
            SubagentPrepareActivityResult::Rejected { .. } => panic!("expected a prepared child"),
        }
    }

    async fn rejection_message(
        blobs: &InMemoryBlobStore,
        result: SubagentPrepareActivityResult,
    ) -> String {
        match result {
            SubagentPrepareActivityResult::Rejected { error_ref } => {
                let value: serde_json::Value =
                    serde_json::from_slice(&blobs.read_bytes(&error_ref).await.expect("read"))
                        .expect("decode rejection");
                value["error"].as_str().expect("error text").to_owned()
            }
            SubagentPrepareActivityResult::Prepared { .. } => panic!("expected a rejection"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prepare_creates_the_child_with_its_origin_and_starts_its_run() {
        let runtime = FakeChildRuntime::with_profile("reviewer", 4);
        let h = harness(runtime).await;
        let limits = SubagentLimits {
            deadline_ms: 30_000,
            ..SubagentLimits::default()
        };
        let start = start_args(&h.blobs, "wte:a", "parent", "reviewer", "reviewer", limits).await;

        let (child, deadline_ms) = prepared(
            h.service
                .prepare(start.clone(), 100)
                .await
                .expect("prepare"),
        );
        assert_eq!(child.session_id, child_session_id("wte:a").as_str());
        assert_eq!(child.run_id, 1);
        assert_eq!(child.agent_profile_id, "reviewer");
        assert_eq!(deadline_ms, 30_000);

        let record = h
            .sessions
            .load_session(&SessionId::new(&child.session_id))
            .await
            .expect("load")
            .expect("child row reserved");
        assert_eq!(record.display_name.as_deref(), Some("reviewer: change"));
        let origin = record.origin.expect("origin");
        assert_eq!(origin.kind, SessionOriginKind::Subagent);
        assert_eq!(origin.parent_session_id.as_str(), "parent");
        assert_eq!(origin.root_session_id.as_str(), "parent");
        assert_eq!(origin.parent_run_id, 3);
        assert_eq!(origin.depth, 1);
        assert_eq!(origin.profile_id, "reviewer");
        assert_eq!(origin.profile_revision, 4);
        assert_eq!(origin.limits, limits);
        assert_eq!(
            origin.invocation_id,
            start.invocation.invocation_id.as_str()
        );

        let started = h.runtime.started_sessions.lock().unwrap().clone();
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].0, child.session_id);
        assert!(matches!(started[0].1, ProfileSource::Inline { .. }));
        let runs = h.runtime.started_runs.lock().unwrap().clone();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].0, child.session_id);
        assert_eq!(
            runs[0].1,
            vec![InputItem::Text {
                origin: None,
                text: "review the change".to_owned()
            }]
        );
        assert_eq!(
            runs[0].3,
            vec![RunTerminalNotifyIntent {
                holder_workflow_id: "wte:a".to_owned(),
                token: reply_promise().as_str().to_owned(),
            }],
            "the child's terminal must be addressed to the execution with the reply promise as token"
        );
        assert!(h.runtime.closed.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prepare_retry_reuses_the_reserved_child() {
        let runtime = FakeChildRuntime::with_profile("reviewer", 1);
        let h = harness(runtime).await;
        let start = start_args(
            &h.blobs,
            "wte:a",
            "parent",
            "reviewer",
            "reviewer",
            SubagentLimits::default(),
        )
        .await;

        let first = h
            .service
            .prepare(start.clone(), 100)
            .await
            .expect("first prepare");
        let second = h
            .service
            .prepare(start, 200)
            .await
            .expect("retried prepare");
        assert_eq!(first, second);
        let listed = h
            .sessions
            .list_sessions(engine::storage::ListSessions {
                metadata: Default::default(),
                cursor: None,
                limit: 10,
                root_session_id: Some(SessionId::new("parent")),
                parent_session_id: None,
                exclude_closed: false,
            })
            .await
            .expect("list")
            .sessions;
        assert_eq!(listed.len(), 1, "a retry must not reserve a second slot");
        assert_eq!(
            h.runtime.started_runs.lock().unwrap().len(),
            2,
            "the run start is re-issued with the same submission id"
        );
        let runs = h.runtime.started_runs.lock().unwrap().clone();
        assert_eq!(runs[0].2, runs[1].2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prepare_rejects_an_agent_other_than_the_admitted_one() {
        let runtime = FakeChildRuntime::with_profile("reviewer", 1);
        let h = harness(runtime).await;
        let start = start_args(
            &h.blobs,
            "wte:a",
            "parent",
            "planner",
            "reviewer",
            SubagentLimits::default(),
        )
        .await;

        let result = h.service.prepare(start, 100).await.expect("prepare");
        let message = rejection_message(&h.blobs, result).await;
        assert!(
            message.contains("does not match the admitted agent"),
            "{message}"
        );
        assert!(h.runtime.started_sessions.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prepare_rejects_a_missing_profile_without_reserving() {
        let runtime = Arc::new(FakeChildRuntime::default());
        let h = harness(runtime).await;
        let start = start_args(
            &h.blobs,
            "wte:a",
            "parent",
            "reviewer",
            "reviewer",
            SubagentLimits::default(),
        )
        .await;

        let result = h.service.prepare(start, 100).await.expect("prepare");
        let message = rejection_message(&h.blobs, result).await;
        assert!(
            message.contains("agent profile does not exist"),
            "{message}"
        );
        assert!(
            h.sessions
                .load_session(&child_session_id("wte:a"))
                .await
                .expect("load")
                .is_none(),
            "no child row without a profile"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prepare_rejects_when_the_root_limit_is_exceeded() {
        let runtime = FakeChildRuntime::with_profile("reviewer", 1);
        let h = harness(runtime).await;
        let limits = SubagentLimits {
            max_descendants: 1,
            ..SubagentLimits::default()
        };
        let first = start_args(&h.blobs, "wte:a", "parent", "reviewer", "reviewer", limits).await;
        let second = start_args(&h.blobs, "wte:b", "parent", "reviewer", "reviewer", limits).await;

        assert!(matches!(
            h.service.prepare(first, 100).await.expect("first prepare"),
            SubagentPrepareActivityResult::Prepared { .. }
        ));
        let result = h
            .service
            .prepare(second, 101)
            .await
            .expect("second prepare");
        let message = rejection_message(&h.blobs, result).await;
        assert!(message.contains("maxDescendants"), "{message}");
        assert_eq!(h.runtime.started_sessions.lock().unwrap().len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prepare_attenuates_the_grant_by_the_parents_origin() {
        let runtime = FakeChildRuntime::with_profile("reviewer", 1);
        let h = harness(runtime).await;
        // `mid` is itself a depth-1 child of `root` with tighter limits; it
        // counts as one open descendant of `root` itself.
        let parent_limits = SubagentLimits {
            max_depth: 3,
            max_descendants: 2,
            max_concurrent: 2,
            deadline_ms: 5_000,
        };
        h.sessions
            .create_session(CreateSession {
                metadata: Default::default(),
                session_id: SessionId::new("root"),
                display_name: None,
                origin: None,
                delete_after_close_ms: None,
                created_at_ms: 1,
            })
            .await
            .expect("create root");
        h.sessions
            .create_session(CreateSession {
                metadata: Default::default(),
                session_id: SessionId::new("mid"),
                display_name: None,
                origin: Some(SessionOrigin {
                    kind: SessionOriginKind::Subagent,
                    parent_session_id: SessionId::new("root"),
                    parent_run_id: 1,
                    root_session_id: SessionId::new("root"),
                    depth: 1,
                    invocation_id: format!("wti:sha256:{}", "c".repeat(64)),
                    profile_id: "planner".to_owned(),
                    profile_revision: 1,
                    limits: parent_limits,
                }),
                delete_after_close_ms: None,
                created_at_ms: 2,
            })
            .await
            .expect("create mid");
        let start = start_args(
            &h.blobs,
            "wte:a",
            "mid",
            "reviewer",
            "reviewer",
            SubagentLimits::default(),
        )
        .await;

        let (child, deadline_ms) = prepared(h.service.prepare(start, 100).await.expect("prepare"));
        assert_eq!(
            deadline_ms, 5_000,
            "deadline attenuated by the parent's origin"
        );
        let origin = h
            .sessions
            .load_session(&SessionId::new(&child.session_id))
            .await
            .expect("load")
            .expect("child")
            .origin
            .expect("origin");
        assert_eq!(origin.root_session_id.as_str(), "root");
        assert_eq!(origin.parent_session_id.as_str(), "mid");
        assert_eq!(origin.depth, 2);
        assert_eq!(
            origin.limits,
            SubagentLimits {
                max_depth: 2,
                max_descendants: 2,
                max_concurrent: 2,
                deadline_ms: 5_000,
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prepare_turns_a_caller_error_on_child_start_into_a_rejection_and_closes_the_child() {
        let runtime = FakeChildRuntime::with_profile("reviewer", 1);
        *runtime.start_session_error.lock().unwrap() = Some(AgentApiError::rejected(
            "inherit requires a parent environment",
        ));
        let h = harness(runtime).await;
        let start = start_args(
            &h.blobs,
            "wte:a",
            "parent",
            "reviewer",
            "reviewer",
            SubagentLimits::default(),
        )
        .await;

        let result = h.service.prepare(start, 100).await.expect("prepare");
        let message = rejection_message(&h.blobs, result).await;
        assert!(message.contains("could not be started"), "{message}");
        assert!(
            message.contains("inherit requires a parent environment"),
            "{message}"
        );
        assert_eq!(
            h.runtime.closed.lock().unwrap().clone(),
            vec![(child_session_id("wte:a").as_str().to_owned(), true)],
            "the half-made child is force-closed"
        );
        assert!(h.runtime.started_runs.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prepare_surfaces_an_infrastructure_error_on_child_start() {
        let runtime = FakeChildRuntime::with_profile("reviewer", 1);
        *runtime.start_session_error.lock().unwrap() =
            Some(AgentApiError::internal("temporal unavailable"));
        let h = harness(runtime).await;
        let start = start_args(
            &h.blobs,
            "wte:a",
            "parent",
            "reviewer",
            "reviewer",
            SubagentLimits::default(),
        )
        .await;

        let error = h
            .service
            .prepare(start, 100)
            .await
            .expect_err("infrastructure error");
        assert_eq!(error.kind, AgentApiErrorKind::Internal);
        assert!(
            h.runtime.closed.lock().unwrap().is_empty(),
            "a retryable error keeps the child for the retry"
        );
    }

    async fn envelope_of(
        blobs: &InMemoryBlobStore,
        resolution: &PromiseResolution,
    ) -> SubagentResultEnvelope {
        let payload_ref = match resolution {
            PromiseResolution::Resolved { payload_ref } => payload_ref.clone(),
            PromiseResolution::Failed { error_ref } => error_ref.clone(),
            other => panic!("unexpected resolution {other:?}"),
        }
        .expect("payload ref");
        serde_json::from_slice(&blobs.read_bytes(&payload_ref).await.expect("read"))
            .expect("decode envelope")
    }

    fn child_ref() -> SubagentChildRef {
        SubagentChildRef {
            session_id: "agent_child".to_owned(),
            run_id: 5,
            agent_profile_id: "reviewer".to_owned(),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolve_maps_a_completed_run_to_a_resolved_envelope_and_closes_the_child() {
        let runtime = Arc::new(FakeChildRuntime::default());
        let h = harness(runtime).await;
        let output = h
            .blobs
            .put_bytes(b"looks good".to_vec())
            .await
            .expect("put output");

        let resolution = h
            .service
            .resolve(
                child_ref(),
                SubagentTerminal::Run {
                    status: RunStatus::Completed,
                    output: Some(engine::ContentRef::text(output)),
                    failure_message_ref: None,
                },
            )
            .await
            .expect("resolve");
        assert!(matches!(resolution, PromiseResolution::Resolved { .. }));
        let envelope = envelope_of(&h.blobs, &resolution).await;
        assert_eq!(envelope.status, SubagentResultStatus::Completed);
        assert_eq!(envelope.agent, "reviewer");
        assert_eq!(envelope.session_id, "agent_child");
        assert_eq!(envelope.run_id.as_deref(), Some("run_5"));
        assert_eq!(envelope.output.as_deref(), Some("looks good"));
        assert_eq!(envelope.error, None);
        assert_eq!(
            h.runtime.closed.lock().unwrap().clone(),
            vec![("agent_child".to_owned(), true)]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolve_hands_up_the_media_the_child_linked_by_handle_in_link_order() {
        let runtime = Arc::new(FakeChildRuntime::default());
        let png = MediaDescriptor::new(
            BlobRef::from_bytes(b"png bytes"),
            "image/png",
            Some("render.png"),
        )
        .expect("png");
        let pdf = MediaDescriptor::new(BlobRef::from_bytes(b"%PDF"), "application/pdf", None)
            .expect("pdf");
        let unseen = MediaDescriptor::new(BlobRef::from_bytes(b"other"), "image/jpeg", None)
            .expect("unseen");
        *runtime.media.lock().unwrap() = vec![png.clone(), pdf.clone()];
        let h = harness(runtime).await;
        let text = format!(
            "The brief is [here]({}), the render ![r]({}) and again {}; not ours: {}",
            pdf.handle, png.handle, png.handle, unseen.handle
        );
        let output = h.blobs.put_bytes(text.into_bytes()).await.expect("output");

        let resolution = h
            .service
            .resolve(
                child_ref(),
                SubagentTerminal::Run {
                    status: RunStatus::Completed,
                    output: Some(engine::ContentRef::text(output.clone())),
                    failure_message_ref: None,
                },
            )
            .await
            .expect("resolve");
        let envelope = envelope_of(&h.blobs, &resolution).await;
        assert_eq!(envelope.media, vec![pdf.clone(), png.clone()]);

        // A failed run hands nothing up even when its text links media.
        let failed = h
            .service
            .resolve(
                child_ref(),
                SubagentTerminal::Run {
                    status: RunStatus::Failed,
                    output: Some(engine::ContentRef::text(output)),
                    failure_message_ref: None,
                },
            )
            .await
            .expect("resolve failed");
        let envelope = envelope_of(&h.blobs, &failed).await;
        assert!(envelope.media.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn native_child_output_projects_text_without_exposing_provider_metadata() {
        let h = harness(Arc::new(FakeChildRuntime::default())).await;
        let bytes = serde_json::to_vec(&serde_json::json!([
            {"type":"text", "text":"Answer: "},
            {"type":"text", "text":"42", "citations":[{"encrypted_index":"provider-only"}]}
        ]))
        .unwrap();
        let content_ref = h.blobs.put_bytes(bytes).await.unwrap();
        let resolution = h
            .service
            .resolve(
                child_ref(),
                SubagentTerminal::Run {
                    status: RunStatus::Completed,
                    output: Some(engine::ContentRef {
                        content_ref,
                        media_type: Some("application/json".into()),
                        provider_kind: Some(
                            engine::ANTHROPIC_MESSAGES_TEXT_BLOCKS_PROVIDER_KIND.into(),
                        ),
                    }),
                    failure_message_ref: None,
                },
            )
            .await
            .unwrap();
        let envelope = envelope_of(&h.blobs, &resolution).await;
        assert_eq!(envelope.output.as_deref(), Some("Answer: 42"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolve_maps_failed_cancelled_and_deadline_terminals_to_failed_envelopes() {
        let runtime = Arc::new(FakeChildRuntime::default());
        let h = harness(runtime).await;
        let failure_ref = h
            .blobs
            .put_bytes(b"provider exploded".to_vec())
            .await
            .expect("put failure");

        let failed = h
            .service
            .resolve(
                child_ref(),
                SubagentTerminal::Run {
                    status: RunStatus::Failed,
                    output: None,
                    failure_message_ref: Some(failure_ref),
                },
            )
            .await
            .expect("resolve failed");
        assert!(matches!(failed, PromiseResolution::Failed { .. }));
        let envelope = envelope_of(&h.blobs, &failed).await;
        assert_eq!(envelope.status, SubagentResultStatus::Failed);
        assert_eq!(envelope.error.as_deref(), Some("provider exploded"));

        let cancelled = h
            .service
            .resolve(
                child_ref(),
                SubagentTerminal::Run {
                    status: RunStatus::Cancelled,
                    output: None,
                    failure_message_ref: None,
                },
            )
            .await
            .expect("resolve cancelled");
        assert!(matches!(cancelled, PromiseResolution::Failed { .. }));
        assert_eq!(
            envelope_of(&h.blobs, &cancelled).await.status,
            SubagentResultStatus::Cancelled
        );

        let deadline = h
            .service
            .resolve(child_ref(), SubagentTerminal::Deadline)
            .await
            .expect("resolve deadline");
        assert!(matches!(deadline, PromiseResolution::Failed { .. }));
        let envelope = envelope_of(&h.blobs, &deadline).await;
        assert_eq!(envelope.status, SubagentResultStatus::Deadline);
        assert!(
            envelope
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("deadline")
        );
        assert_eq!(envelope.output, None);
        assert_eq!(
            h.runtime.closed.lock().unwrap().len(),
            3,
            "every terminal closes the child"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn close_tolerates_a_missing_or_already_closed_child() {
        let runtime = Arc::new(FakeChildRuntime::default());
        let h = harness(runtime).await;

        *h.runtime.close_error.lock().unwrap() = Some(AgentApiError::not_found("session"));
        h.service
            .close("agent_child")
            .await
            .expect("missing child is fine");
        *h.runtime.close_error.lock().unwrap() =
            Some(AgentApiError::rejected("session is already closed"));
        h.service
            .close("agent_child")
            .await
            .expect("closed child is fine");
        *h.runtime.close_error.lock().unwrap() = Some(AgentApiError::internal("store down"));
        let error = h
            .service
            .close("agent_child")
            .await
            .expect_err("infrastructure error");
        assert_eq!(error.kind, AgentApiErrorKind::Internal);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn child_session_ids_are_stable_and_distinct_per_execution() {
        assert_eq!(child_session_id("wte:a"), child_session_id("wte:a"));
        assert_ne!(child_session_id("wte:a"), child_session_id("wte:b"));
        assert!(child_session_id("wte:a").as_str().starts_with("agent_"));
        assert_eq!(
            child_session_id("wte:a").as_str().len(),
            "agent_".len() + 32
        );
    }

    /// The in-memory store counts a closed child as no longer open; the
    /// service's reservation goes through the same store rule.
    #[tokio::test(flavor = "current_thread")]
    async fn prepare_reserves_a_concurrency_slot_freed_by_a_closed_child() {
        let runtime = FakeChildRuntime::with_profile("reviewer", 1);
        let h = harness(runtime).await;
        let limits = SubagentLimits {
            max_concurrent: 1,
            ..SubagentLimits::default()
        };
        let first = start_args(&h.blobs, "wte:a", "parent", "reviewer", "reviewer", limits).await;
        let second = start_args(&h.blobs, "wte:b", "parent", "reviewer", "reviewer", limits).await;
        let (child, _) = prepared(h.service.prepare(first, 100).await.expect("first prepare"));
        let message = rejection_message(
            &h.blobs,
            h.service
                .prepare(second.clone(), 101)
                .await
                .expect("second prepare"),
        )
        .await;
        assert!(message.contains("maxConcurrent"), "{message}");

        // Close the first child in the store (the fake runtime does not).
        let closed = engine::CoreAgentCodec
            .encode_uncommitted(&engine::UncommittedCoreAgentEvent {
                observed_at_ms: 150,
                joins: engine::CoreAgentJoins::default(),
                event: engine::CoreAgentEvent::Lifecycle(engine::CoreAgentLifecycleEvent::Closed),
            })
            .expect("encode close");
        h.sessions
            .append(AppendSessionEvents {
                session_id: SessionId::new(&child.session_id),
                expected_head: None,
                events: vec![closed],
            })
            .await
            .expect("close child");
        assert_eq!(
            h.sessions
                .load_session(&SessionId::new(&child.session_id))
                .await
                .expect("load")
                .expect("child")
                .lifecycle_status,
            SessionLifecycleStatus::Closed
        );
        assert!(matches!(
            h.service.prepare(second, 102).await.expect("third prepare"),
            SubagentPrepareActivityResult::Prepared { .. }
        ));
    }
}
