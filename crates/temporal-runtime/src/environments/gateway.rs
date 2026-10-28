use std::{sync::Arc, time::Duration};

use environment_client::{ClientError, ConnectOptions, DataClient};
use environment_protocol::{data::handshake::InitializeResponse, handshake::ImplementationInfo};
use uuid::Uuid;

/// Path prefix of the worker routes the environment gateway serves:
/// `{prefix}/{universe}/{environment}`. Runtime-internal: only Lightspeed's
/// own workers dial it, with the deployment token.
pub const ROUTE_PATH_PREFIX: &str = "/environment-gateway/routes";

/// Connect bound (TCP, TLS and upgrade) of a worker route. The gateway
/// answers a registered environment's upgrade only once the reverse
/// stream is paired, which takes up to its stream-open bound; the worker
/// waits a little longer so that the gateway's typed refusal, not the
/// client's own timeout, reaches the caller.
pub const WORKER_ROUTE_CONNECT_TIMEOUT: Duration =
    Duration::from_secs(crate::gateway::registration::STREAM_OPEN_TIMEOUT.as_secs() + 5);

/// Runtime-internal header of a worker route dial that must not wake a
/// powered-down target: environment source discovery. The gateway asks the
/// provider for a provisioned target's current power first and refuses the
/// dial (bodiless 412) unless the target is active and running; the
/// recorded observation cannot decide, since providers power targets down
/// on their own clock. Other sources have no power to keep.
pub const NO_WAKE_HEADER: &str = "lightspeed-no-wake";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RouteKey {
    pub universe_id: Uuid,
    pub environment_id: String,
}

#[derive(Clone)]
pub struct EnvironmentGatewayClientConfig {
    base_url: String,
    deployment_token: Arc<String>,
}

impl EnvironmentGatewayClientConfig {
    pub fn new(base_url: impl Into<String>, deployment_token: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            deployment_token: Arc::new(deployment_token.into()),
        }
    }

    pub fn from_env(default_base_url: Option<&str>) -> anyhow::Result<Self> {
        let base_url = std::env::var("LIGHTSPEED_ENVIRONMENT_GATEWAY_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| default_base_url.map(ToOwned::to_owned))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "LIGHTSPEED_ENVIRONMENT_GATEWAY_URL is required for environment routing"
                )
            })?;
        let token = std::env::var("LIGHTSPEED_ENVIRONMENT_GATEWAY_TOKEN")
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| default_base_url.map(|_| format!("local-{}", Uuid::new_v4())))
            .ok_or_else(|| anyhow::anyhow!("LIGHTSPEED_ENVIRONMENT_GATEWAY_TOKEN is required for a separate worker deployment"))?;
        Ok(Self::new(base_url, token))
    }

    pub fn deployment_token(&self) -> &str {
        &self.deployment_token
    }

    pub fn route_key(
        &self,
        universe_id: Uuid,
        environment: &environments::EnvironmentRecord,
    ) -> RouteKey {
        RouteKey {
            universe_id,
            environment_id: environment.environment_id.to_string(),
        }
    }

    /// The worker route of one environment: a data-plane endpoint whatever
    /// the environment's source. Instead of upgrading, the gateway may
    /// answer a typed `DataRouteRefusal`.
    pub fn endpoint(&self, key: &RouteKey) -> String {
        format!(
            "{}{ROUTE_PATH_PREFIX}/{}/{}",
            websocket_base(&self.base_url),
            key.universe_id,
            environment_protocol::routes::encode_path_segment(&key.environment_id),
        )
    }

    pub fn endpoint_for(
        &self,
        universe_id: Uuid,
        environment: &environments::EnvironmentRecord,
    ) -> String {
        self.endpoint(&self.route_key(universe_id, environment))
    }

    /// Data-plane connect options for a worker route: `clientInfo` names
    /// the runtime build, `user_agent` the caller, and the deployment token
    /// is the bearer, and the connect bound is
    /// [`WORKER_ROUTE_CONNECT_TIMEOUT`]. Callers on background paths add a
    /// request timeout.
    pub fn connect_options(&self, user_agent: impl Into<String>) -> ConnectOptions {
        let mut options = ConnectOptions::new(runtime_client_info())
            .with_bearer_token((*self.deployment_token).clone())
            .with_user_agent(user_agent);
        options.connect_timeout = WORKER_ROUTE_CONNECT_TIMEOUT;
        options
    }

    /// [`connect_options`](Self::connect_options) for a dial that must not
    /// wake its target ([`NO_WAKE_HEADER`]).
    pub fn no_wake_connect_options(&self, user_agent: impl Into<String>) -> ConnectOptions {
        self.connect_options(user_agent)
            .with_header(NO_WAKE_HEADER, "1")
    }

    /// Dial the worker route of `key` and complete the data-plane
    /// handshake.
    pub async fn connect(
        &self,
        key: &RouteKey,
        options: ConnectOptions,
    ) -> Result<(DataClient, InitializeResponse), ClientError> {
        DataClient::connect_initialized(&self.endpoint(key), options).await
    }
}

/// `clientInfo` of every data and controller connection this runtime
/// opens.
pub fn runtime_client_info() -> ImplementationInfo {
    ImplementationInfo::new("lightspeed-runtime")
        .and_then(|info| info.with_version(release_info::VERSION))
        .and_then(|info| info.with_git_sha(release_info::GIT_SHA))
        .expect("the runtime's build facts are valid implementation info")
}

pub fn bearer_matches(headers: &axum::http::HeaderMap, expected: &str) -> bool {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| constant_time_eq(token.as_bytes(), expected.as_bytes()))
}

pub fn close_message(reason: &'static str) -> axum::extract::ws::Message {
    axum::extract::ws::Message::Close(Some(axum::extract::ws::CloseFrame {
        code: 1008,
        reason: reason.into(),
    }))
}

fn websocket_base(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        url.to_owned()
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (&left, &right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> RouteKey {
        RouteKey {
            universe_id: Uuid::nil(),
            environment_id: "environment-a".to_owned(),
        }
    }

    #[test]
    fn worker_routes_are_stable_websocket_urls() {
        let config = EnvironmentGatewayClientConfig::new("https://gateway.test/", "secret");
        assert_eq!(
            config.endpoint(&key()),
            "wss://gateway.test/environment-gateway/routes/00000000-0000-0000-0000-000000000000/environment-a"
        );
        let local = EnvironmentGatewayClientConfig::new("http://127.0.0.1:18080", "secret");
        assert!(
            local
                .endpoint(&key())
                .starts_with("ws://127.0.0.1:18080/environment-gateway/routes/")
        );
    }

    #[test]
    fn connect_options_carry_the_deployment_token_and_runtime_build() {
        let config = EnvironmentGatewayClientConfig::new("https://gateway.test", "secret");
        let options = config.connect_options("lightspeed-test");
        assert_eq!(
            options.bearer_token.as_ref().map(|token| token.expose()),
            Some("secret")
        );
        assert_eq!(options.user_agent.as_deref(), Some("lightspeed-test"));
        assert_eq!(options.client_info.name(), "lightspeed-runtime");
        assert_eq!(options.client_info.version(), Some(release_info::VERSION));
        assert_eq!(options.connect_timeout, WORKER_ROUTE_CONNECT_TIMEOUT);
    }

    #[test]
    fn bearer_tokens_compare_exactly() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer secret".parse().unwrap(),
        );
        assert!(bearer_matches(&headers, "secret"));
        assert!(!bearer_matches(&headers, "secre"));
        assert!(!bearer_matches(&axum::http::HeaderMap::new(), "secret"));
    }
}
