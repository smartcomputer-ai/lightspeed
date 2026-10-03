use std::sync::Arc;

use environment_protocol::shared::{
    EnvironmentCapabilities, EnvironmentDataConnection, EnvironmentScope, EnvironmentTransport,
    ProviderTargetId,
};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RouteKey {
    pub universe_id: Uuid,
    pub environment_id: String,
    pub incarnation_id: String,
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
            incarnation_id: environment.incarnation.incarnation_id.to_string(),
        }
    }

    pub fn connection(&self, key: &RouteKey) -> EnvironmentDataConnection {
        let base = websocket_base(&self.base_url);
        EnvironmentDataConnection {
            target_id: ProviderTargetId::new(key.environment_id.clone()),
            endpoint: format!(
                "{base}/environment-gateway/routes/{}/{}/{}",
                key.universe_id, key.environment_id, key.incarnation_id
            ),
            transport: EnvironmentTransport::WebSocket,
            scope: EnvironmentScope::Default,
            default_cwd: None,
            capabilities: EnvironmentCapabilities::default(),
        }
    }

    pub fn connection_for(
        &self,
        universe_id: Uuid,
        environment: &environments::EnvironmentRecord,
    ) -> EnvironmentDataConnection {
        self.connection(&self.route_key(universe_id, environment))
    }

    pub fn connect_options(
        &self,
        user_agent: impl Into<String>,
    ) -> environment_client::WebSocketConnectOptions {
        environment_client::WebSocketConnectOptions {
            bearer_token: Some((*self.deployment_token).clone()),
            user_agent: Some(user_agent.into()),
            headers: Vec::new(),
        }
    }
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

pub(crate) fn websocket_base(url: &str) -> String {
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
            incarnation_id: "incarnation-a".to_owned(),
        }
    }

    #[test]
    fn gateway_connection_is_stable_and_has_no_cached_capabilities() {
        let config = EnvironmentGatewayClientConfig::new("https://gateway.test", "secret");
        let connection = config.connection(&key());
        assert_eq!(
            connection.endpoint,
            "wss://gateway.test/environment-gateway/routes/00000000-0000-0000-0000-000000000000/environment-a/incarnation-a"
        );
        assert_eq!(connection.capabilities, EnvironmentCapabilities::default());
    }
}
