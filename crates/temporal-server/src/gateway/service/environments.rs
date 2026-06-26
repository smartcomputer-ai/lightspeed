use super::*;

use ::environments::SessionEnvironmentBindingRecord;
use engine::ToolExecutionTarget;
use tools::{
    environment::EnvironmentToolContext,
    environment::projection::{
        EnvironmentCapabilities, EnvironmentKind, EnvironmentRecord, EnvironmentStatus,
    },
    targets::ENV_TARGET_NAMESPACE,
};

impl GatewayAgentApi {
    pub(super) async fn load_session_state_with_current_environment_projection(
        &self,
        session_id: &SessionId,
    ) -> Result<LoadedSession, AgentApiError> {
        let loaded = self.load_session_state(session_id).await?;
        self.refresh_environment_projection_for_idle_session(session_id, &loaded.state)
            .await?;
        self.load_session_state(session_id).await
    }

    pub(super) async fn load_session_runtime_environments(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<RuntimeEnvironment>, AgentApiError> {
        let mut environments = self
            .environment_manager
            .environments()
            .cloned()
            .collect::<Vec<_>>();
        let bindings = ::environments::SessionEnvironmentBindingStore::list_bindings_for_session(
            self.store.as_ref(),
            session_id,
        )
        .await
        .map_err(map_environments_error)?;
        let blobs: Arc<dyn BlobStore> = self.store.clone();
        for binding in bindings {
            environments.push(runtime_environment_from_binding_projection(
                binding,
                blobs.clone(),
            )?);
        }
        Ok(environments)
    }

    pub(super) async fn project_session_environments(
        &self,
        session_id: &SessionId,
        state: &engine::CoreAgentState,
    ) -> Result<SessionEnvironmentListResponse, AgentApiError> {
        let runtime_environments = self.load_session_runtime_environments(session_id).await?;
        let active_env_id = self
            .environment_manager
            .active_environment_id_for(&runtime_environments, state)
            .map(ToOwned::to_owned);
        let environments = runtime_environments
            .iter()
            .map(|environment| {
                session_environment_view(environment.record(), active_env_id.as_deref())
            })
            .collect();
        Ok(SessionEnvironmentListResponse {
            active_env_id,
            environments,
        })
    }

    pub(super) async fn project_session_environment(
        &self,
        session_id: &SessionId,
        state: &engine::CoreAgentState,
        env_id: &str,
    ) -> Result<SessionEnvironmentView, AgentApiError> {
        let runtime_environments = self.load_session_runtime_environments(session_id).await?;
        let active_env_id = self
            .environment_manager
            .active_environment_id_for(&runtime_environments, state);
        let environment = runtime_environments
            .iter()
            .find(|environment| environment.env_id() == env_id)
            .ok_or_else(|| AgentApiError::not_found(format!("environment not found: {env_id}")))?;
        Ok(session_environment_view(
            environment.record(),
            active_env_id,
        ))
    }

    pub(super) async fn activation_target_for_environment(
        &self,
        session_id: &SessionId,
        env_id: &str,
    ) -> Result<ToolExecutionTarget, AgentApiError> {
        let runtime_environments = self.load_session_runtime_environments(session_id).await?;
        let environment = runtime_environments
            .iter()
            .find(|environment| environment.env_id() == env_id)
            .ok_or_else(|| AgentApiError::not_found(format!("environment not found: {env_id}")))?;
        activation_target_for_environment_record(environment.record())
    }

    pub(super) async fn session_has_process_environment(
        &self,
        session_id: &SessionId,
    ) -> Result<bool, AgentApiError> {
        let runtime_environments = self.load_session_runtime_environments(session_id).await?;
        Ok(runtime_environments.iter().any(|environment| {
            environment.record().status == EnvironmentStatus::Ready
                && environment.record().capabilities.process_exec
        }))
    }

    pub(super) async fn session_has_job_environment(
        &self,
        session_id: &SessionId,
    ) -> Result<bool, AgentApiError> {
        let runtime_environments = self.load_session_runtime_environments(session_id).await?;
        Ok(runtime_environments.iter().any(|environment| {
            let record = environment.record();
            record.status == EnvironmentStatus::Ready
                && (record.capabilities.job_start
                    || record.capabilities.job_list
                    || record.capabilities.job_read
                    || record.capabilities.job_cancel)
        }))
    }

    pub(super) async fn wait_for_environment_default_target(
        &self,
        session_id: &SessionId,
        expected_target: Option<&ToolExecutionTarget>,
        baseline_failures: usize,
    ) -> Result<(), AgentApiError> {
        let started = Instant::now();
        loop {
            if started.elapsed() > self.operation_timeout {
                return Err(AgentApiError::internal(format!(
                    "timed out waiting for environment target update: {session_id}"
                )));
            }
            if let Some(status) = self.query_status_optional(session_id).await? {
                if status.admission_failures.len() > baseline_failures {
                    if let Some(failure) = status.admission_failures.last() {
                        return Err(map_admission_failure_to_api_error(failure));
                    }
                }
                if let Some(error) = status.last_error {
                    return Err(AgentApiError::internal(format!(
                        "agent workflow reported error: {error}"
                    )));
                }
            }
            let loaded = self.load_session_state(session_id).await?;
            let actual_target = loaded
                .state
                .tooling
                .routing
                .default_targets
                .get(ENV_TARGET_NAMESPACE);
            if actual_target == expected_target {
                return Ok(());
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }
}

pub(super) fn runtime_environment_from_binding_projection(
    binding: SessionEnvironmentBindingRecord,
    blobs: Arc<dyn BlobStore>,
) -> Result<RuntimeEnvironment, AgentApiError> {
    crate::environment::runtime_environment_from_binding_record(
        &binding,
        EnvironmentToolContext::new(None, blobs).with_session_id(binding.session_id.as_str()),
    )
    .map_err(|error| AgentApiError::internal(error.to_string()))
}

pub(super) fn parse_environment_id(value: String) -> Result<String, AgentApiError> {
    if value.is_empty() || value.trim() != value {
        return Err(AgentApiError::invalid_request(
            "invalid environment id: expected a non-empty id with no surrounding whitespace",
        ));
    }
    ToolExecutionTarget::new(ENV_TARGET_NAMESPACE, value.as_str())
        .validate()
        .map_err(|error| {
            AgentApiError::invalid_request(format!("invalid environment id: {error}"))
        })?;
    Ok(value)
}

pub(super) fn activation_target_for_environment_record(
    record: &EnvironmentRecord,
) -> Result<ToolExecutionTarget, AgentApiError> {
    if record.status != EnvironmentStatus::Ready {
        return Err(AgentApiError::rejected(format!(
            "environment is not ready: {}",
            record.env_id
        )));
    }
    let target = record.exec_target.clone().ok_or_else(|| {
        AgentApiError::rejected(format!(
            "environment has no execution target: {}",
            record.env_id
        ))
    })?;
    if target.namespace != ENV_TARGET_NAMESPACE {
        return Err(AgentApiError::internal(format!(
            "environment {} uses non-env execution target namespace: {}",
            record.env_id, target.namespace
        )));
    }
    target
        .validate()
        .map_err(|error| AgentApiError::internal(error.to_string()))?;
    Ok(target)
}

pub(super) fn activate_environment_command(target: ToolExecutionTarget) -> CoreAgentCommand {
    CoreAgentCommand::SetDefaultToolTarget { target }
}

pub(super) fn deactivate_environment_command() -> CoreAgentCommand {
    CoreAgentCommand::ClearDefaultToolTarget {
        namespace: ENV_TARGET_NAMESPACE.to_owned(),
    }
}

pub(super) fn session_environment_view(
    record: &EnvironmentRecord,
    active_env_id: Option<&str>,
) -> SessionEnvironmentView {
    SessionEnvironmentView {
        env_id: record.env_id.clone(),
        kind: api_environment_kind(record.kind),
        status: api_environment_status(record.status),
        capabilities: api_environment_capabilities(record.capabilities),
        exec_target: record.exec_target.as_ref().map(api_tool_execution_target),
        cwd: record.cwd.as_ref().map(|cwd| cwd.as_str().to_owned()),
        active: active_env_id == Some(record.env_id.as_str()),
    }
}

fn api_tool_execution_target(target: &ToolExecutionTarget) -> ToolExecutionTargetView {
    ToolExecutionTargetView {
        namespace: target.namespace.clone(),
        id: target.id.clone(),
    }
}

fn api_environment_kind(kind: EnvironmentKind) -> SessionEnvironmentKindView {
    match kind {
        EnvironmentKind::Sandbox => SessionEnvironmentKindView::Sandbox,
        EnvironmentKind::AttachedHost => SessionEnvironmentKindView::AttachedHost,
    }
}

fn api_environment_status(status: EnvironmentStatus) -> SessionEnvironmentStatusView {
    match status {
        EnvironmentStatus::Attaching => SessionEnvironmentStatusView::Attaching,
        EnvironmentStatus::Ready => SessionEnvironmentStatusView::Ready,
        EnvironmentStatus::Degraded => SessionEnvironmentStatusView::Degraded,
        EnvironmentStatus::Detached => SessionEnvironmentStatusView::Detached,
    }
}

fn api_environment_capabilities(
    capabilities: EnvironmentCapabilities,
) -> SessionEnvironmentCapabilitiesView {
    SessionEnvironmentCapabilitiesView {
        fs_read: capabilities.fs_read,
        fs_write: capabilities.fs_write,
        process_exec: capabilities.process_exec,
        process_stdin: capabilities.process_stdin,
        job_start: capabilities.job_start,
        job_list: capabilities.job_list,
        job_read: capabilities.job_read,
        job_cancel: capabilities.job_cancel,
        job_wait_hint: capabilities.job_wait_hint,
        job_dependencies: capabilities.job_dependencies,
        job_queue_keys: capabilities.job_queue_keys,
        network: capabilities.network,
        persistent: capabilities.persistent,
    }
}
