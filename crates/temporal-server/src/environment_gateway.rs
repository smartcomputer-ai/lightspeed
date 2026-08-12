use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::extract::ws::Message;
use host_protocol::shared::{
    HostCapabilities, HostConnectionSpec, HostScope, HostTargetId, HostTransport,
};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RouteKey {
    pub universe_id: Uuid,
    pub environment_id: String,
    pub incarnation_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteOwner {
    DirectDaemon { daemon_id: String },
}

pub enum RouteCommand {
    Call {
        message: Value,
        response: oneshot::Sender<Result<Value, String>>,
    },
    Notify {
        message: Value,
    },
}

#[derive(Clone)]
struct RouteEntry {
    connection_id: String,
    owner: RouteOwner,
    capabilities: HostCapabilities,
    default_cwd: Option<String>,
    sender: mpsc::Sender<RouteCommand>,
    fence: watch::Sender<bool>,
    connected_at: Instant,
}

#[derive(Clone, Debug)]
pub struct RouteSnapshot {
    pub connection_id: String,
    pub owner: RouteOwner,
    pub capabilities: HostCapabilities,
    pub default_cwd: Option<String>,
    pub connected_for: Duration,
}

#[derive(Default)]
pub struct EnvironmentRouteRegistry {
    routes: tokio::sync::RwLock<BTreeMap<RouteKey, RouteEntry>>,
}

impl EnvironmentRouteRegistry {
    pub async fn register(
        &self,
        key: RouteKey,
        connection_id: String,
        owner: RouteOwner,
        capabilities: HostCapabilities,
        default_cwd: Option<String>,
        sender: mpsc::Sender<RouteCommand>,
    ) -> watch::Receiver<bool> {
        let (fence, receiver) = watch::channel(false);
        let old = self.routes.write().await.insert(
            key,
            RouteEntry {
                connection_id,
                owner,
                capabilities,
                default_cwd,
                sender,
                fence,
                connected_at: Instant::now(),
            },
        );
        if let Some(old) = old {
            let _ = old.fence.send(true);
        }
        receiver
    }

    pub async fn unregister(&self, key: &RouteKey, connection_id: &str) -> bool {
        let mut routes = self.routes.write().await;
        if routes
            .get(key)
            .is_some_and(|entry| entry.connection_id == connection_id)
        {
            routes.remove(key);
            true
        } else {
            false
        }
    }

    pub async fn fence(&self, key: &RouteKey) -> bool {
        let entry = self.routes.write().await.remove(key);
        if let Some(entry) = entry {
            let _ = entry.fence.send(true);
            true
        } else {
            false
        }
    }

    pub async fn snapshot(&self, key: &RouteKey) -> Option<RouteSnapshot> {
        self.routes
            .read()
            .await
            .get(key)
            .map(|entry| RouteSnapshot {
                connection_id: entry.connection_id.clone(),
                owner: entry.owner.clone(),
                capabilities: entry.capabilities.clone(),
                default_cwd: entry.default_cwd.clone(),
                connected_for: entry.connected_at.elapsed(),
            })
    }

    pub async fn call(&self, key: &RouteKey, message: Value) -> Result<Value, String> {
        let sender = self
            .routes
            .read()
            .await
            .get(key)
            .map(|entry| entry.sender.clone())
            .ok_or_else(|| "environment has no live gateway route".to_owned())?;
        let (response_tx, response_rx) = oneshot::channel();
        sender
            .send(RouteCommand::Call {
                message,
                response: response_tx,
            })
            .await
            .map_err(|_| "environment gateway route closed".to_owned())?;
        response_rx
            .await
            .map_err(|_| "environment gateway route closed".to_owned())?
    }

    pub async fn notify(&self, key: &RouteKey, message: Value) -> Result<(), String> {
        let sender = self
            .routes
            .read()
            .await
            .get(key)
            .map(|entry| entry.sender.clone())
            .ok_or_else(|| "environment has no live gateway route".to_owned())?;
        sender
            .send(RouteCommand::Notify { message })
            .await
            .map_err(|_| "environment gateway route closed".to_owned())
    }
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

    pub fn connection(
        &self,
        key: &RouteKey,
        snapshot: Option<&RouteSnapshot>,
    ) -> HostConnectionSpec {
        let base = websocket_base(&self.base_url);
        HostConnectionSpec {
            target_id: HostTargetId::new(key.environment_id.clone()),
            endpoint: format!(
                "{base}/environment-gateway/routes/{}/{}/{}",
                key.universe_id, key.environment_id, key.incarnation_id
            ),
            transport: HostTransport::WebSocket,
            scope: HostScope::Default,
            default_cwd: snapshot
                .and_then(|snapshot| snapshot.default_cwd.as_deref())
                .and_then(|cwd| cwd.try_into().ok()),
            capabilities: snapshot
                .map(|snapshot| snapshot.capabilities.clone())
                .unwrap_or_default(),
        }
    }

    pub fn connection_for(
        &self,
        universe_id: Uuid,
        environment: &environments::EnvironmentRecord,
    ) -> HostConnectionSpec {
        self.connection(&self.route_key(universe_id, environment), None)
    }

    pub fn connect_options(
        &self,
        user_agent: impl Into<String>,
    ) -> host_client::WebSocketConnectOptions {
        host_client::WebSocketConnectOptions {
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

pub fn close_message(reason: &'static str) -> Message {
    Message::Close(Some(axum::extract::ws::CloseFrame {
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
            incarnation_id: "incarnation-a".to_owned(),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn replacement_route_fences_old_connection() {
        let registry = EnvironmentRouteRegistry::default();
        let (old_tx, mut old_rx) = mpsc::channel(1);
        let mut old_fence = registry
            .register(
                key(),
                "connection-old".to_owned(),
                RouteOwner::DirectDaemon {
                    daemon_id: "daemon-a".to_owned(),
                },
                HostCapabilities::default(),
                None,
                old_tx,
            )
            .await;
        let (new_tx, _new_rx) = mpsc::channel(1);
        registry
            .register(
                key(),
                "connection-new".to_owned(),
                RouteOwner::DirectDaemon {
                    daemon_id: "daemon-a".to_owned(),
                },
                HostCapabilities::default(),
                None,
                new_tx,
            )
            .await;
        assert!(old_fence.changed().await.is_ok());
        assert!(*old_fence.borrow());
        assert!(old_rx.try_recv().is_err());
        assert_eq!(
            registry.snapshot(&key()).await.unwrap().connection_id,
            "connection-new"
        );
        assert!(!registry.unregister(&key(), "connection-old").await);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn calls_are_forwarded_to_current_route() {
        let registry = Arc::new(EnvironmentRouteRegistry::default());
        let (tx, mut rx) = mpsc::channel(1);
        registry
            .register(
                key(),
                "connection-a".to_owned(),
                RouteOwner::DirectDaemon {
                    daemon_id: "daemon-a".to_owned(),
                },
                HostCapabilities::default(),
                None,
                tx,
            )
            .await;
        let registry_for_call = registry.clone();
        let call = tokio::spawn(async move {
            registry_for_call
                .call(&key(), serde_json::json!({"id": 1, "method": "test"}))
                .await
        });
        let Some(RouteCommand::Call { message, response }) = rx.recv().await else {
            panic!("expected route call")
        };
        assert_eq!(message["method"], "test");
        response
            .send(Ok(serde_json::json!({"id": 1, "result": true})))
            .unwrap();
        assert_eq!(call.await.unwrap().unwrap()["result"], true);
    }
}
