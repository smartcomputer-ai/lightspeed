//! Endpoint dialing: `ws://`, `wss://` and `unix:///path`, with the
//! protocol's message limits and the caller's headers.

use std::time::Duration;

use environment_protocol::{
    limits::MAX_MESSAGE_BYTES,
    routes::{DataRouteRefusal, parse_retry_after_ms},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::UnixStream,
};
use tokio_tungstenite::{
    WebSocketStream, client_async_with_config, connect_async_with_config,
    tungstenite::{
        self,
        client::IntoClientRequest,
        handshake::client::Request,
        http::{
            HeaderMap, HeaderName, HeaderValue,
            header::{AUTHORIZATION, RETRY_AFTER, USER_AGENT},
        },
        protocol::WebSocketConfig,
    },
};

use crate::{error::ClientError, options::ConnectOptions};

/// The WebSocket settings of every client socket: one explicit limit for
/// messages and frames, the protocol's [`MAX_MESSAGE_BYTES`].
pub(crate) fn websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES as usize))
        .max_frame_size(Some(MAX_MESSAGE_BYTES as usize))
}

enum Target<'a> {
    /// `ws://` or `wss://`: dialed as given.
    Url,
    /// `unix:///path`: the socket path; the upgrade targets `/` with
    /// `Host: localhost`.
    Unix(&'a str),
}

fn target(endpoint: &str) -> Result<Target<'_>, ClientError> {
    if endpoint.starts_with("ws://") || endpoint.starts_with("wss://") {
        return Ok(Target::Url);
    }
    if let Some(path) = endpoint.strip_prefix("unix://") {
        if path.len() > 1 && path.starts_with('/') && !path.contains(['?', '#']) {
            return Ok(Target::Unix(path));
        }
        return Err(configuration(
            endpoint,
            "a unix endpoint names an absolute socket path: unix:///path/to/socket",
        ));
    }
    Err(configuration(
        endpoint,
        "the endpoint scheme must be ws://, wss:// or unix://",
    ))
}

fn configuration(endpoint: &str, message: impl Into<String>) -> ClientError {
    ClientError::Configuration {
        endpoint: endpoint.to_owned(),
        message: message.into(),
    }
}

fn upgrade_request(
    endpoint: &str,
    url: &str,
    options: &ConnectOptions,
) -> Result<Request, ClientError> {
    let mut request = url
        .into_client_request()
        .map_err(|error| configuration(endpoint, error.to_string()))?;
    let headers = request.headers_mut();
    let user_agent = HeaderValue::from_str(&options.effective_user_agent())
        .map_err(|_| configuration(endpoint, "the user agent is not a valid header value"))?;
    headers.insert(USER_AGENT, user_agent);
    if let Some(token) = &options.bearer_token {
        let mut value = HeaderValue::from_str(&format!("Bearer {}", token.expose()))
            .map_err(|_| configuration(endpoint, "the bearer token is not a valid header value"))?;
        value.set_sensitive(true);
        headers.insert(AUTHORIZATION, value);
    }
    for (name, value) in &options.headers {
        let header = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| configuration(endpoint, format!("invalid header name {name:?}")))?;
        let value = HeaderValue::from_str(value).map_err(|_| {
            configuration(endpoint, format!("header {name:?} has an invalid value"))
        })?;
        headers.insert(header, value);
    }
    Ok(request)
}

fn dial_error(endpoint: &str, error: tungstenite::Error) -> ClientError {
    match error {
        tungstenite::Error::Http(response) => {
            let status = response.status().as_u16();
            let retry_after_ms = retry_after_ms(response.headers());
            let refusal = DataRouteRefusal::from_refused_upgrade(
                status,
                response.body().as_deref(),
                retry_after_ms,
            );
            ClientError::UpgradeRejected {
                status,
                refusal,
                retry_after_ms,
            }
        }
        tungstenite::Error::Url(error) => configuration(endpoint, error.to_string()),
        error => ClientError::Connect {
            endpoint: endpoint.to_owned(),
            message: error.to_string(),
        },
    }
}

fn retry_after_ms(headers: &HeaderMap) -> Option<u64> {
    parse_retry_after_ms(headers.get(RETRY_AFTER)?.to_str().ok()?)
}

async fn bounded<T>(
    endpoint: &str,
    timeout: Duration,
    dial: impl Future<Output = Result<T, ClientError>>,
) -> Result<T, ClientError> {
    tokio::time::timeout(timeout, dial)
        .await
        .map_err(|_| ClientError::ConnectTimeout {
            endpoint: endpoint.to_owned(),
        })?
}

/// A dialed socket of either transport.
pub(crate) enum Dialed {
    Tcp(Box<WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>>),
    Unix(Box<WebSocketStream<UnixStream>>),
}

/// Dial `endpoint` and complete the WebSocket upgrade within
/// `options.connect_timeout`.
pub(crate) async fn dial(endpoint: &str, options: &ConnectOptions) -> Result<Dialed, ClientError> {
    match target(endpoint)? {
        Target::Url => {
            let request = upgrade_request(endpoint, endpoint, options)?;
            bounded(endpoint, options.connect_timeout, async {
                connect_async_with_config(request, Some(websocket_config()), true)
                    .await
                    .map(|(socket, _)| Dialed::Tcp(Box::new(socket)))
                    .map_err(|error| dial_error(endpoint, error))
            })
            .await
        }
        Target::Unix(path) => {
            let request = upgrade_request(endpoint, "ws://localhost/", options)?;
            bounded(endpoint, options.connect_timeout, async {
                let stream =
                    UnixStream::connect(path)
                        .await
                        .map_err(|error| ClientError::Connect {
                            endpoint: endpoint.to_owned(),
                            message: error.to_string(),
                        })?;
                client_async_with_config(request, stream, Some(websocket_config()))
                    .await
                    .map(|(socket, _)| Dialed::Unix(Box::new(socket)))
                    .map_err(|error| dial_error(endpoint, error))
            })
            .await
        }
    }
}

/// Complete the WebSocket upgrade over an already connected byte stream
/// (request target `/`, `Host: localhost`) within
/// `options.connect_timeout`.
pub(crate) async fn upgrade<S>(
    stream: S,
    options: &ConnectOptions,
) -> Result<WebSocketStream<S>, ClientError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    const ENDPOINT: &str = "ws://localhost/";
    let request = upgrade_request(ENDPOINT, ENDPOINT, options)?;
    bounded(ENDPOINT, options.connect_timeout, async {
        client_async_with_config(request, stream, Some(websocket_config()))
            .await
            .map(|(socket, _)| socket)
            .map_err(|error| dial_error(ENDPOINT, error))
    })
    .await
}

#[cfg(test)]
mod tests {
    use environment_protocol::{handshake::ImplementationInfo, limits::RETRY_AFTER_MAX_SECS};

    use super::*;

    fn options() -> ConnectOptions {
        ConnectOptions::new(
            ImplementationInfo::new("lightspeed-test")
                .and_then(|info| info.with_version("1.0.0"))
                .unwrap(),
        )
    }

    #[test]
    fn endpoints_select_their_dialer() {
        assert!(matches!(target("ws://127.0.0.1:1/x"), Ok(Target::Url)));
        assert!(matches!(target("wss://example.test"), Ok(Target::Url)));
        assert!(matches!(
            target("unix:///run/envd.sock"),
            Ok(Target::Unix("/run/envd.sock"))
        ));
        for bad in [
            "http://example.test",
            "unix://relative.sock",
            "unix:///",
            "unix:///run/envd.sock?x=1",
            "stdio://",
        ] {
            assert!(
                matches!(target(bad), Err(ClientError::Configuration { .. })),
                "{bad}"
            );
        }
    }

    #[test]
    fn retry_after_is_whole_seconds_clamped_to_the_maximum() {
        let header = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(RETRY_AFTER, HeaderValue::from_str(value).unwrap());
            retry_after_ms(&headers)
        };
        assert_eq!(header("3"), Some(3_000));
        assert_eq!(header("3600"), Some(RETRY_AFTER_MAX_SECS * 1_000));
        assert_eq!(header("Wed, 21 Oct 2026 07:28:00 GMT"), None);
        assert_eq!(retry_after_ms(&HeaderMap::new()), None);
    }

    #[test]
    fn rejected_upgrades_decode_a_refusal_body() {
        let response = tungstenite::http::Response::builder()
            .status(503)
            .header(RETRY_AFTER, "2")
            .body(Some(br#"{"state":"booting"}"#.to_vec()))
            .unwrap();
        let error = dial_error("ws://h/", tungstenite::Error::Http(Box::new(response)));
        assert!(
            matches!(
                &error,
                ClientError::UpgradeRejected {
                    status: 503,
                    refusal: Some(DataRouteRefusal::Booting {}),
                    retry_after_ms: Some(2_000),
                }
            ),
            "{error:?}"
        );
        let garbled = tungstenite::http::Response::builder()
            .status(503)
            .body(Some(b"<html>busy</html>".to_vec()))
            .unwrap();
        assert!(matches!(
            dial_error("ws://h/", tungstenite::Error::Http(Box::new(garbled))),
            ClientError::UpgradeRejected {
                status: 503,
                refusal: None,
                retry_after_ms: None,
            }
        ));
    }

    #[test]
    fn a_refusal_whose_body_was_not_read_falls_back_on_its_status() {
        let refused = |status: u16, retry_after: Option<&str>, body: Option<Vec<u8>>| {
            let mut response = tungstenite::http::Response::builder().status(status);
            if let Some(retry_after) = retry_after {
                response = response.header(RETRY_AFTER, retry_after);
            }
            match dial_error(
                "ws://h/",
                tungstenite::Error::Http(Box::new(response.body(body).unwrap())),
            ) {
                ClientError::UpgradeRejected {
                    refusal,
                    retry_after_ms,
                    ..
                } => (refusal, retry_after_ms),
                error => panic!("expected a refused upgrade, got {error:?}"),
            }
        };
        // The tail read with the headers is empty, or there is none.
        assert_eq!(
            refused(410, None, Some(Vec::new())),
            (Some(DataRouteRefusal::Gone {}), None)
        );
        assert_eq!(
            refused(410, None, None),
            (Some(DataRouteRefusal::Gone {}), None)
        );
        assert!(matches!(
            refused(409, None, Some(Vec::new())),
            (Some(DataRouteRefusal::Failed { .. }), None)
        ));
        assert_eq!(
            refused(503, Some("4"), Some(Vec::new())),
            (Some(DataRouteRefusal::Booting {}), Some(4_000))
        );
        // A partial body does not decode either.
        assert_eq!(
            refused(503, Some("4"), Some(br#"{"state":"wak"#.to_vec())),
            (Some(DataRouteRefusal::Booting {}), Some(4_000))
        );
        // A decoded body wins over the status.
        assert_eq!(
            refused(503, Some("4"), Some(br#"{"state":"waking"}"#.to_vec())),
            (Some(DataRouteRefusal::Waking {}), Some(4_000))
        );
        // Statuses that name no refusal.
        assert_eq!(refused(503, None, Some(Vec::new())), (None, None));
        assert_eq!(refused(401, None, None), (None, None));
        assert_eq!(refused(404, Some("4"), None), (None, Some(4_000)));
    }

    #[test]
    fn upgrade_requests_carry_agent_credential_and_headers() {
        let request = upgrade_request(
            "ws://h/",
            "ws://h/",
            &options()
                .with_bearer_token("secret")
                .with_header("x-lightspeed-test", "1"),
        )
        .unwrap();
        let headers = request.headers();
        assert_eq!(headers[USER_AGENT], "lightspeed-test/1.0.0");
        assert_eq!(headers[AUTHORIZATION], "Bearer secret");
        assert!(headers[AUTHORIZATION].is_sensitive());
        assert_eq!(headers["x-lightspeed-test"], "1");
        assert!(matches!(
            upgrade_request(
                "ws://h/",
                "ws://h/",
                &options().with_header("bad name", "1")
            ),
            Err(ClientError::Configuration { .. })
        ));
    }
}
