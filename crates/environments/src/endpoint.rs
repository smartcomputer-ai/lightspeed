//! Environment endpoints: where Lightspeed dials a provider controller or an
//! environment's data plane.
//!
//! An endpoint is one validated URL whose scheme selects the dialer from a
//! closed set; there is no separate transport enum. Every scheme carries
//! WebSocket text frames running the environment protocol's JSON-RPC
//! envelope:
//!
//! - `ws://host[:port][/path]` and `wss://host[:port][/path]`: TCP, plain or
//!   TLS.
//! - `unix:///absolute/socket/path`: a local Unix socket (envd's Unix
//!   listener). The dialer connects to the socket and performs the
//!   WebSocket upgrade with request target `/` and host `localhost`.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

/// Longest endpoint URL in bytes.
pub const MAX_ENDPOINT_BYTES: usize = 2048;

/// The dialer an endpoint selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EndpointScheme {
    Ws,
    Wss,
    Unix,
}

impl EndpointScheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ws => "ws",
            Self::Wss => "wss",
            Self::Unix => "unix",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum EndpointError {
    #[error("endpoint is empty")]
    Empty,

    #[error("endpoint is longer than {MAX_ENDPOINT_BYTES} bytes")]
    TooLong,

    #[error("endpoint must not contain whitespace or control characters: {0}")]
    InvalidCharacter(String),

    #[error("endpoint scheme must be ws://, wss:// or unix://: {0}")]
    Scheme(String),

    #[error("endpoint has no host: {0}")]
    NoHost(String),

    #[error("endpoint must not carry user information: {0}")]
    UserInfo(String),

    #[error("endpoint must not carry a fragment: {0}")]
    Fragment(String),

    #[error("endpoint has an invalid port: {0}")]
    Port(String),

    #[error("unix endpoint must name an absolute socket path without query: {0}")]
    UnixPath(String),
}

/// A validated endpoint URL. Serializes as the URL string.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EnvironmentEndpoint {
    url: String,
    scheme: EndpointScheme,
}

impl EnvironmentEndpoint {
    pub fn parse(value: &str) -> Result<Self, EndpointError> {
        if value.is_empty() {
            return Err(EndpointError::Empty);
        }
        if value.len() > MAX_ENDPOINT_BYTES {
            return Err(EndpointError::TooLong);
        }
        if value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(EndpointError::InvalidCharacter(value.to_owned()));
        }
        let (scheme, rest) = value
            .split_once("://")
            .ok_or_else(|| EndpointError::Scheme(value.to_owned()))?;
        let scheme = match scheme {
            "ws" => EndpointScheme::Ws,
            "wss" => EndpointScheme::Wss,
            "unix" => EndpointScheme::Unix,
            _ => return Err(EndpointError::Scheme(value.to_owned())),
        };
        if rest.contains('#') {
            return Err(EndpointError::Fragment(value.to_owned()));
        }
        match scheme {
            EndpointScheme::Unix => {
                if !rest.starts_with('/') || rest.len() == 1 || rest.contains('?') {
                    return Err(EndpointError::UnixPath(value.to_owned()));
                }
            }
            EndpointScheme::Ws | EndpointScheme::Wss => {
                let authority = rest.split(['/', '?']).next().unwrap_or_default();
                if authority.contains('@') {
                    return Err(EndpointError::UserInfo(value.to_owned()));
                }
                let host =
                    host_of(authority).ok_or_else(|| EndpointError::Port(value.to_owned()))?;
                if host.is_empty() || host == "[]" {
                    return Err(EndpointError::NoHost(value.to_owned()));
                }
            }
        }
        Ok(Self {
            url: value.to_owned(),
            scheme,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.url
    }

    pub fn scheme(&self) -> EndpointScheme {
        self.scheme
    }

    /// The socket path of a `unix://` endpoint.
    pub fn unix_socket_path(&self) -> Option<&str> {
        match self.scheme {
            EndpointScheme::Unix => self.url.strip_prefix("unix://"),
            EndpointScheme::Ws | EndpointScheme::Wss => None,
        }
    }

    /// The host of a `ws://` or `wss://` endpoint, brackets kept for IPv6.
    pub fn host(&self) -> Option<&str> {
        let rest = match self.scheme {
            EndpointScheme::Ws => self.url.strip_prefix("ws://")?,
            EndpointScheme::Wss => self.url.strip_prefix("wss://")?,
            EndpointScheme::Unix => return None,
        };
        host_of(rest.split(['/', '?']).next().unwrap_or_default())
    }

    /// True for a Unix socket or a `localhost` or loopback-address host:
    /// endpoints whose traffic never leaves the machine.
    pub fn is_local(&self) -> bool {
        match self.scheme {
            EndpointScheme::Unix => true,
            EndpointScheme::Ws | EndpointScheme::Wss => self.host().is_some_and(|host| {
                let host = host.trim_start_matches('[').trim_end_matches(']');
                host.eq_ignore_ascii_case("localhost")
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|address| address.is_loopback())
            }),
        }
    }
}

fn host_of(authority: &str) -> Option<&str> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (address, after) = rest.split_once(']')?;
        if !after.is_empty() {
            after.strip_prefix(':')?.parse::<u16>().ok()?;
        }
        return Some(&authority[..address.len() + 2]);
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => {
            port.parse::<u16>().ok()?;
            Some(host)
        }
        None => Some(authority),
    }
}

impl fmt::Display for EnvironmentEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.url.fmt(formatter)
    }
}

impl FromStr for EnvironmentEndpoint {
    type Err = EndpointError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for EnvironmentEndpoint {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.url)
    }
}

impl<'de> Deserialize<'de> for EnvironmentEndpoint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scheme_selects_the_dialer() {
        let tcp = EnvironmentEndpoint::parse("wss://incus.example.com:19090/control").unwrap();
        assert_eq!(tcp.scheme(), EndpointScheme::Wss);
        assert_eq!(tcp.host(), Some("incus.example.com"));
        assert!(!tcp.is_local());
        let local =
            EnvironmentEndpoint::parse("ws://127.0.0.1:18080/environment-gateway/routes/u/e/i")
                .unwrap();
        assert!(local.is_local());
        let unix = EnvironmentEndpoint::parse("unix:///run/lightspeed/envd.sock").unwrap();
        assert_eq!(unix.scheme(), EndpointScheme::Unix);
        assert_eq!(unix.unix_socket_path(), Some("/run/lightspeed/envd.sock"));
        assert!(unix.is_local());
        assert!(
            EnvironmentEndpoint::parse("ws://[::1]:9/x")
                .unwrap()
                .is_local()
        );
    }

    #[test]
    fn other_schemes_and_malformed_urls_are_rejected() {
        for bad in [
            "",
            "http://host/control",
            "ssh://host",
            "stdio",
            "ws://",
            "ws://user@host",
            "ws://host:port",
            "ws://host/a b",
            "ws://host/#frag",
            "unix://relative.sock",
            "unix:///",
        ] {
            assert!(EnvironmentEndpoint::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn endpoints_serialize_as_strings_and_validate_on_decode() {
        let endpoint: EnvironmentEndpoint =
            serde_json::from_value(serde_json::json!("wss://gw.example.com/x")).unwrap();
        assert_eq!(
            serde_json::to_value(&endpoint).unwrap(),
            "wss://gw.example.com/x"
        );
        assert!(
            serde_json::from_value::<EnvironmentEndpoint>(serde_json::json!("http://x")).is_err()
        );
    }
}
