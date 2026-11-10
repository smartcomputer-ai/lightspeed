//! Custom request headers: a credential that is a set of HTTP header
//! name/value pairs rather than one bearer token.
//!
//! Every value is treated as secret. The whole set is stored as one encrypted
//! secret ([`SECRET_KIND_STATIC_HEADERS`]) and only ever crosses boundaries as a
//! [`SecretValue`] or as [`StaticHeaders`], whose `Debug` output names the
//! headers but never their values.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{AuthRegistryError, SecretValue};

pub const SECRET_KIND_STATIC_HEADERS: &str = "auth.static_headers";

/// Most headers one credential may carry.
pub const MAX_STATIC_HEADERS: usize = 16;
const MAX_HEADER_NAME_BYTES: usize = 128;
const MAX_HEADER_VALUE_BYTES: usize = 8 * 1024;

/// Headers the transport owns: framing, content negotiation, the MCP session,
/// and hop-by-hop connection control. `Authorization` is deliberately allowed
/// so servers expecting a scheme other than `Bearer` can be configured.
const RESERVED_HEADER_NAMES: [&str; 11] = [
    "accept",
    "connection",
    "content-length",
    "content-type",
    "host",
    "keep-alive",
    "last-event-id",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];
/// The MCP protocol's own headers (`Mcp-Session-Id`, `MCP-Protocol-Version`,
/// `Mcp-Method`, ...) and Lightspeed's trusted-header namespace.
const RESERVED_HEADER_PREFIXES: [&str; 2] = ["mcp-", "x-lightspeed-"];

/// A validated, non-empty set of custom request headers. Names are unique
/// case-insensitively and keep the spelling they were entered with.
#[derive(Clone, PartialEq, Eq)]
pub struct StaticHeaders(Vec<(String, SecretValue)>);

#[derive(Serialize, Deserialize)]
struct EncodedHeader<'a> {
    name: std::borrow::Cow<'a, str>,
    value: std::borrow::Cow<'a, str>,
}

impl StaticHeaders {
    pub fn new(
        headers: impl IntoIterator<Item = (String, SecretValue)>,
    ) -> Result<Self, AuthRegistryError> {
        let headers: Vec<_> = headers.into_iter().collect();
        if headers.is_empty() {
            return Err(invalid("custom headers must contain at least one header"));
        }
        if headers.len() > MAX_STATIC_HEADERS {
            return Err(invalid(format!(
                "custom headers may contain at most {MAX_STATIC_HEADERS} headers"
            )));
        }
        let mut seen = std::collections::BTreeSet::new();
        for (name, value) in &headers {
            validate_header_name(name)?;
            if !seen.insert(name.to_ascii_lowercase()) {
                return Err(invalid(format!("custom header {name} is listed twice")));
            }
            validate_header_value(name, value.expose())?;
        }
        Ok(Self(headers))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &SecretValue)> {
        self.0.iter().map(|(name, value)| (name.as_str(), value))
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(name, _)| name.as_str())
    }

    /// The stored form: a JSON array of `{name, value}` objects.
    pub fn to_secret_value(&self) -> SecretValue {
        let encoded: Vec<_> = self
            .0
            .iter()
            .map(|(name, value)| EncodedHeader {
                name: name.as_str().into(),
                value: value.expose().into(),
            })
            .collect();
        SecretValue::new(serde_json::to_string(&encoded).expect("headers serialize to JSON"))
    }

    /// Decodes and revalidates the stored form. Errors never echo the value.
    pub fn from_secret_value(value: &SecretValue) -> Result<Self, AuthRegistryError> {
        let encoded: Vec<EncodedHeader<'_>> = serde_json::from_str(value.expose())
            .map_err(|_| invalid("stored custom headers are not valid"))?;
        Self::new(encoded.into_iter().map(|header| {
            (
                header.name.into_owned(),
                SecretValue::new(header.value.into_owned()),
            )
        }))
    }
}

impl fmt::Debug for StaticHeaders {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.0.iter().map(|(name, _)| (name, "<redacted>")))
            .finish()
    }
}

fn validate_header_name(name: &str) -> Result<(), AuthRegistryError> {
    if name.is_empty() {
        return Err(invalid("custom header names must not be empty"));
    }
    if name.len() > MAX_HEADER_NAME_BYTES {
        return Err(invalid(format!(
            "custom header names must be at most {MAX_HEADER_NAME_BYTES} bytes"
        )));
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+.^_`|~-".contains(&byte))
    {
        return Err(invalid(format!(
            "custom header {name:?} is not a valid HTTP header name"
        )));
    }
    let lower = name.to_ascii_lowercase();
    if RESERVED_HEADER_NAMES.contains(&lower.as_str())
        || RESERVED_HEADER_PREFIXES
            .iter()
            .any(|prefix| lower.starts_with(prefix))
    {
        return Err(invalid(format!(
            "custom header {name} is reserved and cannot be set"
        )));
    }
    Ok(())
}

/// Messages name the header, never its value.
fn validate_header_value(name: &str, value: &str) -> Result<(), AuthRegistryError> {
    if value.is_empty() {
        return Err(invalid(format!("custom header {name} needs a value")));
    }
    if value.len() > MAX_HEADER_VALUE_BYTES {
        return Err(invalid(format!(
            "custom header {name} value must be at most {MAX_HEADER_VALUE_BYTES} bytes"
        )));
    }
    if value.trim_matches([' ', '\t']) != value {
        return Err(invalid(format!(
            "custom header {name} value must not start or end with whitespace"
        )));
    }
    if !value
        .bytes()
        .all(|byte| byte == b'\t' || (b' '..=b'~').contains(&byte))
    {
        return Err(invalid(format!(
            "custom header {name} value must be printable ASCII without line breaks"
        )));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> AuthRegistryError {
    AuthRegistryError::InvalidInput {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> Result<StaticHeaders, AuthRegistryError> {
        StaticHeaders::new(
            pairs
                .iter()
                .map(|(name, value)| ((*name).to_owned(), SecretValue::new(*value))),
        )
    }

    #[test]
    fn round_trips_through_the_stored_secret() {
        let original = headers(&[
            ("X-Upstream-Url", "https://tracker.example.com"),
            ("X-Upstream-Token", "secret-token"),
        ])
        .expect("valid headers");
        let decoded =
            StaticHeaders::from_secret_value(&original.to_secret_value()).expect("decodes");
        assert_eq!(decoded, original);
        assert_eq!(
            decoded.names().collect::<Vec<_>>(),
            vec!["X-Upstream-Url", "X-Upstream-Token"]
        );
    }

    #[test]
    fn debug_output_names_headers_without_values() {
        let value = headers(&[("X-Upstream-Token", "secret-token")]).expect("valid headers");
        let debug = format!("{value:?}");
        assert!(debug.contains("X-Upstream-Token"));
        assert!(!debug.contains("secret-token"));
    }

    #[test]
    fn authorization_with_a_custom_scheme_is_allowed() {
        headers(&[("Authorization", "Token abc123")]).expect("authorization is allowed");
    }

    #[test]
    fn rejects_reserved_headers() {
        for name in [
            "Host",
            "content-type",
            "Content-Length",
            "Mcp-Session-Id",
            "MCP-Protocol-Version",
            "Mcp-Method",
            "Accept",
            "X-Lightspeed-Universe",
            "x-lightspeed-anything",
        ] {
            assert!(
                matches!(
                    headers(&[(name, "value")]),
                    Err(AuthRegistryError::InvalidInput { .. })
                ),
                "{name} must be rejected"
            );
        }
    }

    #[test]
    fn rejects_invalid_names_values_and_shapes() {
        let invalid_sets: [&[(&str, &str)]; 8] = [
            &[],
            &[("", "value")],
            &[("X Bad", "value")],
            &[("X-Token", "")],
            &[("X-Token", "line\r\nX-Injected: 1")],
            &[("X-Token", " padded")],
            &[("X-Token", "non-ascii-é")],
            &[("X-Token", "a"), ("x-token", "b")],
        ];
        for set in invalid_sets {
            assert!(
                matches!(headers(set), Err(AuthRegistryError::InvalidInput { .. })),
                "{set:?} must be rejected"
            );
        }
        let too_many: Vec<_> = (0..=MAX_STATIC_HEADERS)
            .map(|index| (format!("X-H{index}"), SecretValue::new("v")))
            .collect();
        assert!(StaticHeaders::new(too_many).is_err());
    }

    #[test]
    fn errors_never_echo_values() {
        let error = headers(&[("X-Token", "secret\nvalue")]).expect_err("rejected");
        assert!(!error.to_string().contains("secret"));
        let error = StaticHeaders::from_secret_value(&SecretValue::new("secret-not-json"))
            .expect_err("rejected");
        assert!(!error.to_string().contains("secret-not-json"));
    }
}
