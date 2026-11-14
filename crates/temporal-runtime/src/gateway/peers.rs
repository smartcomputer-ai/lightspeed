//! Authenticated, direct routing between environment gateway replicas.
//! Tokens name the acceptor, never a client-supplied URL. Only that acceptor
//! can consume the pending stream; a receiver forwards it at most once.

use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json,
    extract::{State, WebSocketUpgrade},
    http::{HeaderMap, HeaderValue, StatusCode, header::AUTHORIZATION},
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use environment_protocol::{ids::ControlConnectionId, limits::MAX_MESSAGE_BYTES};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{client::IntoClientRequest as _, protocol::WebSocketConfig},
};
use uuid::Uuid;

use super::{
    http::GatewayState,
    registration::{self, STREAM_OPEN_TIMEOUT},
};
use crate::environments::gateway::{RouteKey, bearer_matches};

pub(super) const STREAMS_PATH: &str = "/environment-gateway/peer/streams";
pub(super) const DATA_PATH: &str = "/environment-gateway/peer/data";
pub(super) const OWNER_LOOKUP_TIMEOUT: Duration = Duration::from_secs(3);
const STREAM_TOKEN_HEADER: &str = "lightspeed-stream-token";
const TOKEN_DOMAIN: &[u8] = b"lightspeed/environment-gateway/stream/v1\0";
const TOKEN_CLOCK_ALLOWANCE: Duration = Duration::from_secs(5);

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StreamRequest {
    universe_id: Uuid,
    environment_id: String,
    control_connection_id: String,
    token: String,
}

pub(super) fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

pub(super) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(STREAM_OPEN_TIMEOUT)
        .build()
        .expect("gateway peer HTTP client configuration is valid")
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn token_mac(key: &str, payload: &str) -> Hmac<Sha256> {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("HMAC accepts any key length");
    mac.update(TOKEN_DOMAIN);
    mac.update(payload.as_bytes());
    mac
}

pub(super) fn new_token(acceptor: Uuid, key: &str) -> String {
    use rand::TryRng as _;
    let mut nonce = [0u8; 32];
    rand::rngs::SysRng
        .try_fill_bytes(&mut nonce)
        .expect("OS random source unavailable");
    let expires = now_seconds() + STREAM_OPEN_TIMEOUT.as_secs() + TOKEN_CLOCK_ALLOWANCE.as_secs();
    token_with_nonce(acceptor, expires, &nonce, key)
}

fn token_with_nonce(acceptor: Uuid, expires: u64, nonce: &[u8; 32], key: &str) -> String {
    let payload = format!("{acceptor}.{expires}.{}", URL_SAFE_NO_PAD.encode(nonce));
    let signature = token_mac(key, &payload).finalize().into_bytes();
    format!("{payload}.{}", URL_SAFE_NO_PAD.encode(signature))
}

pub(super) fn token_acceptor(token: &str, key: &str) -> Option<Uuid> {
    token_acceptor_at(token, key, now_seconds())
}

fn token_acceptor_at(token: &str, key: &str, now: u64) -> Option<Uuid> {
    if token.len() > 192 {
        return None;
    }
    let (payload, signature) = token.rsplit_once('.')?;
    let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
    token_mac(key, payload).verify_slice(&signature).ok()?;
    let mut parts = payload.split('.');
    let acceptor = Uuid::parse_str(parts.next()?).ok()?;
    let expires: u64 = parts.next()?.parse().ok()?;
    let nonce = URL_SAFE_NO_PAD.decode(parts.next()?).ok()?;
    if parts.next().is_some()
        || nonce.len() != 32
        || expires <= now
        || expires
            > now
                .saturating_add(STREAM_OPEN_TIMEOUT.as_secs() + 2 * TOKEN_CLOCK_ALLOWANCE.as_secs())
    {
        return None;
    }
    Some(acceptor)
}

fn endpoint(base: &str, path: &str, websocket: bool) -> Result<String, StatusCode> {
    let mut url = url::Url::parse(base).map_err(|_| StatusCode::BAD_GATEWAY)?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(StatusCode::BAD_GATEWAY);
    }
    let scheme = match (url.scheme(), websocket) {
        ("http" | "ws", true) => "ws",
        ("https" | "wss", true) => "wss",
        ("http" | "ws", false) => "http",
        ("https" | "wss", false) => "https",
        _ => return Err(StatusCode::BAD_GATEWAY),
    };
    url.set_scheme(scheme)
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    url.set_path(path);
    Ok(url.to_string())
}

pub(super) async fn request_stream(
    state: &GatewayState,
    peer_url: &str,
    route: &RouteKey,
    connection_id: &ControlConnectionId,
    token: &str,
) -> Result<(), StatusCode> {
    let response = state
        .peer_http()
        .post(endpoint(peer_url, STREAMS_PATH, false)?)
        .bearer_auth(state.environment_gateway_token())
        .json(&StreamRequest {
            universe_id: route.universe_id,
            environment_id: route.environment_id.clone(),
            control_connection_id: connection_id.to_string(),
            token: token.to_owned(),
        })
        .send()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    match response.status() {
        StatusCode::NO_CONTENT => Ok(()),
        StatusCode::CONFLICT => Err(StatusCode::CONFLICT),
        status if status.is_server_error() => Err(StatusCode::SERVICE_UNAVAILABLE),
        _ => Err(StatusCode::BAD_GATEWAY),
    }
}

pub(super) async fn open_stream(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    Json(request): Json<StreamRequest>,
) -> Response {
    if !bearer_matches(&headers, state.environment_gateway_token()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !state.replica().is_accepting() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if token_acceptor(&request.token, state.environment_gateway_token()).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(connection_id) = ControlConnectionId::new(request.control_connection_id) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let route = RouteKey {
        universe_id: request.universe_id,
        environment_id: request.environment_id,
    };
    match tokio::time::timeout(
        OWNER_LOOKUP_TIMEOUT,
        store_pg::read_registered_connection_owner(
            state.pool(),
            route.universe_id,
            &route.environment_id,
        ),
    )
    .await
    {
        Ok(Ok(Some(owner)))
            if owner.owner_instance_id == state.replica().instance_id()
                && owner.control_connection_id == connection_id.as_str() => {}
        Ok(Ok(_)) => return StatusCode::CONFLICT.into_response(),
        _ => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    match tokio::time::timeout(
        STREAM_OPEN_TIMEOUT,
        state
            .registrations()
            .request_stream(&route, &connection_id, request.token),
    )
    .await
    {
        Ok(Ok(())) => StatusCode::NO_CONTENT,
        Ok(Err(status)) => status,
        Err(_) => StatusCode::GATEWAY_TIMEOUT,
    }
    .into_response()
}

/// Internal handoff only pairs locally. It cannot forward again.
pub(super) async fn data_upgrade(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !bearer_matches(&headers, state.environment_gateway_token()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !state.replica().lease_valid() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let Some(token) = headers
        .get(STREAM_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if token_acceptor(token, state.environment_gateway_token())
        != Some(state.replica().instance_id())
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    registration::pair_data(&state, token, upgrade).await
}

pub(super) async fn relay_data(
    state: Arc<GatewayState>,
    token: String,
    acceptor: Uuid,
    upgrade: WebSocketUpgrade,
) -> Response {
    let instance = match tokio::time::timeout(
        OWNER_LOOKUP_TIMEOUT,
        store_pg::read_live_runtime_instance(state.pool(), acceptor),
    )
    .await
    {
        Ok(Ok(Some(instance)))
            if instance
                .roles
                .iter()
                .any(|role| role == "environment-gateway") =>
        {
            instance
        }
        Ok(Ok(_)) => return StatusCode::UNAUTHORIZED.into_response(),
        _ => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let Some(peer_url) = instance.peer_url.as_deref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let url = match endpoint(peer_url, DATA_PATH, true) {
        Ok(url) => url,
        Err(status) => return status.into_response(),
    };
    let Ok(mut request) = url.into_client_request() else {
        return StatusCode::BAD_GATEWAY.into_response();
    };
    let Ok(mut bearer) =
        HeaderValue::from_str(&format!("Bearer {}", state.environment_gateway_token()))
    else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    bearer.set_sensitive(true);
    request.headers_mut().insert(AUTHORIZATION, bearer);
    let Ok(mut token_header) = HeaderValue::from_str(&token) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    token_header.set_sensitive(true);
    request
        .headers_mut()
        .insert(STREAM_TOKEN_HEADER, token_header);
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES as usize))
        .max_frame_size(Some(MAX_MESSAGE_BYTES as usize));
    let upstream = match tokio::time::timeout(
        STREAM_OPEN_TIMEOUT,
        connect_async_with_config(request, Some(config), true),
    )
    .await
    {
        Ok(Ok((socket, _))) => socket,
        Ok(Err(_)) => return StatusCode::BAD_GATEWAY.into_response(),
        Err(_) => return StatusCode::GATEWAY_TIMEOUT.into_response(),
    };
    if !state.replica().lease_valid() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let guard = state.registrations().track_proxy();
    upgrade
        .max_message_size(MAX_MESSAGE_BYTES as usize)
        .max_frame_size(MAX_MESSAGE_BYTES as usize)
        .on_upgrade(move |socket| async move {
            let _guard = guard;
            let (_keep_alive, revoked) = tokio::sync::watch::channel(0u64);
            tokio::select! {
                _ = state.replica().stopped() => {},
                _ = super::http::proxy_frames(revoked, socket, upstream) => {},
            }
        })
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_authenticate_the_acceptor_and_expiry() {
        let acceptor = Uuid::new_v4();
        let token = token_with_nonce(acceptor, 110, &[7; 32], "secret");
        assert_eq!(token_acceptor_at(&token, "secret", 100), Some(acceptor));
        assert_eq!(token_acceptor_at(&token, "other-secret", 100), None);
        assert_eq!(token_acceptor_at(&token, "secret", 110), None);
        assert_eq!(token_acceptor_at(&token, "secret", 50), None);
        let changed = token.replacen(&acceptor.to_string(), &Uuid::new_v4().to_string(), 1);
        assert_eq!(token_acceptor_at(&changed, "secret", 100), None);
        for malformed in ["", "..", "../../../peer", "https://attacker.example"] {
            assert_eq!(token_acceptor_at(malformed, "secret", 100), None);
        }
    }

    #[test]
    fn peer_endpoints_are_direct_and_do_not_carry_credentials() {
        assert_eq!(
            endpoint("http://127.0.0.1:8080", DATA_PATH, true).unwrap(),
            "ws://127.0.0.1:8080/environment-gateway/peer/data"
        );
        assert_eq!(
            endpoint("https://gateway.test:8443", STREAMS_PATH, false).unwrap(),
            "https://gateway.test:8443/environment-gateway/peer/streams"
        );
        for rejected in [
            "file:///tmp/socket",
            "http://secret@gateway.test",
            "http://gateway.test?token=secret",
            "http://gateway.test#fragment",
        ] {
            assert!(endpoint(rejected, DATA_PATH, true).is_err());
        }
    }
}
