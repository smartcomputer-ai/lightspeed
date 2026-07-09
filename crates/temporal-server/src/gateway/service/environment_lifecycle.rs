use super::environment_providers::{environment_instance_view, registry_target_status};
use super::*;

use ::environments::{
    BeginCloseEnvironmentInstance, EnvironmentId as RegistryEnvironmentId, EnvironmentInstanceId,
    EnvironmentInstanceOrigin, EnvironmentInstanceRecord, EnvironmentProviderRecord,
    ListEnvironmentInstances, ObserveEnvironmentInstance, ObservedEnvironmentTarget,
    PutSessionEnvironmentBinding, SessionEnvironmentBindingRecord, SessionEnvironmentBindingState,
    SessionEnvironmentFsRoute, SessionEnvironmentFsRouteAccess, UpdateEnvironmentInstanceStatus,
    UpdateSessionEnvironmentBindingState,
};
use host_protocol::{
    control::targets::{
        AttachedHostSpec, CloseTargetParams, CreateTargetParams, HostTargetCreateRequest,
        HostTargetStatus, SandboxTargetSpec,
    },
    shared::HostPath,
};
use tools::targets::ENV_TARGET_NAMESPACE;

impl GatewayAgentApi {
    pub(super) async fn detach_all_session_environment_bindings(
        &self,
        session_id: &SessionId,
    ) -> Result<(), AgentApiError> {
        let bindings = ::environments::SessionEnvironmentBindingStore::list_bindings_for_session(
            self.store.as_ref(),
            session_id,
        )
        .await
        .map_err(map_environments_error)?;
        for binding in bindings {
            if binding.state == SessionEnvironmentBindingState::Attached {
                ::environments::SessionEnvironmentBindingStore::update_binding_state(
                    self.store.as_ref(),
                    UpdateSessionEnvironmentBindingState {
                        session_id: session_id.clone(),
                        env_id: binding.env_id,
                        state: SessionEnvironmentBindingState::Detached,
                        updated_at_ms: now_ms()?,
                    },
                )
                .await
                .map_err(map_environments_error)?;
            }
        }
        Ok(())
    }

    pub(super) async fn create_environment_record(
        &self,
        params: EnvironmentCreateParams,
    ) -> Result<EnvironmentCreateResponse, AgentApiError> {
        let provider_id = parse_environment_provider_id(params.provider_id)?;
        let provider = self.read_live_environment_provider(&provider_id).await?;
        if !provider.capabilities.create_target {
            return Err(AgentApiError::rejected(format!(
                "environment provider does not support target creation: {provider_id}"
            )));
        }
        let request = host_target_create_request(params.request)?;
        let mut controller = self
            .host_controller_connector
            .connect(&provider.controller_connection)
            .await?;
        let response = controller
            .create_target(&CreateTargetParams { request })
            .await?;
        let observed_at_ms = now_ms()?;
        let instance = ::environments::EnvironmentInstanceStore::observe_instance(
            self.store.as_ref(),
            ObserveEnvironmentInstance::from_observation(
                allocate_environment_instance_id(),
                provider_id,
                EnvironmentInstanceOrigin::Provisioned,
                ObservedEnvironmentTarget {
                    target: response.target,
                    connection: response.connection,
                },
                observed_at_ms,
            ),
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentCreateResponse {
            environment: environment_instance_view(&instance),
        })
    }

    pub(super) async fn read_environment_record(
        &self,
        params: EnvironmentReadParams,
    ) -> Result<EnvironmentReadResponse, AgentApiError> {
        let instance_id = parse_environment_instance_id(params.instance_id)?;
        let instance = ::environments::EnvironmentInstanceStore::read_instance(
            self.store.as_ref(),
            &instance_id,
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentReadResponse {
            environment: environment_instance_view(&instance),
        })
    }

    pub(super) async fn list_environment_records(
        &self,
        params: EnvironmentListParams,
    ) -> Result<EnvironmentListResponse, AgentApiError> {
        let provider_id = params
            .provider_id
            .map(parse_environment_provider_id)
            .transpose()?;
        let instances = ::environments::EnvironmentInstanceStore::list_instances(
            self.store.as_ref(),
            ListEnvironmentInstances {
                provider_id,
                status: params.status.map(registry_target_status),
                origin: None,
            },
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentListResponse {
            environments: instances.iter().map(environment_instance_view).collect(),
        })
    }

    pub(super) async fn close_environment_record(
        &self,
        params: EnvironmentCloseParams,
    ) -> Result<EnvironmentCloseResponse, AgentApiError> {
        let instance_id = parse_environment_instance_id(params.instance_id)?;
        let previous = ::environments::EnvironmentInstanceStore::read_instance(
            self.store.as_ref(),
            &instance_id,
        )
        .await
        .map_err(map_environments_error)?;
        let closing = ::environments::EnvironmentInstanceStore::begin_close_instance(
            self.store.as_ref(),
            BeginCloseEnvironmentInstance {
                instance_id: instance_id.clone(),
                updated_at_ms: now_ms()?,
            },
        )
        .await
        .map_err(map_environments_error)?;
        let provider = self
            .read_live_environment_provider(&closing.provider_id)
            .await;
        let result = async {
            let provider = provider?;
            if !provider.capabilities.close_target {
                return Err(AgentApiError::rejected(format!(
                    "environment provider does not support target close: {}",
                    provider.provider_id
                )));
            }
            let mut controller = self
                .host_controller_connector
                .connect(&provider.controller_connection)
                .await?;
            controller
                .close_target(&CloseTargetParams {
                    target_id: closing.provider_target_id.clone(),
                    force: false,
                })
                .await
        }
        .await;
        let (status, error) = match result {
            Ok(response) => (response.status, None),
            Err(error) if error.kind == AgentApiErrorKind::Rejected => {
                (previous.status, Some(error))
            }
            Err(error) => (HostTargetStatus::Unknown, Some(error)),
        };
        let instance = ::environments::EnvironmentInstanceStore::update_instance_status(
            self.store.as_ref(),
            UpdateEnvironmentInstanceStatus {
                instance_id,
                status,
                observed_at_ms: now_ms()?,
            },
        )
        .await
        .map_err(map_environments_error)?;
        if let Some(error) = error {
            return Err(error);
        }
        Ok(EnvironmentCloseResponse {
            environment: environment_instance_view(&instance),
        })
    }

    pub(super) async fn attach_session_environment_record(
        &self,
        params: SessionEnvironmentAttachParams,
    ) -> Result<SessionEnvironmentAttachResponse, AgentApiError> {
        let session_id = parse_core_session_id(params.session_id)?;
        let env_id = parse_or_allocate_environment_id(params.env_id)?;
        let instance_id = parse_environment_instance_id(params.instance_id)?;
        let loaded = self.load_session_state(&session_id).await?;
        self.require_open_session(&session_id, &loaded)?;
        if params.activate {
            self.require_open_idle_session(&session_id, &loaded, "environment attachment")?;
        }
        let feature = loaded
            .state
            .lifecycle
            .config
            .as_ref()
            .and_then(|config| config.features.environments.as_ref())
            .ok_or_else(|| {
                AgentApiError::rejected(
                    "environment attachment requires the environments feature to be granted",
                )
            })?;
        let instance = ::environments::EnvironmentInstanceStore::read_instance(
            self.store.as_ref(),
            &instance_id,
        )
        .await
        .map_err(map_environments_error)?;
        if feature.providers.as_ref().is_some_and(|providers| {
            !providers
                .iter()
                .any(|id| id == instance.provider_id.as_str())
        }) {
            return Err(AgentApiError::rejected(format!(
                "environment provider is not allowed by session config: {}",
                instance.provider_id
            )));
        }
        self.read_live_environment_provider(&instance.provider_id)
            .await?;
        if !instance.is_attachable() {
            return Err(AgentApiError::rejected(format!(
                "environment instance is not ready: {instance_id}"
            )));
        }
        let cwd = params
            .cwd
            .map(HostPath::new)
            .transpose()
            .map_err(|error| AgentApiError::invalid_request(format!("invalid cwd: {error}")))?
            .or_else(|| instance.default_cwd.clone());
        let fs_routes = if params.fs_routes.is_empty() {
            default_fs_routes(&env_id, &instance)?
        } else {
            params
                .fs_routes
                .into_iter()
                .map(|route| registry_fs_route(route, &env_id))
                .collect::<Result<Vec<_>, _>>()?
        };
        let binding = ::environments::SessionEnvironmentBindingStore::put_binding(
            self.store.as_ref(),
            PutSessionEnvironmentBinding {
                session_id: session_id.clone(),
                env_id,
                instance_id,
                cwd,
                fs_routes,
                updated_at_ms: now_ms()?,
            },
        )
        .await
        .map_err(map_environments_error)?;
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

    pub(super) async fn detach_session_environment_record(
        &self,
        params: SessionEnvironmentDetachParams,
    ) -> Result<SessionEnvironmentDetachResponse, AgentApiError> {
        let session_id = parse_core_session_id(params.session_id)?;
        let env_id = parse_registry_environment_id(params.env_id)?;
        let loaded = self.load_session_state(&session_id).await?;
        self.require_open_session(&session_id, &loaded)?;
        let binding = ::environments::SessionEnvironmentBindingStore::read_binding(
            self.store.as_ref(),
            &session_id,
            &env_id,
        )
        .await
        .map_err(map_environments_error)?;
        let target = binding.exec_target();
        let is_active = loaded
            .state
            .tooling
            .routing
            .default_targets
            .get(ENV_TARGET_NAMESPACE)
            == Some(&target);
        if is_active {
            self.require_open_idle_session(&session_id, &loaded, "environment detach")?;
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
        let detached = ::environments::SessionEnvironmentBindingStore::update_binding_state(
            self.store.as_ref(),
            UpdateSessionEnvironmentBindingState {
                session_id: session_id.clone(),
                env_id,
                state: SessionEnvironmentBindingState::Detached,
                updated_at_ms: now_ms()?,
            },
        )
        .await
        .map_err(map_environments_error)?;
        let response = self
            .project_session_environment_lifecycle_response(&session_id, detached.env_id.as_str())
            .await?;
        Ok(SessionEnvironmentDetachResponse {
            environment: response.environment,
            active_env_id: response.active_env_id,
            environments: response.environments,
        })
    }

    pub(super) async fn read_live_environment_provider(
        &self,
        provider_id: &::environments::EnvironmentProviderId,
    ) -> Result<EnvironmentProviderRecord, AgentApiError> {
        let provider = ::environments::EnvironmentProviderStore::read_provider(
            self.store.as_ref(),
            provider_id,
        )
        .await
        .map_err(map_environments_error)?;
        if !provider.is_live_at(now_ms()?) {
            return Err(AgentApiError::rejected(format!(
                "environment provider lease is not live: {provider_id}"
            )));
        }
        Ok(provider)
    }

    async fn maybe_activate_environment_binding(
        &self,
        session_id: &SessionId,
        loaded: &LoadedSession,
        binding: &SessionEnvironmentBindingRecord,
        activate: bool,
    ) -> Result<(), AgentApiError> {
        let target = binding.exec_target();
        if !activate
            || loaded
                .state
                .tooling
                .routing
                .default_targets
                .get(ENV_TARGET_NAMESPACE)
                == Some(&target)
        {
            return Ok(());
        }
        let baseline_failures = self
            .query_status_optional(session_id)
            .await?
            .map(|status| status.admission_failures.len())
            .unwrap_or(0);
        self.submit_core_command(session_id, activate_environment_command(target.clone()))
            .await?;
        self.wait_for_environment_default_target(session_id, Some(&target), baseline_failures)
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

pub(super) fn parse_core_session_id(value: String) -> Result<SessionId, AgentApiError> {
    SessionId::try_new(value)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid session id: {error}")))
}

fn parse_or_allocate_environment_id(
    value: Option<String>,
) -> Result<RegistryEnvironmentId, AgentApiError> {
    parse_registry_environment_id(
        value.unwrap_or_else(|| format!("env_{}", uuid::Uuid::new_v4().simple())),
    )
}

pub(super) fn parse_registry_environment_id(
    value: String,
) -> Result<RegistryEnvironmentId, AgentApiError> {
    let value = parse_environment_id(value)?;
    RegistryEnvironmentId::try_new(value)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid environment id: {error}")))
}

pub(super) fn parse_environment_instance_id(
    value: String,
) -> Result<EnvironmentInstanceId, AgentApiError> {
    EnvironmentInstanceId::try_new(value).map_err(|error| {
        AgentApiError::invalid_request(format!("invalid environment instance id: {error}"))
    })
}

pub(super) fn allocate_environment_instance_id() -> EnvironmentInstanceId {
    EnvironmentInstanceId::new(format!("evi_{}", uuid::Uuid::new_v4().simple()))
}

fn host_target_create_request(
    value: HostTargetCreateRequestView,
) -> Result<HostTargetCreateRequest, AgentApiError> {
    Ok(match value {
        HostTargetCreateRequestView::Sandbox { spec } => HostTargetCreateRequest::Sandbox {
            spec: SandboxTargetSpec {
                template: spec.template,
                image: spec.image,
                cwd: optional_host_path(spec.cwd, "cwd")?,
                env: spec.env,
                labels: spec.labels,
                provider_options: spec.provider_options,
            },
        },
        HostTargetCreateRequestView::AttachedHost { spec } => {
            HostTargetCreateRequest::AttachedHost {
                spec: AttachedHostSpec {
                    name: spec.name,
                    endpoint: spec.endpoint,
                    cwd: optional_host_path(spec.cwd, "cwd")?,
                    labels: spec.labels,
                    provider_options: spec.provider_options,
                },
            }
        }
        HostTargetCreateRequestView::Provider {
            provider_type,
            spec,
        } => {
            if provider_type.is_empty() {
                return Err(AgentApiError::invalid_request(
                    "provider type must not be empty",
                ));
            }
            HostTargetCreateRequest::Provider {
                provider_type,
                spec,
            }
        }
    })
}

fn optional_host_path(
    value: Option<String>,
    name: &str,
) -> Result<Option<HostPath>, AgentApiError> {
    value
        .map(HostPath::new)
        .transpose()
        .map_err(|error| AgentApiError::invalid_request(format!("invalid {name}: {error}")))
}

fn registry_fs_route(
    route: SessionEnvironmentFsRouteView,
    env_id: &RegistryEnvironmentId,
) -> Result<SessionEnvironmentFsRoute, AgentApiError> {
    Ok(SessionEnvironmentFsRoute {
        path: HostPath::new(route.path).map_err(|error| {
            AgentApiError::invalid_request(format!("invalid fs route path: {error}"))
        })?,
        source_path: optional_host_path(route.source_path, "fs route source path")?,
        access: match route.access {
            SessionEnvironmentFsAccessView::ReadOnly => SessionEnvironmentFsRouteAccess::ReadOnly,
            SessionEnvironmentFsAccessView::ReadWrite => SessionEnvironmentFsRouteAccess::ReadWrite,
        },
        same_state_as_active_env: route.same_state_as_active_env.then(|| env_id.clone()),
    })
}

fn default_fs_routes(
    env_id: &RegistryEnvironmentId,
    instance: &EnvironmentInstanceRecord,
) -> Result<Vec<SessionEnvironmentFsRoute>, AgentApiError> {
    if !instance.capabilities.filesystem_read {
        return Ok(Vec::new());
    }
    let source_path = instance
        .metadata
        .get("fsRoot")
        .map(HostPath::new)
        .transpose()
        .map_err(|error| AgentApiError::rejected(format!("invalid fsRoot metadata: {error}")))?;
    Ok(vec![SessionEnvironmentFsRoute {
        path: HostPath::root(),
        source_path,
        access: if instance.capabilities.filesystem_write {
            SessionEnvironmentFsRouteAccess::ReadWrite
        } else {
            SessionEnvironmentFsRouteAccess::ReadOnly
        },
        same_state_as_active_env: Some(env_id.clone()),
    }])
}
