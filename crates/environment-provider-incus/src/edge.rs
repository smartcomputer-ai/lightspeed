//! Stateless provider edge proxy for public environment applications.

use axum::{
    Router,
    body::Body,
    extract::{Query, State, WebSocketUpgrade, ws::WebSocket},
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::any,
};
use futures_util::{SinkExt as _, StreamExt as _};
use serde::Deserialize;
use tokio_tungstenite::{connect_async, tungstenite::Message as UpstreamMessage};

use crate::IncusBackend;

#[derive(Clone)]
struct EdgeState<B> {
    backend: B,
    http: reqwest::Client,
}

const AUTHORIZE_PATH: &str = "/.well-known/lightspeed-ingress-authorization";

pub async fn serve<B: IncusBackend>(
    listener: tokio::net::TcpListener,
    backend: B,
) -> anyhow::Result<()> {
    let app = Router::new()
        .route(AUTHORIZE_PATH, axum::routing::get(authorize::<B>))
        .fallback(any(proxy::<B>))
        .with_state(EdgeState {
            backend,
            http: reqwest::Client::new(),
        });
    axum::serve(listener, app).await?;
    Ok(())
}

#[derive(Deserialize)]
struct AuthorizeQuery {
    domain: String,
}

/// Authorize Caddy on-demand TLS only for a currently enabled, ready ingress
/// hostname. The edge remains tailnet-only; Caddy also blocks this path from
/// the public reverse proxy.
async fn authorize<B: IncusBackend>(
    State(state): State<EdgeState<B>>,
    Query(query): Query<AuthorizeQuery>,
) -> StatusCode {
    match resolve(&state, &query.domain).await {
        Ok(Some(_)) => StatusCode::NO_CONTENT,
        Ok(None) => StatusCode::NOT_FOUND,
        Err(_) => StatusCode::BAD_GATEWAY,
    }
}

async fn proxy<B: IncusBackend>(
    State(state): State<EdgeState<B>>,
    upgrade: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(':').next())
        .unwrap_or_default();
    let route = match resolve(&state, host).await {
        Ok(Some(route)) => route,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };
    let path = uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/");
    if let Ok(upgrade) = upgrade {
        let endpoint = format!("ws://{}:{}{}", route.address, route.port, path);
        return upgrade
            .on_upgrade(move |socket| proxy_websocket(socket, endpoint))
            .into_response();
    }
    let endpoint = format!("http://{}:{}{}", route.address, route.port, path);
    let mut request = state
        .http
        .request(method, endpoint)
        .body(reqwest::Body::wrap_stream(body.into_data_stream()));
    for (name, value) in &headers {
        if name != header::HOST && name != header::CONNECTION && name != header::UPGRADE {
            request = request.header(name, value);
        }
    }
    request = request
        .header("x-forwarded-host", host)
        .header("x-forwarded-proto", "https");
    let upstream = match request.send().await {
        Ok(response) => response,
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };
    let status = upstream.status();
    let response_headers = upstream.headers().clone();
    let mut response = Response::new(Body::from_stream(upstream.bytes_stream()));
    *response.status_mut() = status;
    for (name, value) in response_headers {
        if let Some(name) = name
            && name != header::CONNECTION
            && name != header::UPGRADE
        {
            response.headers_mut().append(name, value);
        }
    }
    response
}

struct Route {
    address: String,
    port: u16,
}

async fn resolve<B: IncusBackend>(
    state: &EdgeState<B>,
    hostname: &str,
) -> anyhow::Result<Option<Route>> {
    Ok(resolve_targets(
        state.backend.list_all_owned().await?,
        hostname,
    ))
}

fn resolve_targets(targets: Vec<crate::incus::OwnedTarget>, hostname: &str) -> Option<Route> {
    for target in targets {
        if target.ingress_hostname.as_deref() == Some(hostname)
            && target.status == environment_protocol::control::targets::ProviderTargetStatus::Ready
            && let (Some(address), Some(port)) = (target.ipv4_address, target.ingress_port)
        {
            return Some(Route { address, port });
        }
    }
    None
}

async fn proxy_websocket(mut client: WebSocket, endpoint: String) {
    let Ok((mut upstream, _)) = connect_async(endpoint).await else {
        return;
    };
    loop {
        tokio::select! {
            message = client.recv() => match message {
                Some(Ok(message)) => {
                    let close = matches!(message, axum::extract::ws::Message::Close(_));
                    let mapped = match message {
                        axum::extract::ws::Message::Text(v) => UpstreamMessage::Text(v.to_string().into()),
                        axum::extract::ws::Message::Binary(v) => UpstreamMessage::Binary(v),
                        axum::extract::ws::Message::Ping(v) => UpstreamMessage::Ping(v),
                        axum::extract::ws::Message::Pong(v) => UpstreamMessage::Pong(v),
                        axum::extract::ws::Message::Close(_) => UpstreamMessage::Close(None),
                    };
                    if upstream.send(mapped).await.is_err() || close { break }
                }
                _ => break,
            },
            message = upstream.next() => match message {
                Some(Ok(message)) => {
                    let close = matches!(message, UpstreamMessage::Close(_));
                    let mapped = match message {
                        UpstreamMessage::Text(v) => axum::extract::ws::Message::Text(v.to_string().into()),
                        UpstreamMessage::Binary(v) => axum::extract::ws::Message::Binary(v),
                        UpstreamMessage::Ping(v) => axum::extract::ws::Message::Ping(v),
                        UpstreamMessage::Pong(v) => axum::extract::ws::Message::Pong(v),
                        UpstreamMessage::Close(_) => axum::extract::ws::Message::Close(None),
                        UpstreamMessage::Frame(_) => continue,
                    };
                    if client.send(mapped).await.is_err() || close { break }
                }
                _ => break,
            },
        }
    }
    // Complete both handshakes even when either peer disappears or errors.
    let _ = upstream.close(None).await;
    let _ = client.send(axum::extract::ws::Message::Close(None)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incus::OwnedTarget;
    use axum::{Router, extract::WebSocketUpgrade, routing::get};
    use environment_protocol::{control::targets::ProviderTargetStatus, shared::ProviderTargetId};
    use std::time::Duration;

    fn target(status: ProviderTargetStatus, hostname: Option<&str>) -> OwnedTarget {
        OwnedTarget {
            target_id: ProviderTargetId::new("target-a"),
            name: "target-a".to_owned(),
            universe_id: "universe-a".to_owned(),
            binding_id: "binding-a".to_owned(),
            environment_id: "environment-a".to_owned(),
            incarnation_id: "incarnation-a".to_owned(),
            request_id: "request-a".to_owned(),
            template_id: "template-a".to_owned(),
            image_fingerprint: "fingerprint-a".to_owned(),
            adoption_source: None,
            status,
            ipv4_address: Some("10.0.0.2".to_owned()),
            ingress_hostname: hostname.map(ToOwned::to_owned),
            ingress_port: Some(8080),
            location: None,
        }
    }

    #[tokio::test]
    async fn ingress_proxy_closes_upstream_when_client_disappears() {
        let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("upstream listener");
        let upstream_address = upstream_listener.local_addr().expect("upstream address");
        let upstream_observer = tokio::spawn(async move {
            let (stream, _) = upstream_listener.accept().await.expect("upstream accept");
            let mut socket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("upstream websocket");
            while let Some(message) = socket.next().await {
                match message {
                    Ok(UpstreamMessage::Close(_)) => return true,
                    Ok(_) => {}
                    Err(_) => return false,
                }
            }
            false
        });

        let edge_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("edge listener");
        let edge_address = edge_listener.local_addr().expect("edge address");
        let endpoint = format!("ws://{upstream_address}");
        let app = Router::new().route(
            "/",
            get(move |upgrade: WebSocketUpgrade| {
                let endpoint = endpoint.clone();
                async move { upgrade.on_upgrade(move |socket| proxy_websocket(socket, endpoint)) }
            }),
        );
        let edge_server =
            tokio::spawn(
                async move { axum::serve(edge_listener, app).await.expect("edge server") },
            );

        let (client, _) = tokio_tungstenite::connect_async(format!("ws://{edge_address}"))
            .await
            .expect("connect edge");
        drop(client);

        let saw_close = tokio::time::timeout(Duration::from_secs(2), upstream_observer)
            .await
            .expect("upstream close timeout")
            .expect("upstream observer");
        edge_server.abort();
        assert!(saw_close, "edge dropped upstream without a close frame");
    }

    #[test]
    fn authorization_resolves_only_the_exact_ready_ingress_hostname() {
        let hostname = "route.env.example.com";
        let route = resolve_targets(
            vec![target(ProviderTargetStatus::Ready, Some(hostname))],
            hostname,
        )
        .expect("ready route");
        assert_eq!(route.address, "10.0.0.2");
        assert_eq!(route.port, 8080);

        assert!(
            resolve_targets(
                vec![target(ProviderTargetStatus::Starting, Some(hostname))],
                hostname,
            )
            .is_none()
        );
        assert!(
            resolve_targets(
                vec![target(ProviderTargetStatus::Ready, Some(hostname))],
                "other.env.example.com",
            )
            .is_none()
        );
    }
}
