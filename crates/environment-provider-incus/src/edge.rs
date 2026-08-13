//! Stateless provider edge proxy for public environment applications.

use axum::{
    Router,
    body::Body,
    extract::{State, WebSocketUpgrade, ws::WebSocket},
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::any,
};
use futures_util::{SinkExt as _, StreamExt as _};
use tokio_tungstenite::{connect_async, tungstenite::Message as UpstreamMessage};

use crate::IncusBackend;

#[derive(Clone)]
struct EdgeState<B> {
    backend: B,
    http: reqwest::Client,
}

pub async fn serve<B: IncusBackend>(
    listener: tokio::net::TcpListener,
    backend: B,
) -> anyhow::Result<()> {
    let app = Router::new()
        .fallback(any(proxy::<B>))
        .with_state(EdgeState {
            backend,
            http: reqwest::Client::new(),
        });
    axum::serve(listener, app).await?;
    Ok(())
}

async fn proxy<B: IncusBackend>(
    State(state): State<EdgeState<B>>,
    upgrade: Option<WebSocketUpgrade>,
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
    if let Some(upgrade) = upgrade {
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
    for target in state.backend.list_all_owned().await? {
        if target.ingress_hostname.as_deref() == Some(hostname)
            && target.status == host_protocol::control::targets::HostTargetStatus::Ready
            && let (Some(address), Some(port)) = (target.ipv4_address, target.ingress_port)
        {
            return Ok(Some(Route { address, port }));
        }
    }
    Ok(None)
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
                        axum::extract::ws::Message::Binary(v) => UpstreamMessage::Binary(v.to_vec().into()),
                        axum::extract::ws::Message::Ping(v) => UpstreamMessage::Ping(v.to_vec().into()),
                        axum::extract::ws::Message::Pong(v) => UpstreamMessage::Pong(v.to_vec().into()),
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
                        UpstreamMessage::Text(v) => axum::extract::ws::Message::Text(v.to_string()),
                        UpstreamMessage::Binary(v) => axum::extract::ws::Message::Binary(v.to_vec()),
                        UpstreamMessage::Ping(v) => axum::extract::ws::Message::Ping(v.to_vec()),
                        UpstreamMessage::Pong(v) => axum::extract::ws::Message::Pong(v.to_vec()),
                        UpstreamMessage::Close(_) => axum::extract::ws::Message::Close(None),
                        UpstreamMessage::Frame(_) => continue,
                    };
                    if client.send(mapped).await.is_err() || close { break }
                }
                _ => break,
            },
        }
    }
}
