use super::*;

use environment_registry::{
    CreateSessionEnvironmentBinding, EnvironmentId as RegistryEnvironmentId,
    EnvironmentProviderKind as RegistryProviderKind, EnvironmentProviderRecord,
    EnvironmentProviderStatus as RegistryProviderStatus, EnvironmentTargetRecord,
    SessionEnvironmentBindingRecord, SessionEnvironmentBindingStatus,
    SessionEnvironmentCapabilities, SessionEnvironmentFsRoute, SessionEnvironmentFsRouteAccess,
    SessionEnvironmentKind, UpdateEnvironmentTargetStatus, UpdateSessionEnvironmentBindingStatus,
    UpsertEnvironmentTargetRecord,
};
use host_protocol::{
    control::targets::{
        AttachTargetParams, AttachedHostSpec, CloseTargetParams, CreateTargetParams,
        HostTargetAttachRequest, HostTargetCreateRequest, HostTargetStatus, HostTargetSummary,
        SandboxTargetSpec,
    },
    shared::{HostPath, HostTargetId},
};
use tools::targets::ENV_TARGET_NAMESPACE;

impl GatewayAgentApi {
    pub(super) async fn create_session_environment_record(
        &self,
        params: SessionEnvironmentCreateParams,
    ) -> Result<SessionEnvironmentCreateResponse, AgentApiError> {
        let session_id = parse_core_session_id(params.session_id)?;
        let env_id = parse_or_allocate_environment_id(params.env_id)?;
        let loaded = self.load_session_state(&session_id).await?;
        self.require_open_session(&session_id, &loaded)?;
        if params.activate {
            self.require_open_idle_session(&session_id, &loaded, "environment creation")?;
        }

        let provider_id = parse_environment_provider_id(params.provider_id)?;
        let provider = self.read_online_environment_provider(&provider_id).await?;
        if !provider.capabilities.create_target {
            return Err(AgentApiError::rejected(format!(
                "environment provider does not support target creation: {provider_id}"
            )));
        }

        let binding_kind = binding_kind_for_create_request(&provider, &params.request);
        let request = host_target_create_request(params.request)?;
        let mut controller = self
            .host_controller_connector
            .connect(&provider.controller_connection)
            .await?;
        let response = controller
            .create_target(&CreateTargetParams { request })
            .await?;
        let binding = self
            .store_session_environment_binding(
                session_id.clone(),
                env_id,
                provider,
                response.target,
                response.connection,
                binding_kind,
            )
            .await?;
        self.maybe_activate_environment_binding(&session_id, &loaded, &binding, params.activate)
            .await?;

        let response = self
            .project_session_environment_lifecycle_response(&session_id, binding.env_id.as_str())
            .await?;
        Ok(SessionEnvironmentCreateResponse {
            environment: response.environment,
            active_env_id: response.active_env_id,
            environments: response.environments,
        })
    }

    pub(super) async fn attach_session_environment_record(
        &self,
        params: SessionEnvironmentAttachParams,
    ) -> Result<SessionEnvironmentAttachResponse, AgentApiError> {
        let session_id = parse_core_session_id(params.session_id)?;
        let env_id = parse_or_allocate_environment_id(params.env_id)?;
        let loaded = self.load_session_state(&session_id).await?;
        self.require_open_session(&session_id, &loaded)?;
        if params.activate {
            self.require_open_idle_session(&session_id, &loaded, "environment attachment")?;
        }

        let provider_id = parse_environment_provider_id(params.provider_id)?;
        let provider = self.read_online_environment_provider(&provider_id).await?;
        if !provider.capabilities.attach_target {
            return Err(AgentApiError::rejected(format!(
                "environment provider does not support target attachment: {provider_id}"
            )));
        }

        let binding_kind = binding_kind_for_provider(&provider);
        let request = host_target_attach_request(params.request)?;
        let mut controller = self
            .host_controller_connector
            .connect(&provider.controller_connection)
            .await?;
        let response = controller
            .attach_target(&AttachTargetParams { request })
            .await?;
        let binding = self
            .store_session_environment_binding(
                session_id.clone(),
                env_id,
                provider,
                response.target,
                response.connection,
                binding_kind,
            )
            .await?;
        self.maybe_activate_environment_binding(&session_id, &loaded, &binding, params.activate)
            .await?;

        let response = self
            .project_session_environment_lifecycle_response(&session_id, binding.env_id.as_str())
            .await?;
        Ok(SessionEnvironmentAttachResponse {
            environment: response.environment,
            active_env_id: response.active_env_id,
            environments: response.environments,
        })
    }

    pub(super) async fn close_session_environment_record(
        &self,
        params: SessionEnvironmentCloseParams,
    ) -> Result<SessionEnvironmentCloseResponse, AgentApiError> {
        let session_id = parse_core_session_id(params.session_id)?;
        let env_id = parse_registry_environment_id(params.env_id)?;
        let loaded = self.load_session_state(&session_id).await?;
        self.require_open_session(&session_id, &loaded)?;

        let binding = environment_registry::SessionEnvironmentBindingStore::read_binding(
            self.store.as_ref(),
            &session_id,
            &env_id,
        )
        .await
        .map_err(map_environment_registry_error)?;
        let is_active = loaded
            .state
            .tooling
            .routing
            .default_targets
            .get(ENV_TARGET_NAMESPACE)
            == Some(&binding.exec_target);
        if is_active {
            self.require_open_idle_session(&session_id, &loaded, "environment close")?;
            let baseline_failures = self
                .query_status_optional(&session_id)
                .await?
                .map(|status| status.admission_failures.len())
                .unwrap_or(0);
            self.submit_core_command(&session_id, deactivate_environment_command())
                .await?;
            self.wait_for_environment_default_target(&session_id, None, baseline_failures)
                .await?;
        }

        if params.close_target.unwrap_or(true) {
            let provider = environment_registry::EnvironmentProviderStore::read_provider(
                self.store.as_ref(),
                &binding.provider_id,
            )
            .await
            .map_err(map_environment_registry_error)?;
            let mut controller = self
                .host_controller_connector
                .connect(&provider.controller_connection)
                .await?;
            let response = controller
                .close_target(&CloseTargetParams {
                    target_id: binding.target_id.clone(),
                    force: params.force,
                })
                .await?;
            let _ = environment_registry::EnvironmentTargetStore::update_target_status(
                self.store.as_ref(),
                UpdateEnvironmentTargetStatus {
                    provider_id: binding.provider_id.clone(),
                    target_id: binding.target_id.clone(),
                    status: response.status,
                    observed_at_ms: now_ms()?,
                },
            )
            .await
            .map_err(map_environment_registry_error)?;
        }

        let detached = environment_registry::SessionEnvironmentBindingStore::update_binding_status(
            self.store.as_ref(),
            UpdateSessionEnvironmentBindingStatus {
                session_id: session_id.clone(),
                env_id,
                status: SessionEnvironmentBindingStatus::Detached,
                updated_at_ms: now_ms()?,
            },
        )
        .await
        .map_err(map_environment_registry_error)?;

        let response = self
            .project_session_environment_lifecycle_response(&session_id, detached.env_id.as_str())
            .await?;
        Ok(SessionEnvironmentCloseResponse {
            environment: response.environment,
            active_env_id: response.active_env_id,
            environments: response.environments,
        })
    }

    async fn read_online_environment_provider(
        &self,
        provider_id: &environment_registry::EnvironmentProviderId,
    ) -> Result<EnvironmentProviderRecord, AgentApiError> {
        let provider = environment_registry::EnvironmentProviderStore::read_provider(
            self.store.as_ref(),
            provider_id,
        )
        .await
        .map_err(map_environment_registry_error)?;
        if provider.status != RegistryProviderStatus::Online {
            return Err(AgentApiError::rejected(format!(
                "environment provider is not online: {provider_id}"
            )));
        }
        Ok(provider)
    }

    async fn store_session_environment_binding(
        &self,
        session_id: SessionId,
        env_id: RegistryEnvironmentId,
        provider: EnvironmentProviderRecord,
        target: HostTargetSummary,
        connection: host_protocol::shared::HostConnectionSpec,
        kind: SessionEnvironmentKind,
    ) -> Result<SessionEnvironmentBindingRecord, AgentApiError> {
        if connection.target_id != target.target_id {
            return Err(AgentApiError::rejected(format!(
                "host controller returned mismatched target ids: target={} connection={}",
                target.target_id.as_str(),
                connection.target_id.as_str()
            )));
        }
        let now = now_ms()?;
        let target = environment_registry::EnvironmentTargetStore::upsert_target(
            self.store.as_ref(),
            UpsertEnvironmentTargetRecord::from((provider.provider_id.clone(), target, now)),
        )
        .await
        .map_err(map_environment_registry_error)?;
        let persistent = matches!(kind, SessionEnvironmentKind::AttachedHost);
        let capabilities =
            SessionEnvironmentCapabilities::from_host(&connection.capabilities, persistent);
        let cwd = connection
            .default_cwd
            .clone()
            .or_else(|| target.default_cwd.clone());
        let binding = environment_registry::SessionEnvironmentBindingStore::create_binding(
            self.store.as_ref(),
            CreateSessionEnvironmentBinding {
                session_id,
                env_id: env_id.clone(),
                provider_id: provider.provider_id,
                target_id: target.target_id.clone(),
                kind,
                status: binding_status_from_target(target.status),
                capabilities,
                connection,
                cwd: cwd.clone(),
                fs_routes: fs_routes_for_binding(&env_id, &target)?,
                created_at_ms: now,
            },
        )
        .await
        .map_err(map_environment_registry_error)?;
        Ok(binding)
    }

    async fn maybe_activate_environment_binding(
        &self,
        session_id: &SessionId,
        loaded: &LoadedSession,
        binding: &SessionEnvironmentBindingRecord,
        activate: bool,
    ) -> Result<(), AgentApiError> {
        if !activate
            || loaded
                .state
                .tooling
                .routing
                .default_targets
                .get(ENV_TARGET_NAMESPACE)
                == Some(&binding.exec_target)
        {
            return Ok(());
        }
        let baseline_failures = self
            .query_status_optional(session_id)
            .await?
            .map(|status| status.admission_failures.len())
            .unwrap_or(0);
        self.submit_core_command(
            session_id,
            activate_environment_command(binding.exec_target.clone()),
        )
        .await?;
        self.wait_for_environment_default_target(
            session_id,
            Some(&binding.exec_target),
            baseline_failures,
        )
        .await
    }

    async fn project_session_environment_lifecycle_response(
        &self,
        session_id: &SessionId,
        env_id: &str,
    ) -> Result<SessionEnvironmentActivateResponse, AgentApiError> {
        let mut loaded = self
            .load_session_state_with_current_environment_projection(session_id)
            .await?;
        let _ = self.configure_session_toolset(session_id, &loaded).await?;
        loaded = self.load_session_state(session_id).await?;
        let environment = self
            .project_session_environment(session_id, &loaded.state, env_id)
            .await?;
        let response = self
            .project_session_environments(session_id, &loaded.state)
            .await?;
        Ok(SessionEnvironmentActivateResponse {
            environment,
            active_env_id: response.active_env_id,
            environments: response.environments,
        })
    }

    fn require_open_session(
        &self,
        session_id: &SessionId,
        loaded: &LoadedSession,
    ) -> Result<(), AgentApiError> {
        if loaded.state.lifecycle.status != CoreAgentStatus::Open {
            return Err(AgentApiError::rejected(format!(
                "session is not open: {session_id}"
            )));
        }
        Ok(())
    }
}

fn parse_core_session_id(value: String) -> Result<SessionId, AgentApiError> {
    SessionId::try_new(value)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid session id: {error}")))
}

fn parse_or_allocate_environment_id(
    value: Option<String>,
) -> Result<RegistryEnvironmentId, AgentApiError> {
    let value = value.unwrap_or_else(|| format!("env_{}", uuid::Uuid::new_v4().simple()));
    parse_registry_environment_id(value)
}

fn parse_registry_environment_id(value: String) -> Result<RegistryEnvironmentId, AgentApiError> {
    let value = parse_environment_id(value)?;
    RegistryEnvironmentId::try_new(value)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid environment id: {error}")))
}

fn host_target_create_request(
    value: HostTargetCreateRequestView,
) -> Result<HostTargetCreateRequest, AgentApiError> {
    Ok(match value {
        HostTargetCreateRequestView::Sandbox { spec } => HostTargetCreateRequest::Sandbox {
            spec: sandbox_target_spec(spec)?,
        },
        HostTargetCreateRequestView::AttachedHost { spec } => {
            HostTargetCreateRequest::AttachedHost {
                spec: attached_host_spec(spec)?,
            }
        }
        HostTargetCreateRequestView::Provider {
            provider_type,
            spec,
        } => {
            validate_provider_type(&provider_type)?;
            HostTargetCreateRequest::Provider {
                provider_type,
                spec,
            }
        }
    })
}

fn host_target_attach_request(
    value: HostTargetAttachRequestView,
) -> Result<HostTargetAttachRequest, AgentApiError> {
    Ok(match value {
        HostTargetAttachRequestView::Target { target_id } => HostTargetAttachRequest::Target {
            target_id: HostTargetId::new(target_id),
        },
        HostTargetAttachRequestView::Provider {
            provider_type,
            spec,
        } => {
            validate_provider_type(&provider_type)?;
            HostTargetAttachRequest::Provider {
                provider_type,
                spec,
            }
        }
    })
}

fn sandbox_target_spec(value: SandboxTargetSpecView) -> Result<SandboxTargetSpec, AgentApiError> {
    Ok(SandboxTargetSpec {
        template: value.template,
        image: value.image,
        cwd: value
            .cwd
            .map(HostPath::new)
            .transpose()
            .map_err(|error| AgentApiError::invalid_request(format!("invalid cwd: {error}")))?,
        env: value.env,
        labels: value.labels,
        provider_options: value.provider_options,
    })
}

fn attached_host_spec(value: AttachedHostSpecView) -> Result<AttachedHostSpec, AgentApiError> {
    Ok(AttachedHostSpec {
        name: value.name,
        endpoint: value.endpoint,
        cwd: value
            .cwd
            .map(HostPath::new)
            .transpose()
            .map_err(|error| AgentApiError::invalid_request(format!("invalid cwd: {error}")))?,
        labels: value.labels,
        provider_options: value.provider_options,
    })
}

fn validate_provider_type(provider_type: &str) -> Result<(), AgentApiError> {
    if provider_type.is_empty() {
        return Err(AgentApiError::invalid_request(
            "provider type must not be empty",
        ));
    }
    Ok(())
}

fn binding_kind_for_create_request(
    provider: &EnvironmentProviderRecord,
    request: &HostTargetCreateRequestView,
) -> SessionEnvironmentKind {
    match request {
        HostTargetCreateRequestView::Sandbox { .. } => SessionEnvironmentKind::Sandbox,
        HostTargetCreateRequestView::AttachedHost { .. } => SessionEnvironmentKind::AttachedHost,
        HostTargetCreateRequestView::Provider { .. } => binding_kind_for_provider(provider),
    }
}

fn binding_kind_for_provider(provider: &EnvironmentProviderRecord) -> SessionEnvironmentKind {
    match provider.provider_kind {
        RegistryProviderKind::Sandbox => SessionEnvironmentKind::Sandbox,
        RegistryProviderKind::Bridge | RegistryProviderKind::Custom => {
            SessionEnvironmentKind::AttachedHost
        }
    }
}

fn binding_status_from_target(status: HostTargetStatus) -> SessionEnvironmentBindingStatus {
    match status {
        HostTargetStatus::Ready => SessionEnvironmentBindingStatus::Ready,
        HostTargetStatus::Creating | HostTargetStatus::Starting | HostTargetStatus::Unknown => {
            SessionEnvironmentBindingStatus::Attaching
        }
        HostTargetStatus::Stopped
        | HostTargetStatus::Closing
        | HostTargetStatus::Closed
        | HostTargetStatus::Failed => SessionEnvironmentBindingStatus::Degraded,
    }
}

fn fs_routes_for_binding(
    env_id: &RegistryEnvironmentId,
    target: &EnvironmentTargetRecord,
) -> Result<Vec<SessionEnvironmentFsRoute>, AgentApiError> {
    let capabilities = &target.capabilities;
    if !capabilities.filesystem_read {
        return Ok(Vec::new());
    }
    let source_path = target
        .metadata
        .get("fsRoot")
        .map(|path| {
            HostPath::new(path).map_err(|error| {
                AgentApiError::rejected(format!("invalid environment fsRoot metadata: {error}"))
            })
        })
        .transpose()?;
    Ok(vec![SessionEnvironmentFsRoute {
        path: HostPath::root(),
        source_path,
        access: if capabilities.filesystem_write {
            SessionEnvironmentFsRouteAccess::ReadWrite
        } else {
            SessionEnvironmentFsRouteAccess::ReadOnly
        },
        same_state_as_active_env: Some(env_id.clone()),
    }])
}
