//! Hosted Fleet subagent control-plane service.

use std::sync::Arc;

use api::{AgentApiError, AgentApiService, InputItem, RunStartParams, SessionStartParams};
use api_projection::{MAX_EVENT_PAGE_LIMIT, read_all_session_entries, replay_core_agent_state};
use async_trait::async_trait;
use engine::{
    AgentHandle, BlobRef, EventSeq, RunId, SessionId, SubmissionId, ToolBatchId, ToolCallId,
    TurnId, core_agent_clone_opening_events,
    storage::{
        CreateClonedSession, CreateForkedSession, ListSessionLinks, SessionLinkDirection,
        SessionRecord, SessionStore, SessionStoreError, UpsertSessionLink,
    },
};
use serde_json::{Value, json};
use tools::fleet::{
    AgentSpawnArgs, AgentSpawnOutput, AgentSpawnSource, EnvironmentPolicy, VfsPolicy,
};

pub const FLEET_CHILD_RELATIONSHIP: &str = "fleet_child";

#[derive(Clone)]
pub struct FleetService {
    sessions: Arc<dyn SessionStore>,
    runtime: Arc<dyn FleetChildRuntime>,
}

impl FleetService {
    pub fn new(sessions: Arc<dyn SessionStore>, runtime: Arc<dyn FleetChildRuntime>) -> Self {
        Self { sessions, runtime }
    }

    pub async fn spawn(
        &self,
        context: FleetInvocationContext,
        args: AgentSpawnArgs,
    ) -> Result<AgentSpawnOutput, AgentApiError> {
        validate_spawn_args(&args)?;
        let source_session_id = self.resolve_source(&context, &args.source)?;
        let source_record = self.load_session_required(&source_session_id).await?;
        let child_id_was_derived = args.child_session_id.is_none();
        let child_session_id = match args.child_session_id.as_deref() {
            Some(session_id) => parse_session_id(session_id, "child_session_id")?,
            None => derived_child_session_id(&context),
        };
        let spawn_request_id = spawn_request_id(&context);
        let child_run_submission_id = child_run_submission_id(&context);
        let source_seq = if args.fork {
            Some(match args.fork_at_seq {
                Some(seq) => EventSeq::new(seq),
                None => self
                    .sessions
                    .safe_fork_seq(&source_session_id)
                    .await
                    .map_err(map_session_store_error)?,
            })
        } else {
            None
        };

        let created_or_reused = self
            .create_or_reuse_child(
                &context,
                &source_record,
                &child_session_id,
                source_seq,
                &spawn_request_id,
                child_id_was_derived,
            )
            .await?;
        self.upsert_spawn_link(
            &context,
            &source_session_id,
            &child_session_id,
            source_seq,
            &spawn_request_id,
            &args,
        )
        .await?;

        self.runtime.start_session(&child_session_id).await?;
        let child_run_id = if args.lifecycle.run_immediately {
            Some(
                self.runtime
                    .start_run(
                        &child_session_id,
                        args.input.clone(),
                        child_run_submission_id,
                    )
                    .await?,
            )
        } else {
            None
        };

        Ok(AgentSpawnOutput {
            child_session_id: child_session_id.as_str().to_owned(),
            child_run_id,
            status: if created_or_reused {
                "created".to_owned()
            } else {
                "reused".to_owned()
            },
        })
    }

    fn resolve_source(
        &self,
        context: &FleetInvocationContext,
        source: &AgentSpawnSource,
    ) -> Result<SessionId, AgentApiError> {
        match source {
            AgentSpawnSource::Self_ => Ok(context.parent_session_id.clone()),
            AgentSpawnSource::Session { session_id } => parse_session_id(session_id, "source"),
        }
    }

    async fn load_session_required(
        &self,
        session_id: &SessionId,
    ) -> Result<SessionRecord, AgentApiError> {
        self.sessions
            .load_session(session_id)
            .await
            .map_err(map_session_store_error)?
            .ok_or_else(|| AgentApiError::not_found(format!("session not found: {session_id}")))
    }

    async fn create_or_reuse_child(
        &self,
        context: &FleetInvocationContext,
        source_record: &SessionRecord,
        child_session_id: &SessionId,
        source_seq: Option<EventSeq>,
        spawn_request_id: &str,
        child_id_was_derived: bool,
    ) -> Result<bool, AgentApiError> {
        let result = if let Some(source_seq) = source_seq {
            self.sessions
                .create_forked_session(CreateForkedSession {
                    source_session_id: source_record.session_id.clone(),
                    session_id: child_session_id.clone(),
                    agent_handle: source_record.agent_handle.clone(),
                    source_seq,
                    created_at_ms: context.observed_at_ms,
                })
                .await
        } else {
            let entries = read_all_session_entries(
                self.sessions.as_ref(),
                &source_record.session_id,
                MAX_EVENT_PAGE_LIMIT as usize,
            )
            .await?;
            let state = replay_core_agent_state(&entries)?;
            let opening_events = core_agent_clone_opening_events(&state, context.observed_at_ms)
                .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
            self.sessions
                .create_cloned_session(CreateClonedSession {
                    source_session_id: source_record.session_id.clone(),
                    session_id: child_session_id.clone(),
                    agent_handle: source_record.agent_handle.clone(),
                    created_at_ms: context.observed_at_ms,
                    opening_events,
                })
                .await
        };

        match result {
            Ok(_) => Ok(true),
            Err(SessionStoreError::SessionAlreadyExists { .. }) => {
                self.validate_existing_child(
                    child_session_id,
                    &source_record.session_id,
                    source_seq,
                    spawn_request_id,
                    child_id_was_derived,
                )
                .await?;
                Ok(false)
            }
            Err(error) => Err(map_session_store_error(error)),
        }
    }

    async fn validate_existing_child(
        &self,
        child_session_id: &SessionId,
        source_session_id: &SessionId,
        source_seq: Option<EventSeq>,
        spawn_request_id: &str,
        child_id_was_derived: bool,
    ) -> Result<(), AgentApiError> {
        let existing = self.load_session_required(child_session_id).await?;
        if existing.source_session_id.as_ref() != Some(source_session_id) {
            return Err(AgentApiError::conflict(format!(
                "child session id {child_session_id} already exists with a different source"
            )));
        }
        if existing.source_seq != source_seq {
            return Err(AgentApiError::conflict(format!(
                "child session id {child_session_id} already exists with a different fork point"
            )));
        }
        let links = self
            .sessions
            .list_links(ListSessionLinks {
                session_id: child_session_id.clone(),
                direction: SessionLinkDirection::Incoming,
                relationship: Some(FLEET_CHILD_RELATIONSHIP.to_owned()),
                limit: 100,
            })
            .await
            .map_err(map_session_store_error)?;
        if links.is_empty() {
            if child_id_was_derived {
                return Ok(());
            }
            return Err(AgentApiError::conflict(format!(
                "child session id {child_session_id} already exists without matching fleet spawn metadata"
            )));
        }
        if links.iter().any(|link| {
            link.metadata
                .get("spawn_request_id")
                .and_then(Value::as_str)
                == Some(spawn_request_id)
        }) {
            return Ok(());
        }
        Err(AgentApiError::conflict(format!(
            "child session id {child_session_id} is already linked to a different spawn request"
        )))
    }

    async fn upsert_spawn_link(
        &self,
        context: &FleetInvocationContext,
        source_session_id: &SessionId,
        child_session_id: &SessionId,
        source_seq: Option<EventSeq>,
        spawn_request_id: &str,
        args: &AgentSpawnArgs,
    ) -> Result<(), AgentApiError> {
        self.sessions
            .upsert_link(UpsertSessionLink {
                from_session_id: context.parent_session_id.clone(),
                to_session_id: child_session_id.clone(),
                relationship: FLEET_CHILD_RELATIONSHIP.to_owned(),
                created_at_ms: context.observed_at_ms,
                metadata: spawn_link_metadata(
                    context,
                    source_session_id,
                    source_seq,
                    spawn_request_id,
                    args,
                ),
            })
            .await
            .map_err(map_session_store_error)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FleetInvocationContext {
    pub parent_session_id: SessionId,
    pub parent_run_id: RunId,
    pub turn_id: TurnId,
    pub batch_id: ToolBatchId,
    pub call_id: ToolCallId,
    pub observed_at_ms: u64,
}

#[async_trait]
pub trait FleetChildRuntime: Send + Sync {
    async fn start_session(&self, session_id: &SessionId) -> Result<(), AgentApiError>;

    async fn start_run(
        &self,
        session_id: &SessionId,
        input: String,
        submission_id: SubmissionId,
    ) -> Result<String, AgentApiError>;
}

#[derive(Clone)]
pub struct AgentApiFleetRuntime {
    api: Arc<dyn AgentApiService>,
}

impl AgentApiFleetRuntime {
    pub fn new(api: Arc<dyn AgentApiService>) -> Self {
        Self { api }
    }
}

#[async_trait]
impl FleetChildRuntime for AgentApiFleetRuntime {
    async fn start_session(&self, session_id: &SessionId) -> Result<(), AgentApiError> {
        self.api
            .start_session(SessionStartParams {
                session_id: Some(session_id.as_str().to_owned()),
                cwd: None,
                config: None,
            })
            .await?;
        Ok(())
    }

    async fn start_run(
        &self,
        session_id: &SessionId,
        input: String,
        submission_id: SubmissionId,
    ) -> Result<String, AgentApiError> {
        let response = self
            .api
            .start_run(RunStartParams {
                session_id: session_id.as_str().to_owned(),
                input: vec![InputItem::Text { text: input }],
                submission_id: Some(submission_id.as_str().to_owned()),
                config: None,
            })
            .await?;
        Ok(response.result.run.id)
    }
}

fn validate_spawn_args(args: &AgentSpawnArgs) -> Result<(), AgentApiError> {
    if args.input.trim().is_empty() {
        return Err(AgentApiError::invalid_request(
            "agent_spawn input must not be empty",
        ));
    }
    if args.config_overrides.is_some() {
        return Err(AgentApiError::invalid_request(
            "agent_spawn config_overrides are not implemented in this slice",
        ));
    }
    if args.environment != EnvironmentPolicy::Share {
        return Err(AgentApiError::invalid_request(
            "agent_spawn environment policy must be share",
        ));
    }
    if args.vfs != VfsPolicy::Share {
        return Err(AgentApiError::invalid_request(
            "agent_spawn vfs isolate requires the G4 resource-policy pass",
        ));
    }
    Ok(())
}

fn parse_session_id(value: &str, field: &str) -> Result<SessionId, AgentApiError> {
    SessionId::try_new(value.to_owned())
        .map_err(|error| AgentApiError::invalid_request(format!("invalid {field}: {error}")))
}

fn derived_child_session_id(context: &FleetInvocationContext) -> SessionId {
    let digest = digest_suffix(&spawn_request_material(context));
    SessionId::new(format!("agent_{digest}"))
}

fn spawn_request_id(context: &FleetInvocationContext) -> String {
    format!(
        "fleet_spawn_{}",
        digest_suffix(&spawn_request_material(context))
    )
}

fn child_run_submission_id(context: &FleetInvocationContext) -> SubmissionId {
    SubmissionId::new(format!(
        "fleet_run_{}",
        digest_suffix(&spawn_request_material(context))
    ))
}

fn spawn_request_material(context: &FleetInvocationContext) -> String {
    format!(
        "{}:{}:{}:{}:{}",
        context.parent_session_id,
        context.parent_run_id,
        context.turn_id,
        context.batch_id,
        context.call_id
    )
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

fn spawn_link_metadata(
    context: &FleetInvocationContext,
    source_session_id: &SessionId,
    source_seq: Option<EventSeq>,
    spawn_request_id: &str,
    args: &AgentSpawnArgs,
) -> Value {
    json!({
        "kind": "fleet_spawn",
        "spawn_request_id": spawn_request_id,
        "parent_run_id": context.parent_run_id.as_u64(),
        "turn_id": context.turn_id.as_u64(),
        "tool_batch_id": context.batch_id.as_u64(),
        "tool_call_id": context.call_id.as_str(),
        "source_session_id": source_session_id.as_str(),
        "source_seq": source_seq.map(EventSeq::as_u64),
        "fork": args.fork,
        "vfs": match args.vfs {
            VfsPolicy::Share => "share",
            VfsPolicy::Isolate => "isolate",
        },
        "environment": "share",
    })
}

fn map_session_store_error(error: SessionStoreError) -> AgentApiError {
    match error {
        SessionStoreError::SessionAlreadyExists { session_id } => {
            AgentApiError::conflict(format!("session already exists: {session_id}"))
        }
        SessionStoreError::SessionNotFound { session_id } => {
            AgentApiError::not_found(format!("session not found: {session_id}"))
        }
        SessionStoreError::ExpectedHeadMismatch { .. } => {
            AgentApiError::conflict(error.to_string())
        }
        SessionStoreError::InvalidForkPoint { .. }
        | SessionStoreError::InvalidRelationship { .. }
        | SessionStoreError::InvalidLimit { .. } => {
            AgentApiError::invalid_request(error.to_string())
        }
        SessionStoreError::Store { .. } => AgentApiError::internal(error.to_string()),
    }
}

pub fn default_agent_handle() -> AgentHandle {
    AgentHandle::new("lightspeed.agent")
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use engine::{
        ContextConfig, ModelSelection, ProviderApiKind, RunConfig, SessionConfig, ToolConfig,
        TurnConfig,
        storage::{CreateSession, InMemorySessionStore, SessionStore},
    };

    use super::*;

    #[derive(Default)]
    struct FakeRuntime {
        started_sessions: Mutex<Vec<SessionId>>,
        started_runs: Mutex<Vec<(SessionId, String, SubmissionId)>>,
    }

    #[async_trait]
    impl FleetChildRuntime for FakeRuntime {
        async fn start_session(&self, session_id: &SessionId) -> Result<(), AgentApiError> {
            self.started_sessions
                .lock()
                .expect("lock")
                .push(session_id.clone());
            Ok(())
        }

        async fn start_run(
            &self,
            session_id: &SessionId,
            input: String,
            submission_id: SubmissionId,
        ) -> Result<String, AgentApiError> {
            self.started_runs.lock().expect("lock").push((
                session_id.clone(),
                input,
                submission_id,
            ));
            Ok("run_1".to_owned())
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_clone_self_creates_child_link_and_starts_run() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let source = SessionId::new("parent");
        sessions
            .create_session(CreateSession {
                session_id: source.clone(),
                agent_handle: default_agent_handle(),
                created_at_ms: 1,
            })
            .await
            .expect("create source");
        let opening_events =
            core_agent_clone_opening_events(&open_state(), 2).expect("opening events");
        sessions
            .append(engine::storage::AppendSessionEvents {
                session_id: source.clone(),
                expected_head: None,
                events: opening_events,
            })
            .await
            .expect("append open");

        let runtime = Arc::new(FakeRuntime::default());
        let service = FleetService::new(sessions.clone(), runtime.clone());
        let output = service
            .spawn(context(source.clone()), spawn_args("summarize"))
            .await
            .expect("spawn");

        let child = SessionId::new(output.child_session_id);
        let child_record = sessions
            .load_session(&child)
            .await
            .expect("load")
            .expect("child");
        assert_eq!(child_record.source_session_id, Some(source.clone()));
        assert_eq!(child_record.source_seq, None);

        let links = sessions
            .list_links(ListSessionLinks {
                session_id: source,
                direction: SessionLinkDirection::Outgoing,
                relationship: Some(FLEET_CHILD_RELATIONSHIP.to_owned()),
                limit: 10,
            })
            .await
            .expect("links");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].to_session_id, child);

        assert_eq!(
            runtime.started_sessions.lock().expect("lock").as_slice(),
            &[links[0].to_session_id.clone()]
        );
        assert_eq!(output.child_run_id.as_deref(), Some("run_1"));
        assert_eq!(output.status, "created");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_retry_reuses_existing_child() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let source = SessionId::new("parent");
        sessions
            .create_session(CreateSession {
                session_id: source.clone(),
                agent_handle: default_agent_handle(),
                created_at_ms: 1,
            })
            .await
            .expect("create source");
        let opening_events =
            core_agent_clone_opening_events(&open_state(), 2).expect("opening events");
        sessions
            .append(engine::storage::AppendSessionEvents {
                session_id: source.clone(),
                expected_head: None,
                events: opening_events,
            })
            .await
            .expect("append open");

        let runtime = Arc::new(FakeRuntime::default());
        let service = FleetService::new(sessions, runtime);
        let first = service
            .spawn(context(source.clone()), spawn_args("do work"))
            .await
            .expect("first spawn");
        let second = service
            .spawn(context(source), spawn_args("do work"))
            .await
            .expect("retry spawn");

        assert_eq!(first.child_session_id, second.child_session_id);
        assert_eq!(second.status, "reused");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn explicit_child_id_collision_without_spawn_metadata_conflicts() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let source = SessionId::new("parent");
        sessions
            .create_session(CreateSession {
                session_id: source.clone(),
                agent_handle: default_agent_handle(),
                created_at_ms: 1,
            })
            .await
            .expect("create source");
        let opening_events =
            core_agent_clone_opening_events(&open_state(), 2).expect("opening events");
        sessions
            .append(engine::storage::AppendSessionEvents {
                session_id: source.clone(),
                expected_head: None,
                events: opening_events.clone(),
            })
            .await
            .expect("append open");
        let child = SessionId::new("explicit_child");
        sessions
            .create_cloned_session(CreateClonedSession {
                source_session_id: source.clone(),
                session_id: child,
                agent_handle: default_agent_handle(),
                created_at_ms: 3,
                opening_events,
            })
            .await
            .expect("preexisting clone");

        let runtime = Arc::new(FakeRuntime::default());
        let service = FleetService::new(sessions, runtime);
        let error = service
            .spawn(
                context(source),
                spawn_args_with_child("do work", "explicit_child"),
            )
            .await
            .expect_err("explicit collision must be rejected");

        assert_eq!(error.kind, api::AgentApiErrorKind::Conflict);
    }

    fn spawn_args(input: &str) -> AgentSpawnArgs {
        serde_json::from_value(json!({ "input": input })).expect("args")
    }

    fn spawn_args_with_child(input: &str, child_session_id: &str) -> AgentSpawnArgs {
        serde_json::from_value(json!({
            "input": input,
            "child_session_id": child_session_id
        }))
        .expect("args")
    }

    fn context(parent_session_id: SessionId) -> FleetInvocationContext {
        FleetInvocationContext {
            parent_session_id,
            parent_run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            batch_id: ToolBatchId::new(1),
            call_id: ToolCallId::new("call_1"),
            observed_at_ms: 10,
        }
    }

    fn open_state() -> engine::CoreAgentState {
        let mut state = engine::CoreAgentState::new();
        state.lifecycle.config = Some(SessionConfig {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "test".to_owned(),
                model: "test-model".to_owned(),
            },
            run: RunConfig::default(),
            turn: TurnConfig {
                max_output_tokens: None,
                tool_choice: None,
                provider_params: None,
            },
            context: ContextConfig { compaction: None },
            tools: ToolConfig::default(),
        });
        state
    }
}
