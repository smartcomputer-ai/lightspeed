use super::*;

use ::environments::EnvironmentConnectionSpec;
use async_trait::async_trait;
use environment_client::{
    EnvironmentClientError, EnvironmentProviderClient, WebSocketConnectOptions,
};
use environment_protocol::{
    control::{
        handshake::{ControllerInitializeParams, ControllerInitializeResponse},
        ingress::{
            EnsureIngressParams, IngressResponse, ProviderIngressStatus, RemoveIngressParams,
        },
        targets::{
            AdoptTargetParams, AdoptTargetResponse, CloseTargetParams, CloseTargetResponse,
            CreateTargetParams, CreateTargetResponse, EnvironmentTemplate, ListTemplatesParams,
            ListTemplatesResponse, PowerState, ProviderTargetStatus, ProviderTargetSummary,
            SetTargetPowerParams, SetTargetPowerResponse,
        },
    },
    shared::{
        CURRENT_PROTOCOL_VERSION, EnvironmentCapabilities, EnvironmentScope, EnvironmentTransport,
        ImplementationInfo, ProviderTargetId,
    },
};

use std::{
    collections::{BTreeMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    sync::Arc,
};

#[async_trait]
pub(crate) trait ProviderController: Send {
    async fn close(&mut self) -> Result<(), AgentApiError> {
        Ok(())
    }

    async fn initialize(
        &mut self,
        params: &ControllerInitializeParams,
    ) -> Result<ControllerInitializeResponse, AgentApiError>;

    async fn create_target(
        &mut self,
        params: &CreateTargetParams,
    ) -> Result<CreateTargetResponse, AgentApiError>;

    async fn adopt_target(
        &mut self,
        params: &AdoptTargetParams,
    ) -> Result<AdoptTargetResponse, AgentApiError>;

    async fn list_templates(
        &mut self,
        params: &ListTemplatesParams,
    ) -> Result<ListTemplatesResponse, AgentApiError>;

    async fn close_target(
        &mut self,
        params: &CloseTargetParams,
    ) -> Result<CloseTargetResponse, AgentApiError>;
    async fn set_target_power(
        &mut self,
        params: &SetTargetPowerParams,
    ) -> Result<SetTargetPowerResponse, AgentApiError>;
    async fn ensure_ingress(
        &mut self,
        params: &EnsureIngressParams,
    ) -> Result<IngressResponse, AgentApiError>;
    async fn remove_ingress(
        &mut self,
        params: &RemoveIngressParams,
    ) -> Result<IngressResponse, AgentApiError>;
}

#[async_trait]
pub(crate) trait ProviderControllerConnector: Send + Sync {
    async fn connect(
        &self,
        connection: &EnvironmentConnectionSpec,
    ) -> Result<Box<dyn ProviderController>, AgentApiError>;
}

#[derive(Default)]
pub(crate) struct WebSocketProviderControllerConnector {
    fake_backend: Arc<tokio::sync::Mutex<FakeBackend>>,
}

#[async_trait]
impl ProviderControllerConnector for WebSocketProviderControllerConnector {
    async fn connect(
        &self,
        connection: &EnvironmentConnectionSpec,
    ) -> Result<Box<dyn ProviderController>, AgentApiError> {
        let mut controller: Box<dyn ProviderController> = match &connection.transport {
            EnvironmentTransport::WebSocket => {
                let client = EnvironmentProviderClient::connect(
                    &connection.endpoint,
                    WebSocketConnectOptions {
                        bearer_token: None,
                        user_agent: Some("lightspeed-temporal-server".to_owned()),
                        headers: Vec::new(),
                    },
                )
                .await
                .map_err(map_environment_client_error)?;
                Box::new(client)
            }
            EnvironmentTransport::Http => return Err(unsupported_transport("http")),
            EnvironmentTransport::Stdio => return Err(unsupported_transport("stdio")),
            EnvironmentTransport::Ssh => return Err(unsupported_transport("ssh")),
            EnvironmentTransport::Provider { provider_type } => {
                if provider_type == "fake" {
                    Box::new(FakeProviderController {
                        backend: self.fake_backend.clone(),
                    })
                } else {
                    return Err(unsupported_transport(format!("provider:{provider_type}")));
                }
            }
        };
        let initialized = controller
            .initialize(&ControllerInitializeParams {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                client_name: "lightspeed-temporal-server".to_owned(),
            })
            .await;
        let initialized = match initialized {
            Ok(initialized) => initialized,
            Err(error) => {
                let _ = controller.close().await;
                return Err(error);
            }
        };
        if initialized.protocol_version != CURRENT_PROTOCOL_VERSION {
            let _ = controller.close().await;
            return Err(AgentApiError::rejected(format!(
                "environment provider protocol version {} is incompatible with {}",
                initialized.protocol_version, CURRENT_PROTOCOL_VERSION,
            )));
        }
        Ok(controller)
    }
}

/// Power states the in-process fake provider advertises. Suspended is
/// deliberately included so the Firecracker-shaped path has coverage.
const FAKE_POWER_STATES: [PowerState; 4] = [
    PowerState::Running,
    PowerState::Paused,
    PowerState::Suspended,
    PowerState::Stopped,
];

#[derive(Default)]
struct FakeBackend {
    targets: BTreeMap<String, FakeTarget>,
    requests: BTreeMap<(String, String, String), String>,
}

#[derive(Clone)]
struct FakeTarget {
    universe_id: String,
    binding_id: String,
    environment_id: String,
    incarnation_id: String,
    response: CreateTargetResponse,
}

struct FakeProviderController {
    backend: Arc<tokio::sync::Mutex<FakeBackend>>,
}

#[async_trait]
impl ProviderController for FakeProviderController {
    async fn initialize(
        &mut self,
        _params: &ControllerInitializeParams,
    ) -> Result<ControllerInitializeResponse, AgentApiError> {
        Ok(ControllerInitializeResponse {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            capabilities: environment_protocol::control::handshake::ControllerCapabilities {
                list_templates: true,
                list_targets: true,
                create_target: true,
                adopt_target: true,
                get_target: true,
                close_target: true,
                set_target_power: true,
                ingress: true,
            },
            implementation: ImplementationInfo {
                name: "lightspeed-fake-environment-provider".to_owned(),
                version: Some(env!("CARGO_PKG_VERSION").to_owned()),
                ..Default::default()
            },
        })
    }

    async fn list_templates(
        &mut self,
        _params: &ListTemplatesParams,
    ) -> Result<ListTemplatesResponse, AgentApiError> {
        Ok(ListTemplatesResponse {
            templates: vec![EnvironmentTemplate {
                template_id: "rust-v1".to_owned(),
                display_name: "Rust development".to_owned(),
                description: Some("Deterministic fake provider template".to_owned()),
                public_ingress: false,
                deprecated: false,
                metadata: BTreeMap::new(),
            }],
        })
    }

    async fn create_target(
        &mut self,
        params: &CreateTargetParams,
    ) -> Result<CreateTargetResponse, AgentApiError> {
        if params.template_id != "rust-v1" && !params.template_id.starts_with("adopted:") {
            return Err(AgentApiError::rejected(
                "fake provider policy rejected the template",
            ));
        }
        let key = (
            params.binding.universe_id.clone(),
            params.binding.binding_id.clone(),
            params.request_id.clone(),
        );
        let mut backend = self.backend.lock().await;
        if let Some(target_id) = backend.requests.get(&key) {
            return Ok(backend
                .targets
                .get(target_id)
                .expect("fake request index target")
                .response
                .clone());
        }
        if backend.targets.values().any(|target| {
            target.universe_id == params.binding.universe_id
                && target.binding_id == params.binding.binding_id
                && target.response.target.status != ProviderTargetStatus::Closed
        }) {
            return Err(AgentApiError::rejected(
                "fake provider policy allows one active target per binding",
            ));
        }
        let mut identity = DefaultHasher::new();
        params.binding.universe_id.hash(&mut identity);
        params.binding.binding_id.hash(&mut identity);
        params.incarnation_id.hash(&mut identity);
        let target_id = format!("fake-{:016x}", identity.finish());
        let provider_target_id = ProviderTargetId::new(target_id.clone());
        let capabilities = EnvironmentCapabilities::filesystem(true, true).with_process();
        let response = CreateTargetResponse {
            target: ProviderTargetSummary {
                target_id: provider_target_id.clone(),
                display_name: Some(params.environment_id.clone()),
                status: ProviderTargetStatus::Ready,
                scope: EnvironmentScope::Default,
                capabilities: capabilities.clone(),
                default_cwd: None,
                power_states: FAKE_POWER_STATES.to_vec(),
                metadata: BTreeMap::from([(
                    "imageFingerprint".to_owned(),
                    format!("fake:{}", params.template_id),
                )]),
            },
        };
        backend.requests.insert(key, target_id.clone());
        backend.targets.insert(
            target_id,
            FakeTarget {
                universe_id: params.binding.universe_id.clone(),
                binding_id: params.binding.binding_id.clone(),
                environment_id: params.environment_id.clone(),
                incarnation_id: params.incarnation_id.clone(),
                response: response.clone(),
            },
        );
        Ok(response)
    }

    async fn adopt_target(
        &mut self,
        params: &AdoptTargetParams,
    ) -> Result<AdoptTargetResponse, AgentApiError> {
        let create = CreateTargetParams {
            request_id: params.request_id.clone(),
            environment_id: params.environment_id.clone(),
            incarnation_id: params.incarnation_id.clone(),
            binding: params.binding.clone(),
            template_id: format!("adopted:{}", params.source_target),
        };
        let response = self.create_target(&create).await?;
        Ok(AdoptTargetResponse {
            target: response.target,
        })
    }

    async fn close_target(
        &mut self,
        params: &CloseTargetParams,
    ) -> Result<CloseTargetResponse, AgentApiError> {
        let mut backend = self.backend.lock().await;
        let target = backend
            .targets
            .get_mut(params.target_id.as_str())
            .ok_or_else(|| {
                AgentApiError::not_found(format!("fake target not found: {}", params.target_id))
            })?;
        if target.universe_id != params.binding.universe_id
            || target.binding_id != params.binding.binding_id
            || target.environment_id != params.environment_id
            || target.incarnation_id != params.incarnation_id
        {
            return Err(AgentApiError::rejected(
                "fake target ownership metadata does not match close request",
            ));
        }
        target.response.target.status = ProviderTargetStatus::Closed;
        Ok(CloseTargetResponse {
            target_id: params.target_id.clone(),
            status: ProviderTargetStatus::Closed,
        })
    }

    /// The fake provider converges instantly: the requested steady state is
    /// the observed state on return.
    async fn set_target_power(
        &mut self,
        params: &SetTargetPowerParams,
    ) -> Result<SetTargetPowerResponse, AgentApiError> {
        if !FAKE_POWER_STATES.contains(&params.power) {
            return Err(AgentApiError::rejected(format!(
                "fake provider does not support power state {}",
                params.power
            )));
        }
        let mut backend = self.backend.lock().await;
        let target = backend
            .targets
            .get_mut(params.target_id.as_str())
            .ok_or_else(|| {
                AgentApiError::not_found(format!("fake target not found: {}", params.target_id))
            })?;
        if target.universe_id != params.binding.universe_id
            || target.binding_id != params.binding.binding_id
            || target.environment_id != params.environment_id
            || target.incarnation_id != params.incarnation_id
        {
            return Err(AgentApiError::rejected(
                "fake target ownership metadata does not match power request",
            ));
        }
        if target.response.target.status == ProviderTargetStatus::Closed {
            return Err(AgentApiError::rejected("fake target is closed"));
        }
        target.response.target.status = match params.power {
            PowerState::Running => ProviderTargetStatus::Ready,
            PowerState::Paused => ProviderTargetStatus::Paused,
            PowerState::Suspended => ProviderTargetStatus::Suspended,
            PowerState::Stopped => ProviderTargetStatus::Stopped,
        };
        Ok(SetTargetPowerResponse {
            target: target.response.target.clone(),
        })
    }

    async fn ensure_ingress(
        &mut self,
        _params: &EnsureIngressParams,
    ) -> Result<IngressResponse, AgentApiError> {
        Ok(IngressResponse {
            status: ProviderIngressStatus::Ready,
            public_endpoint: Some("https://fake.env.test".to_owned()),
        })
    }

    async fn remove_ingress(
        &mut self,
        _params: &RemoveIngressParams,
    ) -> Result<IngressResponse, AgentApiError> {
        Ok(IngressResponse {
            status: ProviderIngressStatus::Disabled,
            public_endpoint: None,
        })
    }
}

#[async_trait]
impl<T> ProviderController for EnvironmentProviderClient<T>
where
    T: environment_client::JsonRpcTransport + Send,
{
    async fn close(&mut self) -> Result<(), AgentApiError> {
        EnvironmentProviderClient::close(self)
            .await
            .map_err(map_environment_client_error)
    }

    async fn initialize(
        &mut self,
        params: &ControllerInitializeParams,
    ) -> Result<ControllerInitializeResponse, AgentApiError> {
        EnvironmentProviderClient::initialize(self, params)
            .await
            .map_err(map_environment_client_error)
    }

    async fn create_target(
        &mut self,
        params: &CreateTargetParams,
    ) -> Result<CreateTargetResponse, AgentApiError> {
        EnvironmentProviderClient::create_target(self, params)
            .await
            .map_err(map_environment_client_error)
    }

    async fn adopt_target(
        &mut self,
        params: &AdoptTargetParams,
    ) -> Result<AdoptTargetResponse, AgentApiError> {
        EnvironmentProviderClient::adopt_target(self, params)
            .await
            .map_err(map_environment_client_error)
    }

    async fn list_templates(
        &mut self,
        params: &ListTemplatesParams,
    ) -> Result<ListTemplatesResponse, AgentApiError> {
        EnvironmentProviderClient::list_templates(self, params)
            .await
            .map_err(map_environment_client_error)
    }

    async fn close_target(
        &mut self,
        params: &CloseTargetParams,
    ) -> Result<CloseTargetResponse, AgentApiError> {
        EnvironmentProviderClient::close_target(self, params)
            .await
            .map_err(map_environment_client_error)
    }

    async fn set_target_power(
        &mut self,
        params: &SetTargetPowerParams,
    ) -> Result<SetTargetPowerResponse, AgentApiError> {
        EnvironmentProviderClient::set_target_power(self, params)
            .await
            .map_err(map_environment_client_error)
    }

    async fn ensure_ingress(
        &mut self,
        params: &EnsureIngressParams,
    ) -> Result<IngressResponse, AgentApiError> {
        EnvironmentProviderClient::ensure_ingress(self, params)
            .await
            .map_err(map_environment_client_error)
    }

    async fn remove_ingress(
        &mut self,
        params: &RemoveIngressParams,
    ) -> Result<IngressResponse, AgentApiError> {
        EnvironmentProviderClient::remove_ingress(self, params)
            .await
            .map_err(map_environment_client_error)
    }
}

/// Finish one scoped provider-controller operation and close its transport on
/// both success and failure. A close error does not replace the operation's
/// result: controller calls are already complete when this runs.
pub(crate) async fn finish_provider_controller<T>(
    mut controller: Box<dyn ProviderController>,
    result: Result<T, AgentApiError>,
) -> Result<T, AgentApiError> {
    let _ = controller.close().await;
    result
}

pub(crate) fn map_environment_client_error(error: EnvironmentClientError) -> AgentApiError {
    match error {
        EnvironmentClientError::Protocol(error) => {
            AgentApiError::rejected(format!("provider controller error: {}", error.message))
        }
        EnvironmentClientError::TransportClosed => {
            AgentApiError::rejected("provider controller disconnected")
        }
        EnvironmentClientError::Serialize(error) => {
            AgentApiError::rejected(format!("provider controller call failed: {error}"))
        }
        EnvironmentClientError::Transport(message)
        | EnvironmentClientError::InvalidMessage(message) => {
            AgentApiError::rejected(format!("provider controller call failed: {message}"))
        }
    }
}

fn unsupported_transport(transport: impl std::fmt::Display) -> AgentApiError {
    AgentApiError::invalid_request(format!(
        "provider controller transport is not supported by this gateway: {transport}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::environments::EnvironmentConnectionSpec;
    use environment_client::{EnvironmentClientResult, JsonRpcTransport};
    use environment_protocol::control::targets::ProviderBindingContext;
    use serde_json::Value;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct CloseTrackingTransport {
        closed: Arc<AtomicBool>,
    }

    #[async_trait]
    impl JsonRpcTransport for CloseTrackingTransport {
        async fn send(&mut self, _message: Value) -> EnvironmentClientResult<()> {
            Ok(())
        }

        async fn recv(&mut self) -> EnvironmentClientResult<Option<Value>> {
            Ok(None)
        }

        async fn close(&mut self) -> EnvironmentClientResult<()> {
            self.closed.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    fn connection() -> EnvironmentConnectionSpec {
        EnvironmentConnectionSpec::new(
            "in-process",
            EnvironmentTransport::Provider {
                provider_type: "fake".to_owned(),
            },
        )
    }

    fn create(
        universe: &str,
        binding: &str,
        request: &str,
        environment: &str,
    ) -> CreateTargetParams {
        CreateTargetParams {
            request_id: request.to_owned(),
            environment_id: environment.to_owned(),
            incarnation_id: format!("incarnation-{environment}"),
            binding: ProviderBindingContext {
                universe_id: universe.to_owned(),
                binding_id: binding.to_owned(),
            },
            template_id: "rust-v1".to_owned(),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finishing_a_provider_operation_closes_its_transport() {
        let closed = Arc::new(AtomicBool::new(false));
        let controller: Box<dyn ProviderController> =
            Box::new(EnvironmentProviderClient::new(CloseTrackingTransport {
                closed: closed.clone(),
            }));

        let value = finish_provider_controller(controller, Ok::<_, AgentApiError>(42))
            .await
            .expect("operation result");

        assert_eq!(value, 42);
        assert!(closed.load(Ordering::SeqCst));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fake_provider_adopts_retries_and_isolates_bindings() {
        let connector = WebSocketProviderControllerConnector::default();
        let mut first = connector.connect(&connection()).await.expect("connect");
        let created = first
            .create_target(&create(
                "universe-a",
                "binding-a",
                "request-1",
                "environment-1",
            ))
            .await
            .expect("create");
        drop(first);

        let mut after_restart = connector.connect(&connection()).await.expect("reconnect");
        let adopted = after_restart
            .create_target(&create(
                "universe-a",
                "binding-a",
                "request-1",
                "different-environment",
            ))
            .await
            .expect("adopt");
        assert_eq!(adopted.target.target_id, created.target.target_id);

        let isolated = after_restart
            .create_target(&create(
                "universe-b",
                "binding-b",
                "request-1",
                "environment-2",
            ))
            .await
            .expect("isolated create");
        assert_ne!(isolated.target.target_id, created.target.target_id);

        let imported = after_restart
            .adopt_target(&AdoptTargetParams {
                request_id: "adopt-request-1".to_owned(),
                environment_id: "environment-imported".to_owned(),
                incarnation_id: "incarnation-imported".to_owned(),
                binding: ProviderBindingContext {
                    universe_id: "universe-c".to_owned(),
                    binding_id: "binding-c".to_owned(),
                },
                source_target: "legacy/hand-built-vm".to_owned(),
            })
            .await
            .expect("explicit adoption");
        assert_eq!(
            imported.target.metadata.get("imageFingerprint"),
            Some(&"fake:adopted:legacy/hand-built-vm".to_owned())
        );

        let wrong_owner = after_restart
            .close_target(&CloseTargetParams {
                request_id: "close-1".to_owned(),
                environment_id: "environment-1".to_owned(),
                incarnation_id: "incarnation-environment-1".to_owned(),
                binding: ProviderBindingContext {
                    universe_id: "universe-b".to_owned(),
                    binding_id: "binding-b".to_owned(),
                },
                target_id: created.target.target_id.clone(),
                force: false,
            })
            .await
            .expect_err("ownership mismatch");
        assert_eq!(wrong_owner.kind, AgentApiErrorKind::Rejected);

        let power = |power: PowerState| SetTargetPowerParams {
            request_id: format!("power-{power}"),
            environment_id: "environment-1".to_owned(),
            incarnation_id: "incarnation-environment-1".to_owned(),
            binding: ProviderBindingContext {
                universe_id: "universe-a".to_owned(),
                binding_id: "binding-a".to_owned(),
            },
            target_id: created.target.target_id.clone(),
            power,
        };
        assert_eq!(created.target.power_states, FAKE_POWER_STATES.to_vec());
        let paused = after_restart
            .set_target_power(&power(PowerState::Paused))
            .await
            .expect("pause");
        assert_eq!(paused.target.status, ProviderTargetStatus::Paused);
        let resumed = after_restart
            .set_target_power(&power(PowerState::Running))
            .await
            .expect("resume");
        assert_eq!(resumed.target.status, ProviderTargetStatus::Ready);
        assert_eq!(resumed.target.target_id, created.target.target_id);

        let closed = after_restart
            .close_target(&CloseTargetParams {
                request_id: "close-1".to_owned(),
                environment_id: "environment-1".to_owned(),
                incarnation_id: "incarnation-environment-1".to_owned(),
                binding: ProviderBindingContext {
                    universe_id: "universe-a".to_owned(),
                    binding_id: "binding-a".to_owned(),
                },
                target_id: created.target.target_id.clone(),
                force: false,
            })
            .await
            .expect("close");
        assert_eq!(closed.status, ProviderTargetStatus::Closed);
        assert!(
            after_restart
                .set_target_power(&power(PowerState::Running))
                .await
                .is_err()
        );
    }
}
