//! Hosted Fleet subagent control-plane service.

use std::sync::Arc;

use api::{
    AgentApiError, AgentApiService, InputItem, RunStartParams, SessionStartParams,
    SessionUpdateParams,
};
use api_projection::{MAX_EVENT_PAGE_LIMIT, read_all_session_entries, replay_core_agent_state};
use async_trait::async_trait;
use engine::{
    AgentHandle, BlobRef, CoreAgentIoError, EventSeq, RunId, SessionId, SubmissionId, ToolBatchId,
    ToolCallId, ToolCallStatus, ToolInvocationRequest, ToolInvocationResult, TurnId,
    core_agent_clone_opening_events,
    storage::{
        BlobStore, BlobStoreError, CreateClonedSession, CreateForkedSession, ListSessionLinks,
        SessionLinkDirection, SessionRecord, SessionStore, SessionStoreError, UpsertSessionLink,
    },
};
use serde_json::{Value, json};
use tools::fleet::{
    AGENT_CANCEL_TOOL_NAME, AGENT_LIST_TOOL_NAME, AGENT_READ_TOOL_NAME, AGENT_SPAWN_TOOL_NAME,
    AgentSpawnArgs, AgentSpawnOutput, AgentSpawnSource, EnvironmentPolicy, VfsPolicy,
};
use vfs::{
    CreateVfsWorkspaceRecord, VfsCatalogError, VfsMountSource, VfsMountStore, VfsPath,
    VfsWorkspaceId, VfsWorkspaceStore,
};

pub const FLEET_CHILD_RELATIONSHIP: &str = "fleet_child";

#[derive(Clone)]
pub struct FleetService {
    sessions: Arc<dyn SessionStore>,
    runtime: Arc<dyn FleetChildRuntime>,
    workspace_store: Option<Arc<dyn VfsWorkspaceStore>>,
    mount_store: Option<Arc<dyn VfsMountStore>>,
}

impl FleetService {
    pub fn new(sessions: Arc<dyn SessionStore>, runtime: Arc<dyn FleetChildRuntime>) -> Self {
        Self {
            sessions,
            runtime,
            workspace_store: None,
            mount_store: None,
        }
    }

    pub fn with_vfs_stores(
        mut self,
        workspace_store: Arc<dyn VfsWorkspaceStore>,
        mount_store: Arc<dyn VfsMountStore>,
    ) -> Self {
        self.workspace_store = Some(workspace_store);
        self.mount_store = Some(mount_store);
        self
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

        let outcome = self
            .create_or_reuse_child(
                &context,
                &source_record,
                &child_session_id,
                source_seq,
                &spawn_request_id,
                child_id_was_derived,
            )
            .await?;
        let skip_pre_run_setup = outcome.has_matching_spawn_link();
        if !skip_pre_run_setup {
            self.apply_resource_policies(&child_session_id, context.observed_at_ms, &args)
                .await?;
        }

        self.runtime.start_session(&child_session_id).await?;
        if !skip_pre_run_setup && let Some(config_overrides) = args.config_overrides.clone() {
            self.runtime
                .patch_session_config(&child_session_id, config_overrides)
                .await?;
        }
        self.upsert_spawn_link(
            &context,
            &source_session_id,
            &child_session_id,
            source_seq,
            &spawn_request_id,
            &args,
        )
        .await?;
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
            status: if matches!(outcome, ChildCreateOutcome::Created) {
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
    ) -> Result<ChildCreateOutcome, AgentApiError> {
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
            Ok(_) => Ok(ChildCreateOutcome::Created),
            Err(SessionStoreError::SessionAlreadyExists { .. }) => {
                let existing = self
                    .validate_existing_child(
                        child_session_id,
                        &source_record.session_id,
                        source_seq,
                        spawn_request_id,
                        child_id_was_derived,
                    )
                    .await?;
                Ok(ChildCreateOutcome::Reused {
                    matching_spawn_link: existing.matching_spawn_link,
                })
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
    ) -> Result<ExistingChildValidation, AgentApiError> {
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
                return Ok(ExistingChildValidation {
                    matching_spawn_link: false,
                });
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
            return Ok(ExistingChildValidation {
                matching_spawn_link: true,
            });
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

    async fn apply_resource_policies(
        &self,
        child_session_id: &SessionId,
        observed_at_ms: u64,
        args: &AgentSpawnArgs,
    ) -> Result<(), AgentApiError> {
        if args.environment != EnvironmentPolicy::Share {
            return Err(AgentApiError::invalid_request(
                "agent_spawn environment policy must be share",
            ));
        }
        match args.vfs {
            VfsPolicy::Share => Ok(()),
            VfsPolicy::Isolate => {
                self.isolate_vfs_mounts(child_session_id, observed_at_ms)
                    .await
            }
        }
    }

    async fn isolate_vfs_mounts(
        &self,
        child_session_id: &SessionId,
        observed_at_ms: u64,
    ) -> Result<(), AgentApiError> {
        let workspace_store = self.workspace_store.as_ref().ok_or_else(|| {
            AgentApiError::internal("agent_spawn vfs isolate requires a workspace store")
        })?;
        let mount_store = self.mount_store.as_ref().ok_or_else(|| {
            AgentApiError::internal("agent_spawn vfs isolate requires a mount store")
        })?;
        let created_at_ms = nonnegative_i64(observed_at_ms, "observed_at_ms")?;
        let mounts = mount_store
            .list_mounts(child_session_id)
            .await
            .map_err(map_vfs_catalog_error)?;
        for mount in mounts {
            let VfsMountSource::Workspace { workspace_id } = mount.source.clone() else {
                continue;
            };
            let child_workspace_id = isolated_workspace_id(child_session_id, &mount.mount_path);
            if workspace_id == child_workspace_id {
                continue;
            }
            let source_workspace = workspace_store
                .read_workspace(&workspace_id)
                .await
                .map_err(map_vfs_catalog_error)?;
            match workspace_store
                .create_workspace(CreateVfsWorkspaceRecord {
                    workspace_id: child_workspace_id.clone(),
                    base_snapshot_ref: Some(source_workspace.head_snapshot_ref.clone()),
                    head_snapshot_ref: source_workspace.head_snapshot_ref,
                    created_at_ms,
                })
                .await
            {
                Ok(_) | Err(VfsCatalogError::AlreadyExists { .. }) => {}
                Err(error) => return Err(map_vfs_catalog_error(error)),
            }
            let mut isolated_mount = mount;
            isolated_mount.source = VfsMountSource::Workspace {
                workspace_id: child_workspace_id,
            };
            mount_store
                .put_mount(isolated_mount)
                .await
                .map_err(map_vfs_catalog_error)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChildCreateOutcome {
    Created,
    Reused { matching_spawn_link: bool },
}

impl ChildCreateOutcome {
    const fn has_matching_spawn_link(self) -> bool {
        matches!(
            self,
            ChildCreateOutcome::Reused {
                matching_spawn_link: true
            }
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExistingChildValidation {
    matching_spawn_link: bool,
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

    async fn patch_session_config(
        &self,
        session_id: &SessionId,
        patch: Value,
    ) -> Result<(), AgentApiError>;

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

    async fn patch_session_config(
        &self,
        session_id: &SessionId,
        patch: Value,
    ) -> Result<(), AgentApiError> {
        let patch = serde_json::from_value(patch).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid config_overrides: {error}"))
        })?;
        self.api
            .update_session(SessionUpdateParams {
                session_id: session_id.as_str().to_owned(),
                expected_config_revision: None,
                patch,
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

#[derive(Clone)]
pub struct FleetToolExecutor {
    blobs: Arc<dyn BlobStore>,
    service: FleetService,
}

impl FleetToolExecutor {
    pub fn new(blobs: Arc<dyn BlobStore>, service: FleetService) -> Self {
        Self { blobs, service }
    }

    pub async fn invoke(
        &self,
        context: FleetInvocationContext,
        call: &ToolInvocationRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        match call.tool_name.as_str() {
            AGENT_SPAWN_TOOL_NAME => self.invoke_spawn(context, call).await,
            AGENT_LIST_TOOL_NAME | AGENT_READ_TOOL_NAME | AGENT_CANCEL_TOOL_NAME => {
                fleet_failed_result(
                    self.blobs.as_ref(),
                    call.call_id.clone(),
                    format!("{} is not implemented until P83 G6", call.tool_name),
                )
                .await
            }
            other => {
                fleet_failed_result(
                    self.blobs.as_ref(),
                    call.call_id.clone(),
                    format!("unknown Fleet tool: {other}"),
                )
                .await
            }
        }
    }

    async fn invoke_spawn(
        &self,
        context: FleetInvocationContext,
        call: &ToolInvocationRequest,
    ) -> Result<ToolInvocationResult, CoreAgentIoError> {
        let args: AgentSpawnArgs = self.decode_args(call).await?;
        match self.service.spawn(context, args).await {
            Ok(output) => {
                let output_ref = self
                    .blobs
                    .put_bytes(serde_json::to_vec(&output).map_err(io_error)?)
                    .await
                    .map_err(map_blob_error)?;
                let visible = spawn_model_visible_text(&output);
                let visible_ref = self
                    .blobs
                    .put_bytes(visible.into_bytes())
                    .await
                    .map_err(map_blob_error)?;
                Ok(ToolInvocationResult {
                    call_id: call.call_id.clone(),
                    status: ToolCallStatus::Succeeded,
                    output_ref: Some(output_ref),
                    model_visible_output_ref: Some(visible_ref),
                    error_ref: None,
                    effects: Vec::new(),
                })
            }
            Err(error) => {
                fleet_failed_result(self.blobs.as_ref(), call.call_id.clone(), error.to_string())
                    .await
            }
        }
    }

    async fn decode_args<T>(&self, call: &ToolInvocationRequest) -> Result<T, CoreAgentIoError>
    where
        T: serde::de::DeserializeOwned,
    {
        let bytes = self
            .blobs
            .read_bytes(&call.arguments_ref)
            .await
            .map_err(map_blob_error)?;
        serde_json::from_slice(&bytes)
            .map_err(|error| io_error(format!("invalid JSON tool arguments: {error}")))
    }
}

fn validate_spawn_args(args: &AgentSpawnArgs) -> Result<(), AgentApiError> {
    if args.input.trim().is_empty() {
        return Err(AgentApiError::invalid_request(
            "agent_spawn input must not be empty",
        ));
    }
    if args.environment != EnvironmentPolicy::Share {
        return Err(AgentApiError::invalid_request(
            "agent_spawn environment policy must be share",
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

fn isolated_workspace_id(child_session_id: &SessionId, mount_path: &VfsPath) -> VfsWorkspaceId {
    let digest = digest_suffix(&format!("{child_session_id}:{}", mount_path.as_str()));
    VfsWorkspaceId::new(format!("workspace_{digest}"))
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

fn map_vfs_catalog_error(error: VfsCatalogError) -> AgentApiError {
    match error {
        VfsCatalogError::AlreadyExists { .. } | VfsCatalogError::RevisionConflict { .. } => {
            AgentApiError::conflict(error.to_string())
        }
        VfsCatalogError::NotFound { .. } => AgentApiError::not_found(error.to_string()),
        VfsCatalogError::InvalidInput { .. } => AgentApiError::invalid_request(error.to_string()),
        VfsCatalogError::Store { .. } => AgentApiError::internal(error.to_string()),
    }
}

fn nonnegative_i64(value: u64, name: &str) -> Result<i64, AgentApiError> {
    i64::try_from(value)
        .map_err(|_| AgentApiError::invalid_request(format!("{name} is too large: {value}")))
}

fn spawn_model_visible_text(output: &AgentSpawnOutput) -> String {
    match output.child_run_id.as_deref() {
        Some(run_id) => format!(
            "Agent {} {} and started run {}.",
            output.child_session_id, output.status, run_id
        ),
        None => format!(
            "Agent {} {} without starting a run.",
            output.child_session_id, output.status
        ),
    }
}

async fn fleet_failed_result(
    blobs: &dyn BlobStore,
    call_id: ToolCallId,
    message: impl Into<String>,
) -> Result<ToolInvocationResult, CoreAgentIoError> {
    let error_ref = blobs
        .put_bytes(message.into().into_bytes())
        .await
        .map_err(map_blob_error)?;
    Ok(ToolInvocationResult {
        call_id,
        status: ToolCallStatus::Failed,
        output_ref: None,
        model_visible_output_ref: Some(error_ref.clone()),
        error_ref: Some(error_ref),
        effects: Vec::new(),
    })
}

fn map_blob_error(error: BlobStoreError) -> CoreAgentIoError {
    io_error(format!("Fleet tool blob operation failed: {error}"))
}

fn io_error(error: impl std::fmt::Display) -> CoreAgentIoError {
    CoreAgentIoError::Failed {
        message: error.to_string(),
    }
}

pub fn default_agent_handle() -> AgentHandle {
    AgentHandle::new("lightspeed.agent")
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Mutex};

    use async_trait::async_trait;
    use engine::{
        ContextConfig, ModelSelection, ProviderApiKind, RunConfig, SessionConfig, ToolCallId,
        ToolConfig, ToolInvocationRequest, ToolName, TurnConfig,
        storage::{CreateSession, InMemorySessionStore, SessionStore},
    };
    use vfs::{CompareAndSetVfsWorkspaceHead, VfsMountAccess, VfsMountRecord, VfsWorkspaceRecord};

    use super::*;

    #[derive(Default)]
    struct FakeRuntime {
        started_sessions: Mutex<Vec<SessionId>>,
        patched_sessions: Mutex<Vec<(SessionId, Value)>>,
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

        async fn patch_session_config(
            &self,
            session_id: &SessionId,
            patch: Value,
        ) -> Result<(), AgentApiError> {
            self.patched_sessions
                .lock()
                .expect("lock")
                .push((session_id.clone(), patch));
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

    #[tokio::test(flavor = "current_thread")]
    async fn spawn_applies_config_overrides_before_first_run() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let source = open_source_session(sessions.as_ref()).await;
        let runtime = Arc::new(FakeRuntime::default());
        let service = FleetService::new(sessions, runtime.clone());
        let output = service
            .spawn(
                context(source),
                serde_json::from_value(json!({
                    "input": "do work",
                    "config_overrides": {
                        "tools": {
                            "fleet": { "op": "set", "value": true }
                        }
                    }
                }))
                .expect("args"),
            )
            .await
            .expect("spawn");

        let child = SessionId::new(output.child_session_id);
        assert_eq!(
            runtime.patched_sessions.lock().expect("lock").as_slice(),
            &[(
                child.clone(),
                json!({
                    "tools": {
                        "fleet": { "op": "set", "value": true }
                    }
                })
            )]
        );
        assert_eq!(
            runtime.started_runs.lock().expect("lock")[0].0,
            child,
            "run starts after patch target is established"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn vfs_isolate_rewrites_workspace_mounts_and_keeps_snapshots() {
        let vfs = Arc::new(TestVfsCatalog::default());
        let child = SessionId::new("child");
        let source_workspace = VfsWorkspaceId::new("workspace_source");
        let head = BlobRef::from_bytes(b"snapshot-head");
        vfs.create_workspace(CreateVfsWorkspaceRecord {
            workspace_id: source_workspace.clone(),
            base_snapshot_ref: None,
            head_snapshot_ref: head.clone(),
            created_at_ms: 1,
        })
        .await
        .expect("source workspace");
        vfs.put_mount(VfsMountRecord {
            session_id: child.clone(),
            mount_path: VfsPath::parse("/workspace").expect("path"),
            source: VfsMountSource::Workspace {
                workspace_id: source_workspace.clone(),
            },
            access: VfsMountAccess::ReadWrite,
        })
        .await
        .expect("workspace mount");
        let snapshot_ref = BlobRef::from_bytes(b"snapshot-mount");
        vfs.put_mount(VfsMountRecord {
            session_id: child.clone(),
            mount_path: VfsPath::parse("/readonly").expect("path"),
            source: VfsMountSource::Snapshot {
                snapshot_ref: snapshot_ref.clone(),
            },
            access: VfsMountAccess::ReadOnly,
        })
        .await
        .expect("snapshot mount");

        let service = FleetService::new(
            Arc::new(InMemorySessionStore::new()),
            Arc::new(FakeRuntime::default()),
        )
        .with_vfs_stores(vfs.clone(), vfs.clone());
        service
            .apply_resource_policies(
                &child,
                10,
                &serde_json::from_value(json!({
                    "input": "do work",
                    "vfs": "isolate"
                }))
                .expect("args"),
            )
            .await
            .expect("isolate");

        let mounts = vfs.list_mounts(&child).await.expect("mounts");
        let workspace_mount = mounts
            .iter()
            .find(|mount| mount.mount_path.as_str() == "/workspace")
            .expect("workspace mount");
        let VfsMountSource::Workspace { workspace_id } = &workspace_mount.source else {
            panic!("workspace mount source");
        };
        assert_ne!(workspace_id, &source_workspace);
        let child_workspace = vfs
            .read_workspace(workspace_id)
            .await
            .expect("child workspace");
        assert_eq!(child_workspace.base_snapshot_ref, Some(head.clone()));
        assert_eq!(child_workspace.head_snapshot_ref, head);
        let snapshot_mount = mounts
            .iter()
            .find(|mount| mount.mount_path.as_str() == "/readonly")
            .expect("snapshot mount");
        assert_eq!(
            snapshot_mount.source,
            VfsMountSource::Snapshot { snapshot_ref }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_executor_runs_spawn_and_writes_output_blobs() {
        let sessions = Arc::new(InMemorySessionStore::new());
        let source = open_source_session(sessions.as_ref()).await;
        let blobs = Arc::new(engine::storage::InMemoryBlobStore::new());
        let runtime = Arc::new(FakeRuntime::default());
        let service = FleetService::new(sessions, runtime);
        let executor = FleetToolExecutor::new(blobs.clone(), service);
        let arguments_ref = blobs
            .put_bytes(br#"{"input":"do work"}"#.to_vec())
            .await
            .expect("args");
        let result = executor
            .invoke(
                context(source),
                &ToolInvocationRequest {
                    call_id: ToolCallId::new("call_1"),
                    tool_name: ToolName::new(AGENT_SPAWN_TOOL_NAME),
                    arguments_ref,
                    execution_target: None,
                },
            )
            .await
            .expect("invoke");

        assert_eq!(result.status, ToolCallStatus::Succeeded);
        let output_ref = result.output_ref.expect("output");
        let output: AgentSpawnOutput =
            serde_json::from_slice(&blobs.read_bytes(&output_ref).await.expect("read output"))
                .expect("decode output");
        assert!(output.child_session_id.starts_with("agent_"));
        let visible_ref = result.model_visible_output_ref.expect("visible");
        let visible = blobs.read_text(&visible_ref).await.expect("read visible");
        assert!(visible.contains("started run"));
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

    async fn open_source_session(sessions: &InMemorySessionStore) -> SessionId {
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
        source
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

    #[derive(Default)]
    struct TestVfsCatalog {
        workspaces: Mutex<BTreeMap<VfsWorkspaceId, VfsWorkspaceRecord>>,
        mounts: Mutex<BTreeMap<(SessionId, VfsPath), VfsMountRecord>>,
    }

    #[async_trait]
    impl VfsWorkspaceStore for TestVfsCatalog {
        async fn create_workspace(
            &self,
            record: CreateVfsWorkspaceRecord,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            let mut workspaces = self.workspaces.lock().expect("workspace lock");
            if workspaces.contains_key(&record.workspace_id) {
                return Err(VfsCatalogError::AlreadyExists {
                    kind: "workspace",
                    id: record.workspace_id.to_string(),
                });
            }
            let workspace = VfsWorkspaceRecord {
                workspace_id: record.workspace_id,
                base_snapshot_ref: record.base_snapshot_ref,
                head_snapshot_ref: record.head_snapshot_ref,
                revision: 0,
                created_at_ms: record.created_at_ms,
                updated_at_ms: record.created_at_ms,
            };
            workspaces.insert(workspace.workspace_id.clone(), workspace.clone());
            Ok(workspace)
        }

        async fn read_workspace(
            &self,
            workspace_id: &VfsWorkspaceId,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            self.workspaces
                .lock()
                .expect("workspace lock")
                .get(workspace_id)
                .cloned()
                .ok_or_else(|| VfsCatalogError::NotFound {
                    kind: "workspace",
                    id: workspace_id.to_string(),
                })
        }

        async fn compare_and_set_head(
            &self,
            request: CompareAndSetVfsWorkspaceHead,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            let mut workspaces = self.workspaces.lock().expect("workspace lock");
            let workspace = workspaces.get_mut(&request.workspace_id).ok_or_else(|| {
                VfsCatalogError::NotFound {
                    kind: "workspace",
                    id: request.workspace_id.to_string(),
                }
            })?;
            workspace.head_snapshot_ref = request.new_head_snapshot_ref;
            workspace.revision += 1;
            workspace.updated_at_ms = request.updated_at_ms;
            Ok(workspace.clone())
        }

        async fn delete_workspace(
            &self,
            workspace_id: &VfsWorkspaceId,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            self.workspaces
                .lock()
                .expect("workspace lock")
                .remove(workspace_id)
                .ok_or_else(|| VfsCatalogError::NotFound {
                    kind: "workspace",
                    id: workspace_id.to_string(),
                })
        }
    }

    #[async_trait]
    impl VfsMountStore for TestVfsCatalog {
        async fn put_mount(&self, record: VfsMountRecord) -> Result<(), VfsCatalogError> {
            self.mounts.lock().expect("mount lock").insert(
                (record.session_id.clone(), record.mount_path.clone()),
                record,
            );
            Ok(())
        }

        async fn list_mounts(
            &self,
            session_id: &SessionId,
        ) -> Result<Vec<VfsMountRecord>, VfsCatalogError> {
            let mut mounts: Vec<_> = self
                .mounts
                .lock()
                .expect("mount lock")
                .values()
                .filter(|mount| &mount.session_id == session_id)
                .cloned()
                .collect();
            mounts.sort_by(|left, right| left.mount_path.as_str().cmp(right.mount_path.as_str()));
            Ok(mounts)
        }

        async fn remove_mount(
            &self,
            session_id: &SessionId,
            mount_path: &VfsPath,
        ) -> Result<(), VfsCatalogError> {
            self.mounts
                .lock()
                .expect("mount lock")
                .remove(&(session_id.clone(), mount_path.clone()))
                .ok_or_else(|| VfsCatalogError::NotFound {
                    kind: "mount",
                    id: format!("{session_id}:{mount_path}"),
                })?;
            Ok(())
        }
    }
}
