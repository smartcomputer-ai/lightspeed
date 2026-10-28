//! Public route paths and the URL rules both peers must compute the same
//! way.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use thiserror::Error;

use crate::{
    control::target::TargetOwner,
    ids::ProviderTargetId,
    limits::{MAX_URL_BYTES, RETRY_AFTER_MAX_SECS},
    metadata::DescriptiveText,
};

/// Gateway route an envd dials to register and hold its control connection.
/// A gateway may be served under a path prefix (`wss://host/ls` plus this
/// path); the daemon is configured with the full connect URL.
pub const CONNECT_PATH: &str = "/environment-gateway/connect";

/// Gateway route an envd dials, with a stream token as a bearer header, to
/// serve one reverse-dialed stream. Always derived from the connect URL by
/// replacing its [`CONNECT_PATH`] suffix ([`gateway_stream_url`]), so it
/// shares the connect URL's origin and path prefix.
pub const DATA_PATH: &str = "/environment-gateway/data";

/// Gateway route serving the envd discovery document.
pub const DISCOVERY_PATH: &str = "/.well-known/lightspeed-envd";

/// Path of the data routes under a controller's origin when the provider
/// is registered without a data route base
/// ([`DataRouteBase::derived_from_controller`]).
pub const DEFAULT_DATA_ROUTE_PATH: &str = "/routes";

/// JSON Schema pattern of [`DataRouteBase`].
pub const DATA_ROUTE_BASE_PATTERN: &str = "^wss?://[^/?#@\\s]+(/[^?#\\s]*[^/?#\\s])?$";

/// JSON Schema pattern of [`PublicUrl`].
pub const PUBLIC_URL_PATTERN: &str = "^https?://[^/?#@\\s]+([/?][^#\\s]*)?$";

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum UrlError {
    #[error("URL has an unsupported scheme: {0}")]
    Scheme(String),

    #[error("URL has no host: {0}")]
    NoHost(String),

    #[error("URL must not carry user information: {0}")]
    UserInfo(String),

    #[error("URL has an invalid port: {0}")]
    Port(String),

    #[error("URL must not carry a query or fragment: {0}")]
    QueryOrFragment(String),

    #[error("URL must not end with '/': {0}")]
    TrailingSlash(String),

    #[error("URL is longer than {MAX_URL_BYTES} bytes")]
    TooLong,

    #[error("URL contains whitespace or a control character: {0}")]
    Character(String),

    #[error("gateway connect URL path must end with {CONNECT_PATH}: {0}")]
    NotConnectPath(String),

    #[error("a controller reached over wss needs a wss data route base: {0}")]
    Downgrade(String),
}

/// The parts of an absolute URL the protocol's rules look at.
struct UrlParts<'a> {
    /// As written.
    scheme: &'a str,
    host: &'a str,
    port: Option<u16>,
    path: &'a str,
    query: Option<&'a str>,
    fragment: Option<&'a str>,
}

fn split_url(url: &str) -> Result<UrlParts<'_>, UrlError> {
    if url.len() > MAX_URL_BYTES as usize {
        return Err(UrlError::TooLong);
    }
    if url
        .chars()
        .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(UrlError::Character(url.to_owned()));
    }
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| UrlError::Scheme(url.to_owned()))?;
    let (rest, fragment) = match rest.split_once('#') {
        Some((rest, fragment)) => (rest, Some(fragment)),
        None => (rest, None),
    };
    let (rest, query) = match rest.split_once('?') {
        Some((rest, query)) => (rest, Some(query)),
        None => (rest, None),
    };
    let (authority, path) = match rest.find('/') {
        Some(index) => rest.split_at(index),
        None => (rest, ""),
    };
    if authority.contains('@') {
        return Err(UrlError::UserInfo(url.to_owned()));
    }
    let (host, port) = split_host_port(authority).ok_or_else(|| UrlError::Port(url.to_owned()))?;
    if host.is_empty() || host == "[]" {
        return Err(UrlError::NoHost(url.to_owned()));
    }
    Ok(UrlParts {
        scheme,
        host,
        port,
        path,
        query,
        fragment,
    })
}

/// The canonical WebSocket origin of a URL, the value the registration
/// signature binds. Only [`canonical_origin`] builds one, so the daemon
/// (from the URL it dialed) and the gateway (from its configured public
/// URLs) cannot pass a raw URL where an origin belongs.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CanonicalOrigin(String);

impl CanonicalOrigin {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True for a `wss` origin.
    pub fn is_secure(&self) -> bool {
        self.0.starts_with("wss://")
    }
}

impl fmt::Display for CanonicalOrigin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The canonical WebSocket origin of a URL: `ws` or `wss` (`http` maps to
/// `ws`, `https` to `wss`, any case), `://`, the lowercase host, and
/// `:port` only when it is not the scheme's default. Path, query and
/// fragment are dropped.
pub fn canonical_origin(url: &str) -> Result<CanonicalOrigin, UrlError> {
    let parts = split_url(url)?;
    let (scheme, default_port) = match parts.scheme.to_ascii_lowercase().as_str() {
        "ws" | "http" => ("ws", 80),
        "wss" | "https" => ("wss", 443),
        _ => return Err(UrlError::Scheme(url.to_owned())),
    };
    let host = parts.host.to_ascii_lowercase();
    Ok(CanonicalOrigin(match parts.port {
        Some(port) if port != default_port => format!("{scheme}://{host}:{port}"),
        _ => format!("{scheme}://{host}"),
    }))
}

/// The URL a daemon dials for a reverse-dialed stream: the canonical origin
/// of its configured gateway connect URL, the connect URL's path with its
/// [`CONNECT_PATH`] suffix replaced by [`DATA_PATH`]. A path prefix in front
/// of the gateway routes is kept. The gateway never names this URL, so a
/// stream can never be pointed at another host or a weaker scheme than the
/// control connection. A connect URL whose path does not end with
/// [`CONNECT_PATH`], or that carries a query or fragment, is refused.
pub fn gateway_stream_url(gateway_connect_url: &str) -> Result<String, UrlError> {
    let parts = split_url(gateway_connect_url)?;
    if parts.query.is_some() || parts.fragment.is_some() {
        return Err(UrlError::QueryOrFragment(gateway_connect_url.to_owned()));
    }
    let prefix = parts
        .path
        .strip_suffix(CONNECT_PATH)
        .ok_or_else(|| UrlError::NotConnectPath(gateway_connect_url.to_owned()))?;
    Ok(format!(
        "{}{prefix}{DATA_PATH}",
        canonical_origin(gateway_connect_url)?
    ))
}

/// The absolute `ws://` or `wss://` base URL of a provider's data routes,
/// validated on decode and never rewritten: lowercase scheme, a host and
/// optional port, an optional path that does not end with `/`, no user
/// information, query or fragment, at most 2048 bytes. The operator may
/// configure one when registering the provider; without one the runtime
/// derives it from the controller URL
/// ([`DataRouteBase::derived_from_controller`]). A controller reached over
/// `wss` needs a `wss` base, so a provider credential never travels in
/// cleartext; the runtime checks a configured base with
/// [`DataRouteBase::check_for_controller`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DataRouteBase(
    #[cfg_attr(
        feature = "schema",
        schemars(length(min = 1, max = 2048), pattern(DATA_ROUTE_BASE_PATTERN))
    )]
    String,
);

impl DataRouteBase {
    pub fn new(value: impl Into<String>) -> Result<Self, UrlError> {
        let value = value.into();
        let parts = split_url(&value)?;
        if !matches!(parts.scheme, "ws" | "wss") {
            return Err(UrlError::Scheme(value));
        }
        if parts.query.is_some() || parts.fragment.is_some() {
            return Err(UrlError::QueryOrFragment(value));
        }
        if parts.path.ends_with('/') {
            return Err(UrlError::TrailingSlash(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_secure(&self) -> bool {
        self.0.starts_with("wss://")
    }

    /// The base a provider registered without one uses: the controller
    /// URL's scheme, host and port as written, followed by
    /// [`DEFAULT_DATA_ROUTE_PATH`]; the controller URL's path, query and
    /// fragment are dropped. Only a `ws` or `wss` controller URL has one;
    /// any other scheme (such as `unix`) is [`UrlError::Scheme`].
    pub fn derived_from_controller(controller_url: &str) -> Result<Self, UrlError> {
        // The scheme decides first: a `unix://` controller endpoint has no
        // network origin to derive from, so it needs an explicit base.
        let scheme = controller_url.split_once("://").map(|(scheme, _)| scheme);
        if !matches!(scheme, Some("ws" | "wss")) {
            return Err(UrlError::Scheme(controller_url.to_owned()));
        }
        let parts = split_url(controller_url)?;
        let authority = match parts.port {
            Some(port) => format!("{}:{port}", parts.host),
            None => parts.host.to_owned(),
        };
        Self::new(format!(
            "{}://{authority}{DEFAULT_DATA_ROUTE_PATH}",
            parts.scheme
        ))
    }

    /// The downgrade rule: a controller whose URL is `wss` (or `https`) may
    /// only use a `wss` base. The runtime checks a configured base when the
    /// provider is registered.
    pub fn check_for_controller(&self, controller_url: &str) -> Result<(), UrlError> {
        if canonical_origin(controller_url)?.is_secure() && !self.is_secure() {
            return Err(UrlError::Downgrade(self.0.clone()));
        }
        Ok(())
    }
}

/// A public `http://` or `https://` URL a provider reports, such as a
/// target's ingress `endpoint`: lowercase scheme, a host and optional port,
/// optional path and query, no user information or fragment, at most 2048
/// bytes. Validated on decode.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PublicUrl(
    #[cfg_attr(
        feature = "schema",
        schemars(length(min = 1, max = 2048), pattern(PUBLIC_URL_PATTERN))
    )]
    String,
);

impl PublicUrl {
    pub fn new(value: impl Into<String>) -> Result<Self, UrlError> {
        let value = value.into();
        let parts = split_url(&value)?;
        if !matches!(parts.scheme, "http" | "https") {
            return Err(UrlError::Scheme(value));
        }
        if parts.fragment.is_some() {
            return Err(UrlError::QueryOrFragment(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

macro_rules! url_impls {
    ($name:ident) => {
        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl std::str::FromStr for $name {
            type Err = UrlError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(D::Error::custom)
            }
        }
    };
}

url_impls!(DataRouteBase);
url_impls!(PublicUrl);

/// A target's data route under a provider's data route base: the base
/// followed by `/{universeId}/{environmentId}/{targetId}`, each segment
/// percent-encoded. The runtime dials it with the provider credential, when
/// one is configured. Before the upgrade the provider may hold the dial up
/// to [`MAX_DATA_ROUTE_HOLD_MS`](crate::limits::MAX_DATA_ROUTE_HOLD_MS)
/// while it wakes the target, then answers a [`DataRouteRefusal`].
pub fn provider_data_route(
    base: &DataRouteBase,
    owner: &TargetOwner,
    target_id: &ProviderTargetId,
) -> String {
    format!(
        "{}/{}/{}/{}",
        base.as_str(),
        encode_path_segment(owner.universe_id.as_str()),
        encode_path_segment(owner.environment_id.as_str()),
        encode_path_segment(target_id.as_str()),
    )
}

/// Body of the HTTP refusal a data route answers instead of upgrading, with
/// `Content-Type: application/json`. A provider data route uses `waking`,
/// `booting`, `gone` and `failed`; the gateway's worker route adds
/// `provisioning`, `disconnected` and `away` and forwards a provider's
/// refusal unchanged. Every 503 carries `Retry-After` in whole seconds,
/// 1 to [`RETRY_AFTER_MAX_SECS`](crate::limits::RETRY_AFTER_MAX_SECS).
/// The body is an HTTP refusal, not a JSON-RPC message, and is tagged by
/// `state`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum DataRouteRefusal {
    /// 503: resuming from a powered-down state.
    Waking {},
    /// 503: running, envd not answering yet.
    Booting {},
    /// 503 (gateway): the environment is still provisioning.
    Provisioning {},
    /// 503 (gateway): the registered daemon is not connected.
    Disconnected {},
    /// 410 (gateway): the registered daemon has been away past its
    /// reconnect bound.
    Away {},
    /// 410: the target no longer exists.
    Gone {},
    /// 409: broken beyond the provider's recovery.
    Failed { message: DescriptiveText },
}

impl DataRouteRefusal {
    /// The HTTP status the refusal is answered with.
    pub fn status(&self) -> u16 {
        match self {
            Self::Waking {} | Self::Booting {} | Self::Provisioning {} | Self::Disconnected {} => {
                503
            }
            Self::Away {} | Self::Gone {} => 410,
            Self::Failed { .. } => 409,
        }
    }

    /// True for the 503 refusals: the same dial may succeed after
    /// `Retry-After`.
    pub fn is_retryable(&self) -> bool {
        self.status() == 503
    }

    /// The refusal a refused upgrade names: its body when that decodes,
    /// else what its status alone names. The upgrade response carries only
    /// the bytes read along with its headers, so a body that arrives in a
    /// later TCP segment or TLS record is missing; the refusal then must not
    /// depend on how the transport split the bytes. 410 is `gone`, 409 is
    /// `failed`, and a 503 with `Retry-After` is `booting`, a wait. Any other
    /// status, such as a 503 without `Retry-After` from a proxy, names no
    /// refusal.
    pub fn from_refused_upgrade(
        status: u16,
        body: Option<&[u8]>,
        retry_after_ms: Option<u64>,
    ) -> Option<Self> {
        if let Some(refusal) = body.and_then(|body| serde_json::from_slice::<Self>(body).ok()) {
            return Some(refusal);
        }
        match status {
            410 => Some(Self::Gone {}),
            409 => Some(Self::Failed {
                message: DescriptiveText::new("the data route reports the target failed")
                    .expect("a fixed failure message is valid text"),
            }),
            503 if retry_after_ms.is_some() => Some(Self::Booting {}),
            _ => None,
        }
    }

    /// The `state` tag.
    pub fn state(&self) -> &'static str {
        match self {
            Self::Waking {} => "waking",
            Self::Booting {} => "booting",
            Self::Provisioning {} => "provisioning",
            Self::Disconnected {} => "disconnected",
            Self::Away {} => "away",
            Self::Gone {} => "gone",
            Self::Failed { .. } => "failed",
        }
    }
}

impl fmt::Display for DataRouteRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed { message } => write!(formatter, "failed: {message}"),
            refusal => formatter.write_str(refusal.state()),
        }
    }
}

/// A `Retry-After` header value in whole seconds, clamped to
/// [`RETRY_AFTER_MAX_SECS`], as milliseconds. The HTTP-date form is not
/// used by the protocol and names no delay.
pub fn parse_retry_after_ms(value: &str) -> Option<u64> {
    let seconds: u64 = value.trim().parse().ok()?;
    Some(seconds.min(RETRY_AFTER_MAX_SECS) * 1_000)
}

/// Percent-encode one URL path segment: everything except RFC 3986
/// unreserved characters.
pub fn encode_path_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn split_host_port(authority: &str) -> Option<(&str, Option<u16>)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        let host = &authority[..host.len() + 2];
        return match after {
            "" => Some((host, None)),
            port => Some((host, Some(port.strip_prefix(':')?.parse().ok()?))),
        };
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Some((host, Some(port.parse().ok()?))),
        None => Some((authority, None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_are_canonical() {
        for (url, origin) in [
            (
                "wss://GW.Example.com/environment-gateway/connect",
                "wss://gw.example.com",
            ),
            ("wss://gw.example.com:443/x?y#z", "wss://gw.example.com"),
            ("https://gw.example.com:8443", "wss://gw.example.com:8443"),
            (
                "ws://127.0.0.1:18080/environment-gateway/connect",
                "ws://127.0.0.1:18080",
            ),
            ("http://[::1]:80/", "ws://[::1]"),
        ] {
            assert_eq!(canonical_origin(url).unwrap().as_str(), origin, "{url}");
        }
        for bad in [
            "ftp://x",
            "wss://",
            "wss://user@host",
            "wss://host:port",
            "host",
            "wss://host/a b",
        ] {
            assert!(canonical_origin(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn stream_urls_derive_from_the_connect_url() {
        assert_eq!(
            gateway_stream_url("wss://gw.example.com/environment-gateway/connect").unwrap(),
            "wss://gw.example.com/environment-gateway/data"
        );
        assert_eq!(
            gateway_stream_url("wss://GW.example.com:443/ls/environment-gateway/connect").unwrap(),
            "wss://gw.example.com/ls/environment-gateway/data",
            "a path prefix is kept"
        );
        assert!(matches!(
            gateway_stream_url("wss://gw.example.com/connect"),
            Err(UrlError::NotConnectPath(_))
        ));
        assert!(matches!(
            gateway_stream_url("wss://gw.example.com/environment-gateway/connect?x=1"),
            Err(UrlError::QueryOrFragment(_))
        ));
    }

    #[test]
    fn data_route_bases_are_validated_not_rewritten() {
        for good in [
            "wss://p.example/routes",
            "ws://127.0.0.1:9000",
            "wss://p.example",
        ] {
            assert_eq!(DataRouteBase::new(good).unwrap().as_str(), good);
        }
        for bad in [
            "wss://p.example/routes/",
            "wss://p.example/routes?x=1",
            "WSS://p.example",
            "https://p.example",
            "wss://u@p.example",
            "wss:///routes",
        ] {
            assert!(DataRouteBase::new(bad).is_err(), "{bad}");
        }
        assert!(serde_json::from_str::<DataRouteBase>("\"wss://p.example/\"").is_err());
    }

    #[test]
    fn a_wss_controller_cannot_use_a_ws_data_route_base() {
        let plain = DataRouteBase::new("ws://p.example/routes").unwrap();
        assert!(matches!(
            plain.check_for_controller("wss://p.example/control"),
            Err(UrlError::Downgrade(_))
        ));
        assert!(plain.check_for_controller("ws://127.0.0.1/control").is_ok());
        let secure = DataRouteBase::new("wss://p.example/routes").unwrap();
        assert!(
            secure
                .check_for_controller("wss://p.example/control")
                .is_ok()
        );
    }

    #[test]
    fn public_urls_allow_paths_and_queries() {
        assert!(PublicUrl::new("https://app.example/x?y=1").is_ok());
        for bad in ["ftp://x", "https://x/#f", "https://u@x", "wss://x"] {
            assert!(PublicUrl::new(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn data_routes_encode_opaque_target_ids() {
        let owner = TargetOwner {
            universe_id: "u1".parse().unwrap(),
            environment_id: "env_1".parse().unwrap(),
        };
        let target = ProviderTargetId::new("default/pod abc").unwrap();
        let base = DataRouteBase::new("wss://p.example/routes").unwrap();
        assert_eq!(
            provider_data_route(&base, &owner, &target),
            "wss://p.example/routes/u1/env_1/default%2Fpod%20abc"
        );
    }

    #[test]
    fn data_route_bases_derive_from_the_controller_origin() {
        for (controller, base) in [
            ("wss://p.example/control", "wss://p.example/routes"),
            (
                "wss://p.example:19090/x/y?z=1",
                "wss://p.example:19090/routes",
            ),
            ("ws://127.0.0.1:9000", "ws://127.0.0.1:9000/routes"),
            ("ws://[::1]:9000/control", "ws://[::1]:9000/routes"),
        ] {
            assert_eq!(
                DataRouteBase::derived_from_controller(controller)
                    .unwrap()
                    .as_str(),
                base,
                "{controller}"
            );
        }
        for bad in ["unix:///run/provider.sock", "https://p.example", "wss://"] {
            assert!(
                DataRouteBase::derived_from_controller(bad).is_err(),
                "{bad}"
            );
        }
        assert!(matches!(
            DataRouteBase::derived_from_controller("unix:///run/provider.sock"),
            Err(UrlError::Scheme(_))
        ));
    }

    #[test]
    fn refusals_are_state_tagged_with_fixed_statuses() {
        use serde_json::json;

        let cases = [
            (json!({"state": "waking"}), 503),
            (json!({"state": "booting"}), 503),
            (json!({"state": "provisioning"}), 503),
            (json!({"state": "disconnected"}), 503),
            (json!({"state": "away"}), 410),
            (json!({"state": "gone"}), 410),
            (json!({"state": "failed", "message": "disk lost"}), 409),
        ];
        for (body, status) in cases {
            let refusal: DataRouteRefusal = serde_json::from_value(body.clone()).unwrap();
            assert_eq!(refusal.status(), status, "{body}");
            assert_eq!(refusal.is_retryable(), status == 503, "{body}");
            assert_eq!(body["state"], refusal.state());
            assert_eq!(serde_json::to_value(&refusal).unwrap(), body);
        }
        for bad in [
            json!({"state": "waking", "retryAfterMs": 1}),
            json!({"state": "failed"}),
            json!({"state": "sleeping"}),
            json!({}),
        ] {
            assert!(
                serde_json::from_value::<DataRouteRefusal>(bad.clone()).is_err(),
                "{bad}"
            );
        }
        assert_eq!(
            DataRouteRefusal::Failed {
                message: DescriptiveText::lossy("disk lost").unwrap()
            }
            .to_string(),
            "failed: disk lost"
        );
    }

    #[test]
    fn refused_upgrades_decode_their_body_or_fall_back_on_their_status() {
        let refused = DataRouteRefusal::from_refused_upgrade;
        assert_eq!(
            refused(503, Some(br#"{"state":"waking"}"#), Some(2_000)),
            Some(DataRouteRefusal::Waking {})
        );
        assert_eq!(refused(410, None, None), Some(DataRouteRefusal::Gone {}));
        assert_eq!(
            refused(410, Some(br#"{"state":"aw"#), None),
            Some(DataRouteRefusal::Gone {})
        );
        assert!(matches!(
            refused(409, Some(b""), None),
            Some(DataRouteRefusal::Failed { .. })
        ));
        assert_eq!(
            refused(503, None, Some(4_000)),
            Some(DataRouteRefusal::Booting {})
        );
        assert_eq!(refused(503, None, None), None);
        assert_eq!(refused(502, None, Some(4_000)), None);
    }

    #[test]
    fn retry_after_is_whole_seconds_clamped_to_the_maximum() {
        assert_eq!(parse_retry_after_ms(" 3 "), Some(3_000));
        assert_eq!(
            parse_retry_after_ms("3600"),
            Some(RETRY_AFTER_MAX_SECS * 1_000)
        );
        assert_eq!(parse_retry_after_ms("Wed, 21 Oct 2026 07:28:00 GMT"), None);
    }
}
