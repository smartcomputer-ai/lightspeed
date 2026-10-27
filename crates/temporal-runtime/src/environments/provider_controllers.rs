//! Provider controllers: dialing a provider's controller with its
//! credential, recording what its `initialize` advertises, and classifying
//! what goes wrong.
//!
//! Every connection runs the controller handshake through
//! [`ControllerClient::connect_initialized`]. At each successful handshake
//! the advertised `routes.data` passes the downgrade rule and, together with
//! the optional features, is recorded on the provider record; the gateway's
//! provider data proxy and the reconciler's data-plane handshake build
//! every target's data route from that recorded base, never from the
//! controller URL.

use super::*;

use ::environments::{
    ControllerFeatures, EndpointScheme, EnvironmentEndpoint, EnvironmentProviderId,
    EnvironmentProviderStore, EnvironmentRegistryError, ListEnvironmentProviders,
    RecordProviderController,
};
use async_trait::async_trait;
use auth::SecretValue;
use environment_client::{ClientError, ConnectOptions, ControllerClient};
use environment_protocol::{
    control::handshake::ControllerInitializeResponse,
    error::{ErrorData, PreconditionReason, ProtocolErrorKind},
    routes::{DataRouteBase, UrlError},
};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use thiserror::Error;

/// Deadline of every controller call. Controllers answer target methods
/// promptly and continue slow work in the background, so this bounds a
/// hung controller, not a slow create.
pub(crate) const CONTROLLER_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Options for one controller or provider data-route connection: the
/// provider credential as the bearer token, the default connect and
/// `initialize` bounds, and [`CONTROLLER_REQUEST_TIMEOUT`] on every call.
pub(crate) fn provider_connect_options(credential: &SecretValue) -> ConnectOptions {
    ConnectOptions::new(super::gateway::runtime_client_info())
        .with_bearer_token(credential.expose())
        .with_request_timeout(CONTROLLER_REQUEST_TIMEOUT)
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

/// The facts a controller's `initialize` advertises, checked: the data
/// route base must not downgrade a `wss` controller to `ws`. A controller
/// on a Unix socket is local, so any base passes.
pub(crate) fn controller_facts(
    endpoint: &EnvironmentEndpoint,
    initialize: &ControllerInitializeResponse,
) -> Result<(ControllerFeatures, DataRouteBase), UrlError> {
    if endpoint.scheme() != EndpointScheme::Unix {
        initialize
            .routes
            .data
            .check_for_controller(endpoint.as_str())?;
    }
    Ok((
        ControllerFeatures {
            adopt_target: initialize.capabilities.adopt_target,
            ingress: initialize.capabilities.ingress,
        },
        initialize.routes.data.clone(),
    ))
}

/// An optional controller feature the runtime gates calls on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ControllerFeature {
    AdoptTarget,
    Ingress,
}

impl ControllerFeature {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::AdoptTarget => "target adoption",
            Self::Ingress => "public ingress",
        }
    }

    pub(crate) fn in_features(self, features: &ControllerFeatures) -> bool {
        match self {
            Self::AdoptTarget => features.adopt_target,
            Self::Ingress => features.ingress,
        }
    }
}

/// One initialized controller connection and what its handshake advertised.
#[derive(Clone)]
pub(crate) struct ProviderController {
    pub(crate) provider_id: EnvironmentProviderId,
    pub(crate) client: ControllerClient,
    pub(crate) features: ControllerFeatures,
    pub(crate) data_route_base: DataRouteBase,
    /// The provider credential, presented on its data routes too.
    pub(crate) credential: SecretValue,
}

impl ProviderController {
    /// Refuse a call that needs an optional feature the controller did not
    /// advertise, before anything is sent.
    pub(crate) fn require(&self, feature: ControllerFeature) -> Result<(), ControllerError> {
        if feature.in_features(&self.features) {
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

/// Connect to a provider's controller with its stored credential, check the
/// advertised facts and record them on the provider record when they
/// changed.
pub(crate) async fn connect_provider_controller(
    store: &dyn EnvironmentProviderStore,
    connector: &dyn ProviderControllerConnector,
    provider_id: &EnvironmentProviderId,
) -> Result<ProviderController, ControllerError> {
    let provider = store.read_provider(provider_id).await?;
    let credential = store.read_provider_credential(provider_id).await?;
    let client = connector
        .connect(
            &provider.controller_endpoint,
            provider_connect_options(&credential),
        )
        .await
        .map_err(|source| ControllerError::Connect {
            provider_id: provider_id.to_string(),
            source,
        })?;
    let (features, data_route_base) =
        match controller_facts(&provider.controller_endpoint, client.initialize_response()) {
            Ok(facts) => facts,
            Err(source) => {
                let _ = client.close().await;
                return Err(ControllerError::Routes {
                    provider_id: provider_id.to_string(),
                    source,
                });
            }
        };
    if provider.controller_features != Some(features)
        || provider.data_route_base.as_ref() != Some(&data_route_base)
    {
        let recorded = store
            .record_provider_controller(RecordProviderController {
                provider_id: provider_id.clone(),
                controller_endpoint: provider.controller_endpoint.clone(),
                features,
                data_route_base: data_route_base.clone(),
            })
            .await;
        if let Err(error) = recorded {
            let _ = client.close().await;
            return Err(error.into());
        }
    }
    Ok(ProviderController {
        provider_id: provider_id.clone(),
        client,
        features,
        data_route_base,
        credential,
    })
}

/// Check a controller endpoint at provider registration. A controller that
/// answers definitively wrong (an HTTP refusal such as 404 for a URL that
/// names no controller or 401 for a wrong credential, another plane, or a
/// data route base that fails the downgrade rule) is refused, so a
/// mis-registered provider never provisions anything. One that cannot be
/// reached right now is accepted; its facts are recorded at the first
/// successful handshake.
pub(crate) async fn probe_controller(
    connector: &dyn ProviderControllerConnector,
    endpoint: &EnvironmentEndpoint,
    credential: &SecretValue,
) -> Result<Option<(ControllerFeatures, DataRouteBase)>, AgentApiError> {
    match connector
        .connect(endpoint, provider_connect_options(credential))
        .await
    {
        Ok(client) => {
            let facts = controller_facts(endpoint, client.initialize_response());
            let _ = client.close().await;
            facts.map(Some).map_err(|error| {
                AgentApiError::invalid_request(format!(
                    "the provider controller advertised an unusable data route: {error}"
                ))
            })
        }
        Err(error) if refuses_registration(&error) => Err(AgentApiError::invalid_request(format!(
            "the provider controller at {endpoint} refused the handshake: {error}"
        ))),
        Err(error) => {
            tracing::warn!(
                target: "temporal_runtime",
                %endpoint,
                %error,
                "provider controller is not reachable at registration; its features are recorded at the first successful handshake"
            );
            Ok(None)
        }
    }
}

/// Handshake failures that prove the registration wrong rather than the
/// controller temporarily unavailable.
fn refuses_registration(error: &ClientError) -> bool {
    match error {
        ClientError::UpgradeRejected { status } => {
            (400..500).contains(status) && !matches!(status, 408 | 429)
        }
        ClientError::WrongPlane { .. } | ClientError::Configuration { .. } => true,
        _ => false,
    }
}

/// How the reconciler treats a failed controller interaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ControllerDisposition {
    /// May succeed later unchanged: keep the status, record the error and
    /// retry next pass, until the provisioning deadline.
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

    /// The controller advertised a data route base that fails the
    /// downgrade rule; treated like a failed `initialize`.
    #[error("provider controller {provider_id} advertised an unusable data route: {source}")]
    Routes {
        provider_id: String,
        #[source]
        source: UrlError,
    },

    /// A call on an initialized connection failed.
    #[error("provider controller {provider_id}: {source}")]
    Call {
        provider_id: String,
        #[source]
        source: ClientError,
    },

    /// The call needs an optional feature the controller did not
    /// advertise; nothing was sent.
    #[error("provider {provider_id} does not offer {}", feature.as_str())]
    FeatureUnavailable {
        provider_id: String,
        feature: ControllerFeature,
    },

    /// An earlier failure in this reconcile pass marked the provider
    /// unhealthy; its remaining environments are not attempted.
    #[error("provider controller {provider_id} is skipped for this pass: {message}")]
    Skipped {
        provider_id: String,
        message: String,
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
            Self::Connect { .. } | Self::Routes { .. } | Self::Skipped { .. } => {
                ControllerDisposition::Transient
            }
            Self::Registry(_) => ControllerDisposition::Transient,
        }
    }

    /// True when the controller connection itself failed (a dial, a
    /// deadline, a dropped socket): the provider's remaining environments
    /// are skipped for the rest of the pass, so one hung provider costs one
    /// deadline per pass instead of one per environment.
    pub(crate) fn is_connection_failure(&self) -> bool {
        match self {
            Self::Connect { .. } | Self::Routes { .. } | Self::Skipped { .. } => true,
            Self::Call { source, .. } => !matches!(
                source,
                ClientError::Protocol(_) | ClientError::Decode(_) | ClientError::Encode(_)
            ),
            Self::FeatureUnavailable { .. } | Self::Registry(_) => false,
        }
    }

    /// The precondition a refusal named, if any.
    pub(crate) fn precondition(&self) -> Option<PreconditionReason> {
        match self {
            Self::Call {
                source: ClientError::Protocol(error),
                ..
            } => match error.data {
                ErrorData::FailedPrecondition { reason } => Some(reason),
                _ => None,
            },
            _ => None,
        }
    }

    pub(crate) fn provider_id(&self) -> Option<&str> {
        match self {
            Self::Connect { provider_id, .. }
            | Self::Routes { provider_id, .. }
            | Self::Call { provider_id, .. }
            | Self::FeatureUnavailable { provider_id, .. }
            | Self::Skipped { provider_id, .. } => Some(provider_id),
            Self::Registry(_) => None,
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

/// The controllers one reconcile pass talks to: each provider is dialed at
/// most once per pass, and a provider whose connection failed is skipped
/// for the rest of the pass.
#[derive(Default)]
pub(crate) struct ProviderPass {
    controllers: HashMap<EnvironmentProviderId, PassController>,
}

enum PassController {
    Connected(ProviderController),
    Unhealthy(String),
}

impl ProviderPass {
    pub(crate) async fn controller(
        &mut self,
        store: &dyn EnvironmentProviderStore,
        connector: &dyn ProviderControllerConnector,
        provider_id: &EnvironmentProviderId,
    ) -> Result<ProviderController, ControllerError> {
        match self.controllers.get(provider_id) {
            Some(PassController::Connected(controller)) => return Ok(controller.clone()),
            Some(PassController::Unhealthy(message)) => {
                return Err(ControllerError::Skipped {
                    provider_id: provider_id.to_string(),
                    message: message.clone(),
                });
            }
            None => {}
        }
        match connect_provider_controller(store, connector, provider_id).await {
            Ok(controller) => {
                self.controllers.insert(
                    provider_id.clone(),
                    PassController::Connected(controller.clone()),
                );
                Ok(controller)
            }
            Err(error) => {
                if error.is_connection_failure() {
                    self.controllers.insert(
                        provider_id.clone(),
                        PassController::Unhealthy(error.to_string()),
                    );
                }
                Err(error)
            }
        }
    }

    /// Record a failure of `error`'s provider: a connection failure skips
    /// the provider for the rest of the pass.
    pub(crate) async fn observe_failure(&mut self, error: &ControllerError) {
        if !error.is_connection_failure() {
            return;
        }
        let Some(provider_id) = error.provider_id() else {
            return;
        };
        let Ok(provider_id) = EnvironmentProviderId::try_new(provider_id) else {
            return;
        };
        if let Some(PassController::Connected(controller)) = self
            .controllers
            .insert(provider_id, PassController::Unhealthy(error.to_string()))
        {
            controller.close().await;
        }
    }

    pub(crate) async fn close(self) {
        for controller in self.controllers.into_values() {
            if let PassController::Connected(controller) = controller {
                controller.close().await;
            }
        }
    }
}

/// Dials providers whose controller facts are not recorded yet (never
/// reached, or their endpoint changed and the registration probe could not
/// reach the new one), so the gateway's data proxy gets a data route base
/// even when none of the provider's environments needs convergence. Failed
/// attempts back off per provider.
#[derive(Default)]
pub(crate) struct ProviderFactsRefresh {
    listed_at: Option<Instant>,
    retry: HashMap<EnvironmentProviderId, (Instant, Duration)>,
}

impl ProviderFactsRefresh {
    /// How often the provider list is read.
    const LIST_INTERVAL: Duration = Duration::from_secs(5);
    const FIRST_RETRY: Duration = Duration::from_secs(5);
    const MAX_RETRY: Duration = Duration::from_secs(300);

    /// One refresh step of a deployment pass; connections stay in `pass`.
    pub(crate) async fn run(
        &mut self,
        pass: &mut ProviderPass,
        store: &dyn EnvironmentProviderStore,
        connector: &dyn ProviderControllerConnector,
    ) {
        let now = Instant::now();
        if self
            .listed_at
            .is_some_and(|listed| now.duration_since(listed) < Self::LIST_INTERVAL)
        {
            return;
        }
        self.listed_at = Some(now);
        let providers = match store
            .list_providers(ListEnvironmentProviders::default())
            .await
        {
            Ok(providers) => providers,
            Err(error) => {
                tracing::warn!(target: "temporal_runtime", %error, "listing providers for a controller handshake failed");
                return;
            }
        };
        let unrecorded: Vec<EnvironmentProviderId> = providers
            .into_iter()
            .filter(|provider| provider.data_route_base.is_none())
            .map(|provider| provider.provider_id)
            .collect();
        self.retry
            .retain(|provider_id, _| unrecorded.contains(provider_id));
        for provider_id in unrecorded {
            if self
                .retry
                .get(&provider_id)
                .is_some_and(|(next, _)| now < *next)
            {
                continue;
            }
            match pass.controller(store, connector, &provider_id).await {
                Ok(_) => {
                    self.retry.remove(&provider_id);
                }
                Err(error) => {
                    let backoff = self
                        .retry
                        .get(&provider_id)
                        .map_or(Self::FIRST_RETRY, |(_, backoff)| {
                            (*backoff * 2).min(Self::MAX_RETRY)
                        });
                    tracing::debug!(
                        target: "temporal_runtime",
                        %provider_id,
                        %error,
                        retry_in_s = backoff.as_secs(),
                        "provider controller handshake failed; its data route base stays unrecorded"
                    );
                    self.retry.insert(provider_id, (now + backoff, backoff));
                }
            }
        }
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

    /// Refuse an API request that needs an optional controller feature the
    /// provider does not offer. Features recorded at the last handshake
    /// decide; a provider not reached yet is dialed once to learn them.
    pub(crate) async fn require_controller_feature(
        &self,
        provider_id: &EnvironmentProviderId,
        feature: ControllerFeature,
    ) -> Result<(), AgentApiError> {
        let provider = self.read_environment_provider(provider_id).await?;
        let features = match provider.controller_features {
            Some(features) => features,
            None => {
                let controller = self
                    .connect_provider_controller(provider_id)
                    .await
                    .map_err(ControllerError::into_api_error)?;
                controller.close().await;
                controller.features
            }
        };
        if feature.in_features(&features) {
            Ok(())
        } else {
            Err(AgentApiError::rejected(format!(
                "provider {provider_id} does not offer {}",
                feature.as_str()
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use environment_protocol::{
        CONTROLLER_PROTOCOL_VERSION,
        control::handshake::{ControllerCapabilities, ControllerRoutes},
        error::ProtocolError,
        handshake::{ImplementationInfo, Plane},
    };

    fn initialize(data: &str) -> ControllerInitializeResponse {
        ControllerInitializeResponse {
            protocol_version: CONTROLLER_PROTOCOL_VERSION,
            plane: Plane::Controller,
            server_info: ImplementationInfo::new("controller").unwrap(),
            capabilities: ControllerCapabilities {
                adopt_target: true,
                ingress: false,
            },
            routes: ControllerRoutes {
                data: DataRouteBase::new(data).unwrap(),
            },
        }
    }

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
                local: 3,
                remote: 4,
            },
            ClientError::UpgradeRejected { status: 404 },
            ClientError::ConnectTimeout {
                endpoint: "ws://provider".into(),
            },
        ] {
            let error = ControllerError::Connect {
                provider_id: "provider".into(),
                source,
            };
            assert_eq!(error.disposition(), ControllerDisposition::Transient);
            assert!(error.is_connection_failure());
        }
    }

    #[test]
    fn typed_refusals_and_missing_targets_are_final() {
        let refused = call(ClientError::Protocol(ProtocolError::failed_precondition(
            PreconditionReason::TemplateDeprecated,
            "deprecated",
        )));
        assert_eq!(refused.disposition(), ControllerDisposition::Refused);
        assert_eq!(
            refused.precondition(),
            Some(PreconditionReason::TemplateDeprecated)
        );
        assert!(!refused.is_connection_failure());
        let gone = call(ClientError::Protocol(ProtocolError::not_found("no target")));
        assert_eq!(gone.disposition(), ControllerDisposition::Gone);
        let unadvertised = ControllerError::FeatureUnavailable {
            provider_id: "provider".into(),
            feature: ControllerFeature::AdoptTarget,
        };
        assert_eq!(unadvertised.disposition(), ControllerDisposition::Refused);
        // A deadline or a dropped socket skips the provider for the pass.
        assert!(call(ClientError::DeadlineExceeded { elapsed_ms: 1 }).is_connection_failure());
        assert!(!call(ClientError::Decode("bad".into())).is_connection_failure());
    }

    #[test]
    fn registration_refuses_only_definitive_handshake_failures() {
        for refused in [
            ClientError::UpgradeRejected { status: 404 },
            ClientError::UpgradeRejected { status: 401 },
            ClientError::WrongPlane {
                expected: Plane::Controller,
                actual: Plane::Data,
            },
        ] {
            assert!(refuses_registration(&refused), "{refused}");
        }
        for accepted in [
            ClientError::UpgradeRejected { status: 503 },
            ClientError::UpgradeRejected { status: 429 },
            ClientError::Connect {
                endpoint: "ws://provider".into(),
                message: "refused".into(),
            },
            ClientError::VersionMismatch {
                plane: Plane::Controller,
                local: 3,
                remote: 4,
            },
        ] {
            assert!(!refuses_registration(&accepted), "{accepted}");
        }
    }

    /// A connector whose every dial fails, counting the attempts.
    #[derive(Default)]
    struct UnreachableConnector {
        attempts: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl ProviderControllerConnector for UnreachableConnector {
        async fn connect(
            &self,
            endpoint: &EnvironmentEndpoint,
            _options: ConnectOptions,
        ) -> Result<ControllerClient, ClientError> {
            self.attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(ClientError::Connect {
                endpoint: endpoint.to_string(),
                message: "refused".into(),
            })
        }
    }

    async fn provider_store(
        endpoint: &str,
        credential: &str,
    ) -> ::environments::InMemoryEnvironmentRegistryStore {
        let store = ::environments::InMemoryEnvironmentRegistryStore::new();
        store
            .put_provider(::environments::PutEnvironmentProvider {
                provider_id: EnvironmentProviderId::new("provider"),
                display_name: None,
                controller_endpoint: EnvironmentEndpoint::parse(endpoint).unwrap(),
                metadata: Default::default(),
                credential: Some(SecretValue::new(credential)),
                updated_at_ms: 1,
            })
            .await
            .unwrap();
        store
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_refresh_records_facts_of_a_provider_never_reached() {
        let controller = crate::environments::fake_controller::FakeController::start()
            .await
            .unwrap();
        let store = provider_store(controller.controller_url(), controller.credential()).await;
        let provider_id = EnvironmentProviderId::new("provider");
        assert!(
            store
                .read_provider(&provider_id)
                .await
                .unwrap()
                .data_route_base
                .is_none()
        );
        let mut pass = ProviderPass::default();
        ProviderFactsRefresh::default()
            .run(&mut pass, &store, &WebSocketProviderControllerConnector)
            .await;
        pass.close().await;
        let provider = store.read_provider(&provider_id).await.unwrap();
        assert!(provider.data_route_base.is_some());
        assert!(provider.controller_features.is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_unreachable_provider_is_retried_with_backoff() {
        let store = provider_store("ws://127.0.0.1:9/control", "credential").await;
        let connector = UnreachableConnector::default();
        let mut refresh = ProviderFactsRefresh::default();
        let mut pass = ProviderPass::default();
        refresh.run(&mut pass, &store, &connector).await;
        assert_eq!(
            connector.attempts.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        // The next listing still finds it unrecorded, but its retry is not
        // due yet.
        refresh.listed_at = None;
        let mut pass = ProviderPass::default();
        refresh.run(&mut pass, &store, &connector).await;
        assert_eq!(
            connector.attempts.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        let (_, backoff) = refresh.retry[&EnvironmentProviderId::new("provider")];
        assert_eq!(backoff, ProviderFactsRefresh::FIRST_RETRY);
    }

    #[test]
    fn controller_facts_enforce_the_downgrade_rule() {
        let secure = EnvironmentEndpoint::parse("wss://provider.test/control").unwrap();
        assert!(matches!(
            controller_facts(&secure, &initialize("ws://provider.test/routes")),
            Err(UrlError::Downgrade(_))
        ));
        let (features, base) =
            controller_facts(&secure, &initialize("wss://provider.test/routes")).unwrap();
        assert!(features.adopt_target && !features.ingress);
        assert_eq!(base.as_str(), "wss://provider.test/routes");
        let plain = EnvironmentEndpoint::parse("ws://127.0.0.1:9/control").unwrap();
        assert!(controller_facts(&plain, &initialize("ws://127.0.0.1:9/routes")).is_ok());
        let local = EnvironmentEndpoint::parse("unix:///run/provider.sock").unwrap();
        assert!(controller_facts(&local, &initialize("ws://localhost/routes")).is_ok());
    }
}
