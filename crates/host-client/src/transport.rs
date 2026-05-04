//! Transport implementations for host protocol clients.

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        http::{
            HeaderName, HeaderValue,
            header::{AUTHORIZATION, USER_AGENT},
        },
    },
};

use crate::{
    error::{HostClientError, HostClientResult},
    rpc::JsonRpcTransport,
};

pub struct WebSocketTransport {
    stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl WebSocketTransport {
    pub async fn connect(
        endpoint: &str,
        options: WebSocketConnectOptions,
    ) -> HostClientResult<Self> {
        let mut request = endpoint
            .into_client_request()
            .map_err(|error| HostClientError::Transport(error.to_string()))?;

        if let Some(user_agent) = options.user_agent {
            let value = HeaderValue::from_str(&user_agent)
                .map_err(|error| HostClientError::Transport(error.to_string()))?;
            request.headers_mut().insert(USER_AGENT, value);
        }

        if let Some(token) = options.bearer_token {
            let value = HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|error| HostClientError::Transport(error.to_string()))?;
            request.headers_mut().insert(AUTHORIZATION, value);
        }

        for (name, value) in options.headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|error| HostClientError::Transport(error.to_string()))?;
            let value = HeaderValue::from_str(&value)
                .map_err(|error| HostClientError::Transport(error.to_string()))?;
            request.headers_mut().insert(name, value);
        }

        let (stream, _) = connect_async(request)
            .await
            .map_err(|error| HostClientError::Transport(error.to_string()))?;
        Ok(Self { stream })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WebSocketConnectOptions {
    pub bearer_token: Option<String>,
    pub user_agent: Option<String>,
    pub headers: Vec<(String, String)>,
}

#[async_trait]
impl JsonRpcTransport for WebSocketTransport {
    async fn send(&mut self, message: Value) -> HostClientResult<()> {
        self.stream
            .send(Message::Text(message.to_string().into()))
            .await
            .map_err(|error| HostClientError::Transport(error.to_string()))
    }

    async fn recv(&mut self) -> HostClientResult<Option<Value>> {
        loop {
            let Some(message) = self.stream.next().await else {
                return Ok(None);
            };
            let message = message.map_err(|error| HostClientError::Transport(error.to_string()))?;
            match message {
                Message::Text(text) => {
                    return serde_json::from_str(&text).map(Some).map_err(Into::into);
                }
                Message::Binary(bytes) => {
                    return serde_json::from_slice(&bytes).map(Some).map_err(Into::into);
                }
                Message::Close(_) => return Ok(None),
                Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
            }
        }
    }
}
