//! Connection settings.

use std::{fmt, time::Duration};

use environment_protocol::{
    bytes::SecretString, handshake::ImplementationInfo, limits::KEEPALIVE_INTERVAL_MS,
};

/// Default bound of connect, TLS and WebSocket upgrade together.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Default bound of the `initialize` round trip.
pub const DEFAULT_INITIALIZE_TIMEOUT: Duration = Duration::from_secs(10);

/// How to dial and greet an endpoint.
#[derive(Clone)]
pub struct ConnectOptions {
    /// Sent as `clientInfo` in `initialize`, and as the default
    /// `User-Agent` (`name/version`).
    pub client_info: ImplementationInfo,
    /// Sent as `Authorization: Bearer <token>` on the upgrade request.
    pub bearer_token: Option<SecretString>,
    /// Overrides the `User-Agent` derived from `client_info`.
    pub user_agent: Option<String>,
    /// Extra upgrade request headers.
    pub headers: Vec<(String, String)>,
    /// Bound of TCP or Unix connect, TLS and the WebSocket upgrade
    /// together. Default [`DEFAULT_CONNECT_TIMEOUT`].
    pub connect_timeout: Duration,
    /// Bound of the `initialize` round trip. Default
    /// [`DEFAULT_INITIALIZE_TIMEOUT`].
    pub initialize_timeout: Duration,
    /// Default deadline of every call made through the client; `None`
    /// means calls wait as long as the caller does (tool paths rely on
    /// their activity deadline). Background paths set one. Default `None`.
    pub request_timeout: Option<Duration>,
    /// Ping interval while any call is pending (the waiting peer keeps
    /// intermediaries from idle-closing a long wait). Default
    /// [`KEEPALIVE_INTERVAL_MS`].
    pub keepalive_interval: Duration,
}

impl ConnectOptions {
    pub fn new(client_info: ImplementationInfo) -> Self {
        Self {
            client_info,
            bearer_token: None,
            user_agent: None,
            headers: Vec::new(),
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            initialize_timeout: DEFAULT_INITIALIZE_TIMEOUT,
            request_timeout: None,
            keepalive_interval: Duration::from_millis(KEEPALIVE_INTERVAL_MS),
        }
    }

    pub fn with_bearer_token(mut self, token: impl Into<SecretString>) -> Self {
        self.bearer_token = Some(token.into());
        self
    }

    pub fn with_user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = Some(user_agent.into());
        self
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = Some(timeout);
        self
    }

    /// The `User-Agent` sent on the upgrade request.
    pub(crate) fn effective_user_agent(&self) -> String {
        if let Some(user_agent) = &self.user_agent {
            return user_agent.clone();
        }
        match self.client_info.version() {
            Some(version) => format!("{}/{version}", self.client_info.name()),
            None => self.client_info.name().to_owned(),
        }
    }
}

impl fmt::Debug for ConnectOptions {
    /// Header values and the bearer token are left out: they may carry
    /// credentials.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConnectOptions")
            .field("client_info", &self.client_info)
            .field("bearer_token", &self.bearer_token)
            .field("user_agent", &self.user_agent)
            .field(
                "headers",
                &self
                    .headers
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect::<Vec<_>>(),
            )
            .field("connect_timeout", &self.connect_timeout)
            .field("initialize_timeout", &self.initialize_timeout)
            .field("request_timeout", &self.request_timeout)
            .field("keepalive_interval", &self.keepalive_interval)
            .finish()
    }
}
