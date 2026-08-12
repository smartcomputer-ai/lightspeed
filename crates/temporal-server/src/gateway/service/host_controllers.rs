use super::*;

use ::environments::HostControllerConnectionSpec;
use async_trait::async_trait;
use host_client::{HostClientError, HostControllerClient, WebSocketConnectOptions};
use host_protocol::{
    control::{
        handshake::{ControllerInitializeParams, ControllerInitializeResponse},
        targets::{
            CloseTargetParams, CloseTargetResponse, CreateTargetParams, CreateTargetResponse,
            EnvironmentTemplate, HostTargetStatus, HostTargetSummary, ListTemplatesParams,
            ListTemplatesResponse,
        },
    },
    shared::{
        CURRENT_PROTOCOL_VERSION, HostCapabilities, HostScope, HostTargetId, HostTransport,
        ImplementationInfo,
    },
};

use std::{
    collections::{BTreeMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    sync::Arc,
};

#[async_trait]
pub(crate) trait HostController: Send {
    async fn initialize(
        &mut self,
        params: &ControllerInitializeParams,
    ) -> Result<ControllerInitializeResponse, AgentApiError>;

    async fn create_target(
        &mut self,
        params: &CreateTargetParams,
    ) -> Result<CreateTargetResponse, AgentApiError>;

    async fn list_templates(
        &mut self,
        params: &ListTemplatesParams,
    ) -> Result<ListTemplatesResponse, AgentApiError>;

    async fn close_target(
        &mut self,
        params: &CloseTargetParams,
    ) -> Result<CloseTargetResponse, AgentApiError>;
}

#[async_trait]
pub(crate) trait HostControllerConnector: Send + Sync {
    async fn connect(
        &self,
        connection: &HostControllerConnectionSpec,
    ) -> Result<Box<dyn HostController>, AgentApiError>;
}

#[derive(Default)]
pub(super) struct WebSocketHostControllerConnector {
    fake_backend: Arc<tokio::sync::Mutex<FakeBackend>>,
}

#[async_trait]
impl HostControllerConnector for WebSocketHostControllerConnector {
    async fn connect(
        &self,
        connection: &HostControllerConnectionSpec,
    ) -> Result<Box<dyn HostController>, AgentApiError> {
        let mut controller: Box<dyn HostController> = match &connection.transport {
            HostTransport::WebSocket => {
                let client = HostControllerClient::connect(
                    &connection.endpoint,
                    WebSocketConnectOptions {
                        user_agent: Some("lightspeed-temporal-server".to_owned()),
                        ..WebSocketConnectOptions::default()
                    },
                )
                .await
                .map_err(map_host_client_error)?;
                Box::new(client)
            }
            HostTransport::Http => return Err(unsupported_transport("http")),
            HostTransport::Stdio => return Err(unsupported_transport("stdio")),
            HostTransport::Ssh => return Err(unsupported_transport("ssh")),
            HostTransport::Provider { provider_type } => {
                if provider_type == "fake" {
                    Box::new(FakeHostController {
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
            .await?;
        if initialized.protocol_version != CURRENT_PROTOCOL_VERSION {
            return Err(AgentApiError::rejected(format!(
                "environment provider protocol version {} is incompatible with {}",
                initialized.protocol_version, CURRENT_PROTOCOL_VERSION,
            )));
        }
        Ok(controller)
    }
}

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

struct FakeHostController {
    backend: Arc<tokio::sync::Mutex<FakeBackend>>,
}

#[async_trait]
impl HostController for FakeHostController {
    async fn initialize(
        &mut self,
        _params: &ControllerInitializeParams,
    ) -> Result<ControllerInitializeResponse, AgentApiError> {
        Ok(ControllerInitializeResponse {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            capabilities: host_protocol::control::handshake::ControllerCapabilities {
                list_templates: true,
                list_targets: true,
                create_target: true,
                attach_target: false,
                get_target: true,
                close_target: true,
            },
            implementation: ImplementationInfo {
                name: "lightspeed-fake-environment-provider".to_owned(),
                version: Some(env!("CARGO_PKG_VERSION").to_owned()),
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
                description: Some("Deterministic P118 fake provider template".to_owned()),
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
        if params.template_id != "rust-v1" {
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
                && target.response.target.status != HostTargetStatus::Closed
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
        let host_target_id = HostTargetId::new(target_id.clone());
        let capabilities = HostCapabilities::filesystem(true, true).with_process();
        let response = CreateTargetResponse {
            target: HostTargetSummary {
                target_id: host_target_id.clone(),
                display_name: Some(params.environment_id.clone()),
                status: HostTargetStatus::Ready,
                scope: HostScope::Default,
                capabilities: capabilities.clone(),
                default_cwd: None,
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
        target.response.target.status = HostTargetStatus::Closed;
        Ok(CloseTargetResponse {
            target_id: params.target_id.clone(),
            status: HostTargetStatus::Closed,
        })
    }
}

#[async_trait]
impl<T> HostController for HostControllerClient<T>
where
    T: host_client::JsonRpcTransport + Send,
{
    async fn initialize(
        &mut self,
        params: &ControllerInitializeParams,
    ) -> Result<ControllerInitializeResponse, AgentApiError> {
        HostControllerClient::initialize(self, params)
            .await
            .map_err(map_host_client_error)
    }

    async fn create_target(
        &mut self,
        params: &CreateTargetParams,
    ) -> Result<CreateTargetResponse, AgentApiError> {
        HostControllerClient::create_target(self, params)
            .await
            .map_err(map_host_client_error)
    }

    async fn list_templates(
        &mut self,
        params: &ListTemplatesParams,
    ) -> Result<ListTemplatesResponse, AgentApiError> {
        HostControllerClient::list_templates(self, params)
            .await
            .map_err(map_host_client_error)
    }

    async fn close_target(
        &mut self,
        params: &CloseTargetParams,
    ) -> Result<CloseTargetResponse, AgentApiError> {
        HostControllerClient::close_target(self, params)
            .await
            .map_err(map_host_client_error)
    }
}

pub(super) fn map_host_client_error(error: HostClientError) -> AgentApiError {
    match error {
        HostClientError::Host(error) => {
            AgentApiError::rejected(format!("host controller error: {}", error.message))
        }
        HostClientError::TransportClosed => AgentApiError::rejected("host controller disconnected"),
        HostClientError::Serialize(error) => {
            AgentApiError::rejected(format!("host controller call failed: {error}"))
        }
        HostClientError::Transport(message) | HostClientError::InvalidMessage(message) => {
            AgentApiError::rejected(format!("host controller call failed: {message}"))
        }
    }
}

fn unsupported_transport(transport: impl std::fmt::Display) -> AgentApiError {
    AgentApiError::invalid_request(format!(
        "host controller transport is not supported by this gateway: {transport}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::environments::HostControllerConnectionSpec;
    use host_protocol::control::targets::ProviderBindingContext;

    fn connection() -> HostControllerConnectionSpec {
        HostControllerConnectionSpec::new(
            "in-process",
            HostTransport::Provider {
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
    async fn fake_provider_adopts_retries_and_isolates_bindings() {
        let connector = WebSocketHostControllerConnector::default();
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

        let closed = after_restart
            .close_target(&CloseTargetParams {
                request_id: "close-1".to_owned(),
                environment_id: "environment-1".to_owned(),
                incarnation_id: "incarnation-environment-1".to_owned(),
                binding: ProviderBindingContext {
                    universe_id: "universe-a".to_owned(),
                    binding_id: "binding-a".to_owned(),
                },
                target_id: created.target.target_id,
                force: false,
            })
            .await
            .expect("close");
        assert_eq!(closed.status, HostTargetStatus::Closed);
    }
}
