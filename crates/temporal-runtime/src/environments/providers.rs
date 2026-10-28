use super::provider_controllers::ControllerError;
use super::*;

use ::environments::{
    DaemonBuild, EnvironmentCloseReason, EnvironmentDisplayStatus,
    EnvironmentProviderBindingRecord, EnvironmentProviderId, EnvironmentProviderRecord,
    EnvironmentRecord, EnvironmentRegistryError, EnvironmentSource, EnvironmentState, IdlePolicy,
    ObservedTarget, PowerState, TargetPhase,
};
use environment_protocol::{
    control::template::{EnvironmentTemplate, ListTemplatesParams},
    ids::UniverseId,
};

impl EnvironmentService {
    pub(crate) async fn list_environment_provider_binding_records(
        &self,
        _params: EnvironmentProviderBindingListParams,
    ) -> Result<EnvironmentProviderBindingListResponse, AgentApiError> {
        let records = ::environments::EnvironmentProviderBindingStore::list_provider_bindings(
            self.store.as_ref(),
            self.store.config().universe_id,
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentProviderBindingListResponse {
            bindings: records
                .iter()
                .map(environment_provider_binding_view)
                .collect(),
        })
    }

    pub(crate) async fn read_environment_provider_binding_record(
        &self,
        params: EnvironmentProviderBindingReadParams,
    ) -> Result<EnvironmentProviderBindingReadResponse, AgentApiError> {
        let provider_id = parse_environment_provider_id(params.provider_id)?;
        let record = ::environments::EnvironmentProviderBindingStore::read_provider_binding(
            self.store.as_ref(),
            self.store.config().universe_id,
            &provider_id,
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentProviderBindingReadResponse {
            binding: environment_provider_binding_view(&record),
        })
    }

    /// Templates are listed live from every enabled binding's provider;
    /// nothing about them is stored.
    pub(crate) async fn list_environment_template_records(
        &self,
        params: EnvironmentTemplateListParams,
    ) -> Result<EnvironmentTemplateListResponse, AgentApiError> {
        let universe_id = self.store.config().universe_id;
        let bindings = match params
            .provider_id
            .map(parse_environment_provider_id)
            .transpose()?
        {
            Some(provider_id) => vec![
                ::environments::EnvironmentProviderBindingStore::read_provider_binding(
                    self.store.as_ref(),
                    universe_id,
                    &provider_id,
                )
                .await
                .map_err(map_environments_error)?,
            ],
            None => ::environments::EnvironmentProviderBindingStore::list_provider_bindings(
                self.store.as_ref(),
                universe_id,
            )
            .await
            .map_err(map_environments_error)?,
        };
        let universe = UniverseId::new(universe_id.to_string()).map_err(|error| {
            AgentApiError::internal(format!(
                "universe id does not fit the controller protocol: {error}"
            ))
        })?;
        let mut templates = Vec::new();
        for binding in bindings.into_iter().filter(|binding| binding.enabled) {
            let controller = self
                .connect_provider_controller(&binding.provider_id)
                .await
                .map_err(ControllerError::into_api_error)?;
            let response = controller
                .client
                .list_templates(&ListTemplatesParams {
                    universe_id: universe.clone(),
                })
                .await
                .map_err(|error| controller.call_error(error).into_api_error());
            controller.close().await;
            templates.extend(
                response?
                    .templates
                    .iter()
                    .map(|template| environment_template_view(&binding.provider_id, template)),
            );
        }
        templates.sort_by(|left, right| {
            (&left.provider_id, &left.template_id).cmp(&(&right.provider_id, &right.template_id))
        });
        Ok(EnvironmentTemplateListResponse { templates })
    }

    pub(crate) async fn read_environment_template_record(
        &self,
        params: EnvironmentTemplateReadParams,
    ) -> Result<EnvironmentTemplateReadResponse, AgentApiError> {
        let expected = params.template_id;
        let response = self
            .list_environment_template_records(EnvironmentTemplateListParams {
                provider_id: Some(params.provider_id),
            })
            .await?;
        let template = response
            .templates
            .into_iter()
            .find(|template| template.template_id == expected)
            .ok_or_else(|| {
                AgentApiError::not_found(format!("environment template not found: {expected}"))
            })?;
        Ok(EnvironmentTemplateReadResponse { template })
    }
}

pub(crate) fn parse_environment_provider_id(
    value: String,
) -> Result<EnvironmentProviderId, AgentApiError> {
    EnvironmentProviderId::try_new(value)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid provider id: {error}")))
}

pub(crate) fn environment_provider_binding_view(
    record: &EnvironmentProviderBindingRecord,
) -> EnvironmentProviderBindingView {
    EnvironmentProviderBindingView {
        provider_id: record.provider_id.to_string(),
        enabled: record.enabled,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

pub(crate) fn environment_template_view(
    provider_id: &EnvironmentProviderId,
    template: &EnvironmentTemplate,
) -> EnvironmentTemplateView {
    EnvironmentTemplateView {
        template_id: template.template_id.to_string(),
        provider_id: provider_id.to_string(),
        display_name: template.display_name.to_string(),
        description: template
            .description
            .as_ref()
            .map(|description| description.to_string()),
        public_ingress: template.public_ingress,
        deprecated: template.deprecated,
        resources: template
            .resources
            .as_ref()
            .map(|resources| EnvironmentTemplateResourcesView {
                vcpus: resources.vcpus,
                memory_bytes: resources.memory_bytes,
                disk_bytes: resources.disk_bytes,
            }),
        power_states: template
            .power_states
            .iter()
            .copied()
            .map(power_state_view)
            .collect(),
        idle: template.idle.as_ref().map(idle_policy_view),
        metadata: template.metadata.as_map().clone(),
    }
}

pub(crate) fn environment_view(record: &EnvironmentRecord) -> EnvironmentView {
    EnvironmentView {
        environment_id: record.environment_id.to_string(),
        request_id: record.request_id.as_ref().map(ToString::to_string),
        source: match &record.source {
            EnvironmentSource::Provisioned(source) => EnvironmentSourceView::Provisioned {
                provider_id: source.provider_id.to_string(),
                template_id: source.template_id.to_string(),
                target_id: source
                    .provider_target_id
                    .as_ref()
                    .map(|target_id| target_id.as_str().to_owned()),
                idle: source.idle.as_ref().map(idle_policy_view),
                observed: source.observed.as_ref().map(observed_target_view),
                public_endpoint: source.public_endpoint.clone(),
            },
            EnvironmentSource::External { endpoint } => EnvironmentSourceView::External {
                endpoint: endpoint.as_str().to_owned(),
            },
            EnvironmentSource::Registered(source) => EnvironmentSourceView::Registered {
                registration_key_id: source.registration_key_id.to_string(),
                daemon_id: source.daemon_id().to_string(),
                identity_mode: identity_mode_view(source.identity_mode),
                connected: source.connected,
                connection_changed_at_ms: source.connection_changed_at_ms,
                last_seen_at_ms: source.last_seen_at_ms,
            },
        },
        display_name: record.display_name.clone(),
        state: state_view(record.state),
        status: status_view(record.display_status()),
        state_message: record.state_message.clone(),
        close_reason: record.close_reason.map(close_reason_view),
        state_changed_at_ms: record.state_changed_at_ms,
        daemon: record.daemon_build.as_ref().map(daemon_view),
        metadata: record.metadata.clone(),
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
        closed_at_ms: record.closed_at_ms,
    }
}

fn observed_target_view(observed: &ObservedTarget) -> EnvironmentObservedTargetView {
    EnvironmentObservedTargetView {
        phase: match observed.phase {
            TargetPhase::Provisioning => EnvironmentTargetPhaseView::Provisioning,
            TargetPhase::Active => EnvironmentTargetPhaseView::Active,
            TargetPhase::Closing => EnvironmentTargetPhaseView::Closing,
            TargetPhase::Closed => EnvironmentTargetPhaseView::Closed,
            TargetPhase::Failed => EnvironmentTargetPhaseView::Failed,
            TargetPhase::Unknown => EnvironmentTargetPhaseView::Unknown,
        },
        power: observed.power.map(power_state_view),
        message: observed.message.clone(),
        observed_at_ms: observed.observed_at_ms,
    }
}

fn daemon_view(build: &DaemonBuild) -> EnvironmentDaemonView {
    EnvironmentDaemonView {
        version: build.version.clone(),
        git_sha: build.git_sha.clone(),
        protocol_version: build.protocol_version,
    }
}

pub(crate) fn identity_mode_view(
    value: ::environments::RegisteredIdentityMode,
) -> api::EnvironmentIdentityModeView {
    match value {
        ::environments::RegisteredIdentityMode::Persistent => {
            api::EnvironmentIdentityModeView::Persistent
        }
        ::environments::RegisteredIdentityMode::Ephemeral => {
            api::EnvironmentIdentityModeView::Ephemeral
        }
    }
}

pub(crate) fn registry_identity_mode(
    value: api::EnvironmentIdentityModeView,
) -> ::environments::RegisteredIdentityMode {
    match value {
        api::EnvironmentIdentityModeView::Persistent => {
            ::environments::RegisteredIdentityMode::Persistent
        }
        api::EnvironmentIdentityModeView::Ephemeral => {
            ::environments::RegisteredIdentityMode::Ephemeral
        }
    }
}

pub(crate) fn power_state_view(value: PowerState) -> EnvironmentPowerStateView {
    match value {
        PowerState::Running => EnvironmentPowerStateView::Running,
        PowerState::Paused => EnvironmentPowerStateView::Paused,
        PowerState::Suspended => EnvironmentPowerStateView::Suspended,
        PowerState::Stopped => EnvironmentPowerStateView::Stopped,
    }
}

pub(crate) fn registry_power_state(value: EnvironmentPowerStateView) -> PowerState {
    match value {
        EnvironmentPowerStateView::Running => PowerState::Running,
        EnvironmentPowerStateView::Paused => PowerState::Paused,
        EnvironmentPowerStateView::Suspended => PowerState::Suspended,
        EnvironmentPowerStateView::Stopped => PowerState::Stopped,
    }
}

pub(crate) fn idle_policy_view(value: &IdlePolicy) -> EnvironmentIdlePolicyView {
    EnvironmentIdlePolicyView {
        pause_after_ms: value.pause_after_ms,
        suspend_after_ms: value.suspend_after_ms,
        stop_after_ms: value.stop_after_ms,
        close_after_ms: value.close_after_ms,
    }
}

/// The idle policy a create-time override names, validated: Lightspeed
/// passes it to the provider unchanged and never interprets it.
pub(crate) fn registry_idle_policy(
    value: &EnvironmentIdlePolicyView,
) -> Result<IdlePolicy, AgentApiError> {
    let policy = IdlePolicy {
        pause_after_ms: value.pause_after_ms,
        suspend_after_ms: value.suspend_after_ms,
        stop_after_ms: value.stop_after_ms,
        close_after_ms: value.close_after_ms,
    };
    policy
        .validate()
        .map_err(|error| AgentApiError::invalid_request(format!("invalid idle policy: {error}")))?;
    Ok(policy)
}

pub(crate) fn state_view(value: EnvironmentState) -> EnvironmentStateView {
    match value {
        EnvironmentState::Provisioning => EnvironmentStateView::Provisioning,
        EnvironmentState::Open => EnvironmentStateView::Open,
        EnvironmentState::Closing => EnvironmentStateView::Closing,
        EnvironmentState::Closed => EnvironmentStateView::Closed,
        EnvironmentState::Failed => EnvironmentStateView::Failed,
    }
}

pub(crate) fn registry_state(value: EnvironmentStateView) -> EnvironmentState {
    match value {
        EnvironmentStateView::Provisioning => EnvironmentState::Provisioning,
        EnvironmentStateView::Open => EnvironmentState::Open,
        EnvironmentStateView::Closing => EnvironmentState::Closing,
        EnvironmentStateView::Closed => EnvironmentState::Closed,
        EnvironmentStateView::Failed => EnvironmentState::Failed,
    }
}

pub(crate) fn status_view(value: EnvironmentDisplayStatus) -> EnvironmentStatusView {
    match value {
        EnvironmentDisplayStatus::Provisioning => EnvironmentStatusView::Provisioning,
        EnvironmentDisplayStatus::Ready => EnvironmentStatusView::Ready,
        EnvironmentDisplayStatus::Paused => EnvironmentStatusView::Paused,
        EnvironmentDisplayStatus::Suspended => EnvironmentStatusView::Suspended,
        EnvironmentDisplayStatus::Stopped => EnvironmentStatusView::Stopped,
        EnvironmentDisplayStatus::Unknown => EnvironmentStatusView::Unknown,
        EnvironmentDisplayStatus::Away => EnvironmentStatusView::Away,
        EnvironmentDisplayStatus::Closing => EnvironmentStatusView::Closing,
        EnvironmentDisplayStatus::Closed => EnvironmentStatusView::Closed,
        EnvironmentDisplayStatus::Failed => EnvironmentStatusView::Failed,
    }
}

fn close_reason_view(value: EnvironmentCloseReason) -> EnvironmentCloseReasonView {
    match value {
        EnvironmentCloseReason::Requested => EnvironmentCloseReasonView::Requested,
        EnvironmentCloseReason::KeyRevoked => EnvironmentCloseReasonView::KeyRevoked,
        EnvironmentCloseReason::EphemeralGrace => EnvironmentCloseReasonView::EphemeralGrace,
        EnvironmentCloseReason::Departed => EnvironmentCloseReasonView::Departed,
        EnvironmentCloseReason::OfflineTimeout => EnvironmentCloseReasonView::OfflineTimeout,
        EnvironmentCloseReason::ProviderEnded => EnvironmentCloseReasonView::ProviderEnded,
    }
}

/// The deployment view of a provider record. The effective data route base
/// is absent only when it cannot be derived.
pub(crate) fn environment_provider_view(
    record: &EnvironmentProviderRecord,
) -> DeploymentEnvironmentProviderView {
    DeploymentEnvironmentProviderView {
        provider_id: record.provider_id.to_string(),
        display_name: record.display_name.clone(),
        controller_endpoint: record.controller_endpoint.as_str().to_owned(),
        data_route_base: record
            .data_route_base
            .as_ref()
            .map(|base| base.as_str().to_owned()),
        effective_data_route_base: record
            .effective_data_route_base()
            .ok()
            .map(|base| base.as_str().to_owned()),
        has_credential: record.has_credential,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

pub(crate) fn map_environments_error(error: EnvironmentRegistryError) -> AgentApiError {
    match error {
        EnvironmentRegistryError::AlreadyExists { kind, id } => {
            AgentApiError::conflict(format!("environment registry {kind} already exists: {id}"))
        }
        EnvironmentRegistryError::NotFound { kind, id } => {
            AgentApiError::not_found(format!("environment registry {kind} not found: {id}"))
        }
        EnvironmentRegistryError::InvalidInput { message } => {
            AgentApiError::invalid_request(message)
        }
        refused @ EnvironmentRegistryError::RegistrationKeyUnavailable { .. } => {
            AgentApiError::rejected(refused.to_string())
        }
        refused @ EnvironmentRegistryError::RegistrationCapacityExhausted { .. } => {
            AgentApiError::rejected(refused.to_string())
        }
        EnvironmentRegistryError::Store { message } => AgentApiError::internal(message),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ::environments::{
        EndpointScheme, EnvironmentEndpoint, EnvironmentProviderBindingStore,
        EnvironmentProviderStore, EnvironmentStore, InMemoryEnvironmentRegistryStore,
        PutEnvironmentProvider, PutEnvironmentProviderBinding,
    };
    use environment_protocol::routes::DataRouteBase;

    use super::*;

    #[test]
    fn the_provider_view_shows_the_effective_data_route_and_no_credential() {
        let record = EnvironmentProviderRecord {
            provider_id: EnvironmentProviderId::new("provider"),
            display_name: None,
            controller_endpoint: EnvironmentEndpoint::parse("wss://provider.test:8443/control")
                .unwrap(),
            data_route_base: None,
            has_credential: false,
            created_at_ms: 1,
            updated_at_ms: 1,
        };
        let view = environment_provider_view(&record);
        assert_eq!(view.data_route_base, None);
        assert_eq!(
            view.effective_data_route_base.as_deref(),
            Some("wss://provider.test:8443/routes")
        );
        assert!(!view.has_credential);
        let local = EnvironmentProviderRecord {
            controller_endpoint: EnvironmentEndpoint::parse("unix:///run/provider.sock").unwrap(),
            data_route_base: Some(DataRouteBase::new("ws://127.0.0.1:9/routes").unwrap()),
            has_credential: true,
            ..record
        };
        assert_eq!(local.controller_endpoint.scheme(), EndpointScheme::Unix);
        let view = environment_provider_view(&local);
        assert_eq!(
            view.effective_data_route_base.as_deref(),
            Some("ws://127.0.0.1:9/routes")
        );
        assert!(view.has_credential);
    }

    #[test]
    fn idle_overrides_are_validated_before_they_reach_a_provider() {
        let valid = EnvironmentIdlePolicyView {
            pause_after_ms: Some(60_000),
            stop_after_ms: Some(3_600_000),
            ..EnvironmentIdlePolicyView::default()
        };
        assert_eq!(
            idle_policy_view(&registry_idle_policy(&valid).unwrap()),
            valid
        );
        let out_of_order = EnvironmentIdlePolicyView {
            pause_after_ms: Some(10),
            stop_after_ms: Some(5),
            ..EnvironmentIdlePolicyView::default()
        };
        assert_eq!(
            registry_idle_policy(&out_of_order).unwrap_err().kind,
            api::AgentApiErrorKind::InvalidRequest
        );
        assert_eq!(
            registry_idle_policy(&EnvironmentIdlePolicyView::default())
                .unwrap_err()
                .kind,
            api::AgentApiErrorKind::InvalidRequest
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_provisioned_view_carries_state_status_and_the_create_inputs() {
        let store = InMemoryEnvironmentRegistryStore::new();
        let provider_id = EnvironmentProviderId::new("provider");
        store
            .put_provider(PutEnvironmentProvider {
                provider_id: provider_id.clone(),
                display_name: None,
                controller_endpoint: EnvironmentEndpoint::parse("ws://provider.test/control")
                    .unwrap(),
                data_route_base: None,
                credential: ::environments::ProviderCredentialChange::Keep,
                updated_at_ms: 1,
            })
            .await
            .unwrap();
        store
            .put_provider_binding(PutEnvironmentProviderBinding {
                universe_id: store.universe_id(),
                provider_id: provider_id.clone(),
                enabled: true,
                updated_at_ms: 1,
            })
            .await
            .unwrap();
        let idle = IdlePolicy {
            pause_after_ms: Some(60_000),
            ..IdlePolicy::default()
        };
        let record = store
            .create_environment(::environments::CreateEnvironment {
                request_id: ::environments::EnvironmentProvisionRequestId::new("request"),
                environment_id: ::environments::EnvironmentId::new("environment"),
                provider_id: provider_id.clone(),
                template_id: ::environments::EnvironmentTemplateId::new("template"),
                idle: Some(idle.clone()),
                display_name: None,
                metadata: BTreeMap::new(),
                created_at_ms: 10,
            })
            .await
            .unwrap();
        let view = environment_view(&record);
        assert_eq!(view.state, EnvironmentStateView::Provisioning);
        assert_eq!(view.status, EnvironmentStatusView::Provisioning);
        assert_eq!(view.request_id.as_deref(), Some("request"));
        assert_eq!(view.close_reason, None);
        match view.source {
            EnvironmentSourceView::Provisioned {
                provider_id,
                template_id,
                target_id,
                idle: shown,
                observed,
                ..
            } => {
                assert_eq!(provider_id, "provider");
                assert_eq!(template_id, "template");
                assert_eq!(target_id, None);
                assert_eq!(shown, Some(idle_policy_view(&idle)));
                assert_eq!(observed, None);
            }
            other => panic!("expected a provisioned source, got {other:?}"),
        }
    }
}
