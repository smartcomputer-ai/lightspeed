//! Provider controllers: dialing a provider's controller, with its bearer
//! credential when one is stored, and classifying what goes wrong.
//!
//! Nothing a controller advertises is stored. Every connection runs the
//! controller handshake through [`ControllerClient::connect_initialized`],
//! and its live `capabilities` gate the optional calls on that connection.
//! The data route base is configuration: the provider record's configured
//! base, or the one derived from its controller endpoint
//! ([`EnvironmentProviderRecord::effective_data_route_base`]).

use super::*;

use ::environments::{
    EnvironmentEndpoint, EnvironmentProviderId, EnvironmentProviderRecord,
    EnvironmentProviderStore, EnvironmentRegistryError,
};
use async_trait::async_trait;
use auth::SecretValue;
use environment_client::{ClientError, ConnectOptions, ControllerClient};
use environment_protocol::{
    control::handshake::ControllerCapabilities, error::ProtocolErrorKind, routes::DataRouteBase,
};
use std::time::Duration;
use thiserror::Error;

/// Deadline of every reconciler controller call. Controllers answer target
/// methods promptly and continue slow work in the background, so this
/// bounds a hung controller, not a slow create; it is short because a hung
/// call holds one of the provider's in-flight rows until it expires.
pub(crate) const CONTROLLER_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Deadline of the synchronous power and ingress calls an API request
/// waits on.
pub(crate) const PASS_THROUGH_TIMEOUT: Duration = Duration::from_secs(30);

/// Options for one controller or provider data-route connection: the
/// provider credential as the bearer token when one is stored (no
/// `Authorization` header otherwise), the default connect and `initialize`
/// bounds, and [`CONTROLLER_REQUEST_TIMEOUT`] on every call.
pub(crate) fn provider_connect_options(credential: Option<&SecretValue>) -> ConnectOptions {
    let options = ConnectOptions::new(super::gateway::runtime_client_info())
        .with_request_timeout(CONTROLLER_REQUEST_TIMEOUT);
    match credential {
        Some(credential) => options.with_bearer_token(credential.expose()),
        None => options,
    }
}

/// Dials a provider controller and runs its handshake. Tests substitute an
/// in-process client.
#[async_trait]
pub(crate) trait ProviderControllerConnector: Send + Sync {
    async fn connect(
        &self,
        endpoint: &EnvironmentEndpoint,
        options: ConnectOptions,
    ) -> Result<ControllerClient, ClientError>;
}

/// The production connector: `ws://`, `wss://` and `unix://` controller
/// URLs through [`ControllerClient::connect_initialized`].
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct WebSocketProviderControllerConnector;

#[async_trait]
impl ProviderControllerConnector for WebSocketProviderControllerConnector {
    async fn connect(
        &self,
        endpoint: &EnvironmentEndpoint,
        options: ConnectOptions,
    ) -> Result<ControllerClient, ClientError> {
        ControllerClient::connect_initialized(endpoint.as_str(), options)
            .await
            .map(|(client, _)| client)
    }
}

/// An optional controller capability the runtime gates calls on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ControllerFeature {
    Power,
    Ingress,
}

impl ControllerFeature {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Power => "power control",
            Self::Ingress => "public ingress",
        }
    }

    pub(crate) fn in_capabilities(self, capabilities: &ControllerCapabilities) -> bool {
        match self {
            Self::Power => capabilities.power,
            Self::Ingress => capabilities.ingress,
        }
    }
}

/// One initialized controller connection, what its handshake advertised,
/// and what its data routes need.
#[derive(Clone)]
pub(crate) struct ProviderController {
    pub(crate) provider_id: EnvironmentProviderId,
    pub(crate) client: ControllerClient,
    /// The live `initialize` capabilities of this connection.
    pub(crate) capabilities: ControllerCapabilities,
    /// The effective data route base.
    pub(crate) data_route_base: DataRouteBase,
    /// The provider credential, presented on its data routes too.
    pub(crate) credential: Option<SecretValue>,
}

impl ProviderController {
    /// Refuse a call that needs an optional capability the controller did
    /// not advertise, before anything is sent.
    pub(crate) fn require(&self, feature: ControllerFeature) -> Result<(), ControllerError> {
        if feature.in_capabilities(&self.capabilities) {
            Ok(())
        } else {
            Err(ControllerError::FeatureUnavailable {
                provider_id: self.provider_id.to_string(),
                feature,
            })
        }
    }

    /// Wrap a failed call of this controller.
    pub(crate) fn call_error(&self, source: ClientError) -> ControllerError {
        ControllerError::Call {
            provider_id: self.provider_id.to_string(),
            source,
        }
    }

    pub(crate) async fn close(&self) {
        let _ = self.client.close().await;
    }
}

/// Connect to a provider's controller with its stored credential, if any.
pub(crate) async fn connect_provider_controller(
    store: &dyn EnvironmentProviderStore,
    connector: &dyn ProviderControllerConnector,
    provider_id: &EnvironmentProviderId,
) -> Result<ProviderController, ControllerError> {
    let provider: EnvironmentProviderRecord = store.read_provider(provider_id).await?;
    let data_route_base = provider.effective_data_route_base()?;
    let credential = store.read_provider_credential(provider_id).await?;
    let client = connector
        .connect(
            &provider.controller_endpoint,
            provider_connect_options(credential.as_ref()),
        )
        .await
        .map_err(|source| ControllerError::Connect {
            provider_id: provider_id.to_string(),
            source,
        })?;
    Ok(ProviderController {
        provider_id: provider_id.clone(),
        capabilities: client.initialize_response().capabilities.clone(),
        client,
        data_route_base,
        credential,
    })
}

/// Check a controller endpoint at provider registration, with the
/// credential the provider will hold. A controller that answers
/// definitively wrong (an HTTP refusal such as 404 for a URL that names no
/// controller or 401 for a wrong or missing credential, or another plane)
/// is refused, so a mis-registered provider never provisions anything. One
/// that cannot be reached right now is accepted.
pub(crate) async fn probe_controller(
    connector: &dyn ProviderControllerConnector,
    endpoint: &EnvironmentEndpoint,
    credential: Option<&SecretValue>,
) -> Result<(), AgentApiError> {
    match connector
        .connect(endpoint, provider_connect_options(credential))
        .await
    {
        Ok(client) => {
            let _ = client.close().await;
            Ok(())
        }
        Err(error) if refuses_registration(&error) => Err(AgentApiError::invalid_request(format!(
            "the provider controller at {endpoint} refused the handshake: {error}"
        ))),
        Err(error) => {
            tracing::warn!(
                target: "temporal_runtime",
                %endpoint,
                %error,
                "provider controller is not reachable at registration; it is registered anyway"
            );
            Ok(())
        }
    }
}

/// Handshake failures that prove the registration wrong rather than the
/// controller temporarily unavailable.
fn refuses_registration(error: &ClientError) -> bool {
    match error {
        ClientError::UpgradeRejected { status, .. } => {
            (400..500).contains(status) && !matches!(status, 408 | 429)
        }
        ClientError::WrongPlane { .. } | ClientError::Configuration { .. } => true,
        _ => false,
    }
}

/// How the reconciler treats a failed controller interaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ControllerDisposition {
    /// May succeed later unchanged: back off and retry, until the
    /// provisioning deadline.
    Transient,
    /// The provider refused the request; repeating it cannot help.
    Refused,
    /// The provider holds no such target.
    Gone,
}

#[derive(Debug, Error)]
pub(crate) enum ControllerError {
    /// Dialing the controller or its `initialize` failed, a version or
    /// plane mismatch included. Always transient: a provider restart or a
    /// rollout that upgrades the runtime first must not fail environments.
    #[error("provider controller {provider_id} is unreachable: {source}")]
    Connect {
        provider_id: String,
        #[source]
        source: ClientError,
    },

    /// A call on an initialized connection failed.
    #[error("provider controller {provider_id}: {source}")]
    Call {
        provider_id: String,
        #[source]
        source: ClientError,
    },

    /// The call needs an optional capability the controller did not
    /// advertise; nothing was sent.
    #[error("provider {provider_id} does not offer {}", feature.as_str())]
    FeatureUnavailable {
        provider_id: String,
        feature: ControllerFeature,
    },

    #[error(transparent)]
    Registry(#[from] EnvironmentRegistryError),
}

impl ControllerError {
    /// Refusals are the controller's typed answers that no retry changes.
    /// Connection failures, deadlines, undecodable answers and retryable or
    /// `internal` answers are transient; `notFound` means the target is
    /// gone. Registry failures are the runtime's own and transient.
    pub(crate) fn disposition(&self) -> ControllerDisposition {
        match self {
            Self::Call { source, .. } => call_disposition(source),
            Self::FeatureUnavailable { .. } => ControllerDisposition::Refused,
            Self::Connect { .. } | Self::Registry(_) => ControllerDisposition::Transient,
        }
    }

    /// The API error for a caller waiting on this controller interaction.
    pub(crate) fn into_api_error(self) -> AgentApiError {
        match self {
            Self::Registry(error) => super::providers::map_environments_error(error),
            other => match other.disposition() {
                ControllerDisposition::Refused => AgentApiError::rejected(other.to_string()),
                ControllerDisposition::Gone => AgentApiError::not_found(other.to_string()),
                ControllerDisposition::Transient => AgentApiError::internal(other.to_string()),
            },
        }
    }
}

/// The disposition of a failed call on an initialized connection.
pub(crate) fn call_disposition(error: &ClientError) -> ControllerDisposition {
    match error {
        ClientError::Protocol(error) => match error.kind() {
            ProtocolErrorKind::NotFound => ControllerDisposition::Gone,
            ProtocolErrorKind::Internal => ControllerDisposition::Transient,
            kind if kind.is_retryable() => ControllerDisposition::Transient,
            _ => ControllerDisposition::Refused,
        },
        _ => ControllerDisposition::Transient,
    }
}

impl EnvironmentService {
    /// A controller connection for one API request; the caller closes it.
    pub(crate) async fn connect_provider_controller(
        &self,
        provider_id: &EnvironmentProviderId,
    ) -> Result<ProviderController, ControllerError> {
        connect_provider_controller(
            self.store.as_ref(),
            self.provider_controller_connector.as_ref(),
            provider_id,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use environment_protocol::{
        error::{PreconditionReason, ProtocolError},
        handshake::Plane,
    };

    fn call(source: ClientError) -> ControllerError {
        ControllerError::Call {
            provider_id: "provider".into(),
            source,
        }
    }

    #[test]
    fn connection_failures_and_contract_breaches_are_transient() {
        for source in [
            ClientError::Closed {
                code: None,
                reason: None,
            },
            ClientError::Transport("reset".into()),
            ClientError::DeadlineExceeded {
                elapsed_ms: 120_000,
            },
            ClientError::Decode("missing field".into()),
            ClientError::InvalidMessage("binary frame".into()),
            ClientError::Protocol(ProtocolError::internal("bug")),
            ClientError::Protocol(ProtocolError::unavailable("incus api down")),
        ] {
            assert_eq!(
                call(source.clone()).disposition(),
                ControllerDisposition::Transient,
                "{source}"
            );
        }
        // Every handshake failure is transient, a version mismatch included.
        for source in [
            ClientError::VersionMismatch {
                plane: Plane::Controller,
                local: 4,
                remote: 5,
            },
            ClientError::UpgradeRejected {
                status: 404,
                refusal: None,
                retry_after_ms: None,
            },
            ClientError::ConnectTimeout {
                endpoint: "ws://provider".into(),
            },
        ] {
            let error = ControllerError::Connect {
                provider_id: "provider".into(),
                source,
            };
            assert_eq!(error.disposition(), ControllerDisposition::Transient);
        }
    }

    #[test]
    fn typed_refusals_and_missing_targets_are_final() {
        let refused = call(ClientError::Protocol(ProtocolError::failed_precondition(
            PreconditionReason::TemplateDeprecated,
            "deprecated",
        )));
        assert_eq!(refused.disposition(), ControllerDisposition::Refused);
        let gone = call(ClientError::Protocol(ProtocolError::not_found("no target")));
        assert_eq!(gone.disposition(), ControllerDisposition::Gone);
        let unadvertised = ControllerError::FeatureUnavailable {
            provider_id: "provider".into(),
            feature: ControllerFeature::Power,
        };
        assert_eq!(unadvertised.disposition(), ControllerDisposition::Refused);
        assert_eq!(
            unadvertised.into_api_error().kind,
            api::AgentApiErrorKind::Rejected
        );
    }

    #[test]
    fn registration_refuses_only_definitive_handshake_failures() {
        let rejected = |status| ClientError::UpgradeRejected {
            status,
            refusal: None,
            retry_after_ms: None,
        };
        for refused in [
            rejected(404),
            rejected(401),
            ClientError::WrongPlane {
                expected: Plane::Controller,
                actual: Plane::Data,
            },
        ] {
            assert!(refuses_registration(&refused), "{refused}");
        }
        for accepted in [
            rejected(503),
            rejected(429),
            ClientError::Connect {
                endpoint: "ws://provider".into(),
                message: "refused".into(),
            },
            ClientError::VersionMismatch {
                plane: Plane::Controller,
                local: 4,
                remote: 5,
            },
        ] {
            assert!(!refuses_registration(&accepted), "{accepted}");
        }
    }

    #[test]
    fn the_bearer_is_sent_only_with_a_credential() {
        assert!(provider_connect_options(None).bearer_token.is_none());
        let credential = SecretValue::new("provider-credential");
        assert_eq!(
            provider_connect_options(Some(&credential))
                .bearer_token
                .as_ref()
                .map(|token| token.expose()),
            Some("provider-credential")
        );
    }

    async fn provider_store(
        endpoint: &str,
        credential: Option<&str>,
    ) -> ::environments::InMemoryEnvironmentRegistryStore {
        let store = ::environments::InMemoryEnvironmentRegistryStore::new();
        store
            .put_provider(::environments::PutEnvironmentProvider {
                provider_id: EnvironmentProviderId::new("provider"),
                display_name: None,
                controller_endpoint: EnvironmentEndpoint::parse(endpoint).unwrap(),
                data_route_base: None,
                credential: match credential {
                    Some(credential) => {
                        ::environments::ProviderCredentialChange::Set(SecretValue::new(credential))
                    }
                    None => ::environments::ProviderCredentialChange::Keep,
                },
                updated_at_ms: 1,
            })
            .await
            .unwrap();
        store
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_connection_carries_live_capabilities_and_the_derived_data_route() {
        let controller = crate::environments::fake_controller::FakeController::start_with(
            crate::environments::fake_controller::FakeControllerOptions {
                power: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let store = provider_store(controller.controller_url(), controller.credential()).await;
        let connected = connect_provider_controller(
            &store,
            &WebSocketProviderControllerConnector,
            &EnvironmentProviderId::new("provider"),
        )
        .await
        .expect("connect");
        assert!(!connected.capabilities.power && connected.capabilities.ingress);
        assert!(matches!(
            connected.require(ControllerFeature::Power),
            Err(ControllerError::FeatureUnavailable { .. })
        ));
        assert!(
            connected
                .data_route_base
                .as_str()
                .ends_with(environment_protocol::routes::DEFAULT_DATA_ROUTE_PATH)
        );
        connected.close().await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_provider_without_a_credential_connects_without_a_bearer() {
        let controller = crate::environments::fake_controller::FakeController::start_with(
            crate::environments::fake_controller::FakeControllerOptions {
                credential: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(controller.credential().is_none());
        let store = provider_store(controller.controller_url(), None).await;
        let connected = connect_provider_controller(
            &store,
            &WebSocketProviderControllerConnector,
            &EnvironmentProviderId::new("provider"),
        )
        .await
        .expect("connect without a credential");
        assert!(connected.credential.is_none());
        connected.close().await;
        // A controller that requires a credential refuses a provider that
        // has none, and registration says so.
        let guarded = crate::environments::fake_controller::FakeController::start()
            .await
            .unwrap();
        let refused = probe_controller(
            &WebSocketProviderControllerConnector,
            &EnvironmentEndpoint::parse(guarded.controller_url()).unwrap(),
            None,
        )
        .await
        .expect_err("a missing credential is a definitive refusal");
        assert_eq!(refused.kind, api::AgentApiErrorKind::InvalidRequest);
    }
}
