use super::*;

use ::environments::HostControllerConnectionSpec;
use async_trait::async_trait;
use host_client::{HostClientError, HostControllerClient, WebSocketConnectOptions};
use host_protocol::{
    control::{
        handshake::{ControllerInitializeParams, ControllerInitializeResponse},
        targets::{
            CloseTargetParams, CloseTargetResponse, CreateTargetParams, CreateTargetResponse,
        },
    },
    shared::HostTransport,
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

pub(super) struct WebSocketHostControllerConnector;

#[async_trait]
impl HostControllerConnector for WebSocketHostControllerConnector {
    async fn connect(
        &self,
        connection: &HostControllerConnectionSpec,
    ) -> Result<Box<dyn HostController>, AgentApiError> {
        match &connection.transport {
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
                Ok(Box::new(client))
            }
            HostTransport::Http => Err(unsupported_transport("http")),
            HostTransport::Stdio => Err(unsupported_transport("stdio")),
            HostTransport::Ssh => Err(unsupported_transport("ssh")),
            HostTransport::Provider { provider_type } => {
                Err(unsupported_transport(format!("provider:{provider_type}")))
            }
        }
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
