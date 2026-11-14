//! Configured request headers.
//!
//! An MCP server and a model provider's endpoint each carry extra request
//! headers. Both hold the same thing: a header whose value is written out, or
//! one that names a credential whose token is sent. A written value is
//! configuration and is returned to every reader; a credential's token is
//! resolved at each request and never leaves the runtime.
//!
//! This module owns what the two share: the value type, the limits, the name
//! and value syntax, the refusal of a written value under a name that takes
//! only a credential (the names are part of the public contract and live in
//! `api`), the rule a credential must meet to be named by a header, and the
//! resolution of a credential into a header value. Each owner adds its own
//! reserved names as a [`RequestHeaderPolicy`] and keeps its own storage.

use std::collections::BTreeMap;

use reqwest::header::HeaderValue;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use api::is_credential_only_header;

use crate::{
    AuthBrokerError, AuthGrantId, AuthGrantRecord, AuthGrantStatus, AuthProviderKind,
    AuthTokenBroker, TokenAudience, audience_covers,
};

/// Configured request headers of one owner, keyed by lowercased header name.
pub type RequestHeaders = BTreeMap<String, RequestHeaderValue>;

/// Value of one configured request header. A literal is configuration and is
/// returned by reads; a credential names a universe auth grant whose token is
/// resolved at each request and sent raw.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RequestHeaderValue {
    Literal { value: String },
    Credential { grant_id: AuthGrantId },
}

impl RequestHeaderValue {
    pub fn grant_id(&self) -> Option<&AuthGrantId> {
        match self {
            Self::Literal { .. } => None,
            Self::Credential { grant_id } => Some(grant_id),
        }
    }
}

pub const REQUEST_HEADERS_MAX: usize = 32;
pub const REQUEST_HEADER_NAME_MAX_LEN: usize = 128;
pub const REQUEST_HEADER_VALUE_MAX_LEN: usize = 4096;

/// What one owner of configured headers adds to the shared rules: the names
/// its transport or its own authentication sets, which a configured header
/// must never override, and how messages name the owner.
#[derive(Clone, Copy, Debug)]
pub struct RequestHeaderPolicy {
    /// How messages name the owner, for example `"MCP server"`.
    pub owner: &'static str,
    /// How messages name the URL the headers are sent to, for example
    /// `"this MCP server's URL"`.
    pub target: &'static str,
    /// Lowercase names refused whatever the value source.
    pub reserved_names: &'static [&'static str],
    /// Lowercase prefixes refused whatever the value source.
    pub reserved_prefixes: &'static [&'static str],
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RequestHeaderError {
    #[error("at most {max} {owner} headers are allowed")]
    TooMany { owner: &'static str, max: usize },

    #[error("{owner} header name must be 1 to {max} characters: {name:?}")]
    NameLength {
        owner: &'static str,
        name: String,
        max: usize,
    },

    #[error("{owner} header name {name:?} is not a valid HTTP field name")]
    NameSyntax { owner: &'static str, name: String },

    #[error("{owner} header name {name:?} must be lowercase")]
    NameNotLowercase { owner: &'static str, name: String },

    #[error("{owner} header {name:?} is reserved and cannot be configured")]
    Reserved { owner: &'static str, name: String },

    #[error("duplicate {owner} header {name:?}; header names are case-insensitive")]
    Duplicate { owner: &'static str, name: String },

    #[error(
        "{owner} header {name:?} carries a secret: store the value as a static token credential \
         and reference it from the header"
    )]
    CredentialOnly { owner: &'static str, name: String },

    #[error("{owner} header {name:?} value must be 1 to {max} characters")]
    ValueLength {
        owner: &'static str,
        name: String,
        max: usize,
    },

    #[error("{owner} header {name:?} value must be printable ASCII without surrounding whitespace")]
    ValueSyntax { owner: &'static str, name: String },
}

/// Why a credential may not be named by a configured header.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HeaderCredentialError {
    #[error("credential {grant_id} for {owner} header {header:?} is not active: {status:?}")]
    NotActive {
        owner: &'static str,
        header: String,
        grant_id: AuthGrantId,
        status: AuthGrantStatus,
    },

    #[error(
        "credential {grant_id} for {owner} header {header:?} must be a static token, not \
         {provider_kind:?}"
    )]
    NotStaticToken {
        owner: &'static str,
        header: String,
        grant_id: AuthGrantId,
        provider_kind: AuthProviderKind,
    },

    #[error("credential {grant_id} for {owner} header {header:?} does not cover {target}")]
    AudienceMismatch {
        owner: &'static str,
        target: &'static str,
        header: String,
        grant_id: AuthGrantId,
    },
}

impl RequestHeaderPolicy {
    /// Canonical form of a configured header name: lowercased, then validated
    /// as an HTTP field name the owner does not reserve.
    pub fn normalize_name(&self, name: &str) -> Result<String, RequestHeaderError> {
        let normalized = name.to_ascii_lowercase();
        self.validate_name(&normalized)?;
        Ok(normalized)
    }

    /// Canonical form of a header map as a client wrote it. Two names that
    /// differ only by case are the same header and are refused rather than
    /// merged.
    pub fn normalize(
        &self,
        headers: impl IntoIterator<Item = (String, RequestHeaderValue)>,
    ) -> Result<RequestHeaders, RequestHeaderError> {
        let mut normalized = RequestHeaders::new();
        for (name, value) in headers {
            let name = self.normalize_name(&name)?;
            if normalized.contains_key(&name) {
                return Err(RequestHeaderError::Duplicate {
                    owner: self.owner,
                    name,
                });
            }
            normalized.insert(name, value);
        }
        Ok(normalized)
    }

    /// Names are compared, stored and looked up in one case, so a stored name
    /// is already lowercase.
    pub fn validate_name(&self, name: &str) -> Result<(), RequestHeaderError> {
        let owner = self.owner;
        if name.is_empty() || name.len() > REQUEST_HEADER_NAME_MAX_LEN {
            return Err(RequestHeaderError::NameLength {
                owner,
                name: name.to_owned(),
                max: REQUEST_HEADER_NAME_MAX_LEN,
            });
        }
        if !name.chars().all(is_field_name_char) {
            return Err(RequestHeaderError::NameSyntax {
                owner,
                name: name.to_owned(),
            });
        }
        if name.chars().any(|ch| ch.is_ascii_uppercase()) {
            return Err(RequestHeaderError::NameNotLowercase {
                owner,
                name: name.to_owned(),
            });
        }
        if self.reserved_names.contains(&name)
            || self
                .reserved_prefixes
                .iter()
                .any(|prefix| name.starts_with(prefix))
        {
            return Err(RequestHeaderError::Reserved {
                owner,
                name: name.to_owned(),
            });
        }
        Ok(())
    }

    /// A written header value: visible ASCII with spaces or tabs inside and no
    /// leading or trailing whitespace. A credential's token meets the same
    /// rule when it is resolved.
    pub fn validate_literal(&self, name: &str, value: &str) -> Result<(), RequestHeaderError> {
        if value.is_empty() || value.len() > REQUEST_HEADER_VALUE_MAX_LEN {
            return Err(RequestHeaderError::ValueLength {
                owner: self.owner,
                name: name.to_owned(),
                max: REQUEST_HEADER_VALUE_MAX_LEN,
            });
        }
        if !is_sendable_value(value) {
            return Err(RequestHeaderError::ValueSyntax {
                owner: self.owner,
                name: name.to_owned(),
            });
        }
        Ok(())
    }

    /// Every rule a stored header set meets: the count, each name, each
    /// written value, and a credential under every name that carries a secret.
    pub fn validate(&self, headers: &RequestHeaders) -> Result<(), RequestHeaderError> {
        if headers.len() > REQUEST_HEADERS_MAX {
            return Err(RequestHeaderError::TooMany {
                owner: self.owner,
                max: REQUEST_HEADERS_MAX,
            });
        }
        for (name, value) in headers {
            self.validate_name(name)?;
            match value {
                RequestHeaderValue::Literal { .. } if is_credential_only_header(name) => {
                    return Err(RequestHeaderError::CredentialOnly {
                        owner: self.owner,
                        name: name.clone(),
                    });
                }
                RequestHeaderValue::Literal { value } => self.validate_literal(name, value)?,
                RequestHeaderValue::Credential { .. } => {}
            }
        }
        Ok(())
    }

    /// The rule a credential meets before a header may name it: an active
    /// static token whose audience, when it has one, covers `target_url`.
    /// OAuth grants are refused: their access tokens would be sent raw to an
    /// arbitrary endpoint, and refresh stays out of header resolution. A
    /// grant's kind never changes, so only status and audience are checked
    /// again when the header is resolved.
    pub fn check_credential(
        &self,
        header: &str,
        target_url: &str,
        grant: &AuthGrantRecord,
    ) -> Result<(), HeaderCredentialError> {
        let owner = self.owner;
        if grant.status != AuthGrantStatus::Active {
            return Err(HeaderCredentialError::NotActive {
                owner,
                header: header.to_owned(),
                grant_id: grant.grant_id.clone(),
                status: grant.status,
            });
        }
        if grant.provider_kind != AuthProviderKind::StaticToken {
            return Err(HeaderCredentialError::NotStaticToken {
                owner,
                header: header.to_owned(),
                grant_id: grant.grant_id.clone(),
                provider_kind: grant.provider_kind,
            });
        }
        if let Some(audience) = &grant.audience
            && !audience_covers(audience, target_url)
        {
            return Err(HeaderCredentialError::AudienceMismatch {
                owner,
                target: self.target,
                header: header.to_owned(),
                grant_id: grant.grant_id.clone(),
            });
        }
        Ok(())
    }
}

/// RFC 9110 token characters.
fn is_field_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || matches!(
            ch,
            '!' | '#'
                | '$'
                | '%'
                | '&'
                | '\''
                | '*'
                | '+'
                | '-'
                | '.'
                | '^'
                | '_'
                | '`'
                | '|'
                | '~'
        )
}

fn is_sendable_value(value: &str) -> bool {
    value.trim() == value
        && value
            .bytes()
            .all(|byte| byte == b'\t' || (0x20..0x7f).contains(&byte))
}

/// Why a header credential produced no header value. No variant carries the
/// token, and none names the target URL: the text can reach a model.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CredentialHeaderError {
    #[error("credential {grant_id} no longer exists")]
    NotFound { grant_id: AuthGrantId },

    #[error("credential {grant_id} does not cover the URL the header is sent to")]
    AudienceMismatch { grant_id: AuthGrantId },

    #[error("credential {grant_id} cannot be used: {message}")]
    NotUsable {
        grant_id: AuthGrantId,
        message: String,
    },

    #[error("the credential's token is not a valid header value")]
    InvalidValue,

    #[error("credential store failure: {message}")]
    Store { message: String },
}

/// The value of a header that names credential `grant_id`, for a request to
/// `audience`. The broker checks the credential's status, expiry and audience
/// now, so a revoked or rebound credential fails the request instead of
/// sending a stale value. The value is marked sensitive.
pub async fn resolve_credential_header(
    broker: &dyn AuthTokenBroker,
    grant_id: &AuthGrantId,
    audience: &TokenAudience,
) -> Result<HeaderValue, CredentialHeaderError> {
    let token = broker
        .bearer_token(grant_id, audience)
        .await
        .map_err(|error| match error {
            AuthBrokerError::GrantNotFound { grant_id } => {
                CredentialHeaderError::NotFound { grant_id }
            }
            AuthBrokerError::AudienceMismatch { grant_id, .. } => {
                CredentialHeaderError::AudienceMismatch { grant_id }
            }
            AuthBrokerError::Store { message } => CredentialHeaderError::Store { message },
            other => CredentialHeaderError::NotUsable {
                grant_id: grant_id.clone(),
                message: other.to_string(),
            },
        })?;
    credential_header_value(token.expose())
}

/// A resolved credential token as a header value. It is marked sensitive, so
/// the HTTP stack keeps it out of its own debug output and compression
/// tables, and it is printable ASCII, so it can also be carried as text. The
/// error never echoes the token.
pub fn credential_header_value(token: &str) -> Result<HeaderValue, CredentialHeaderError> {
    if token.is_empty() || token.len() > REQUEST_HEADER_VALUE_MAX_LEN || !is_sendable_value(token) {
        return Err(CredentialHeaderError::InvalidValue);
    }
    let mut value =
        HeaderValue::from_str(token).map_err(|_| CredentialHeaderError::InvalidValue)?;
    value.set_sensitive(true);
    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use api::Attribution;

    use super::*;
    use crate::{
        AuthGrantExposure, AuthGrantStore, CreateAuthGrantRecord, InMemoryAuthGrantStore,
        InMemoryGrantLocks, InMemorySecretStore, PutSecretRecord, RegistryTokenBroker,
        SECRET_KIND_STATIC_TOKEN, SecretId, SecretStore, SecretValue,
    };

    const POLICY: RequestHeaderPolicy = RequestHeaderPolicy {
        owner: "test",
        target: "the test URL",
        reserved_names: &["authorization", "host"],
        reserved_prefixes: &["x-internal-"],
    };

    fn literal(value: &str) -> RequestHeaderValue {
        RequestHeaderValue::Literal {
            value: value.to_owned(),
        }
    }

    fn credential(grant_id: &str) -> RequestHeaderValue {
        RequestHeaderValue::Credential {
            grant_id: AuthGrantId::new(grant_id),
        }
    }

    #[test]
    fn names_normalize_to_lowercase_and_keep_http_field_syntax() {
        assert_eq!(POLICY.normalize_name("X-Api-Key"), Ok("x-api-key".into()));
        assert!(matches!(
            POLICY.normalize_name(""),
            Err(RequestHeaderError::NameLength { .. })
        ));
        assert!(matches!(
            POLICY.normalize_name(&"x".repeat(REQUEST_HEADER_NAME_MAX_LEN + 1)),
            Err(RequestHeaderError::NameLength { .. })
        ));
        for name in ["bad name", "x:colon", "x-tenant\n", "naïve"] {
            assert!(
                matches!(
                    POLICY.normalize_name(name),
                    Err(RequestHeaderError::NameSyntax { .. })
                ),
                "{name:?}"
            );
        }
        // A stored name is already lowercase.
        assert!(matches!(
            POLICY.validate_name("X-Title"),
            Err(RequestHeaderError::NameNotLowercase { .. })
        ));
    }

    #[test]
    fn each_owner_reserves_its_own_names_for_both_value_sources() {
        for name in ["Authorization", "host", "X-Internal-Trace"] {
            assert!(
                matches!(
                    POLICY.normalize_name(name),
                    Err(RequestHeaderError::Reserved { .. })
                ),
                "{name}"
            );
        }
        for value in [literal("v"), credential("authgrant_1")] {
            let headers = RequestHeaders::from([("authorization".to_owned(), value)]);
            assert!(matches!(
                POLICY.validate(&headers),
                Err(RequestHeaderError::Reserved { .. })
            ));
        }
        let open = RequestHeaderPolicy {
            reserved_names: &[],
            reserved_prefixes: &[],
            ..POLICY
        };
        assert_eq!(open.normalize_name("Host"), Ok("host".into()));
    }

    #[test]
    fn two_spellings_of_one_name_are_refused() {
        let normalized = POLICY
            .normalize([
                ("X-Tenant".to_owned(), literal("acme")),
                ("x-token".to_owned(), credential("authgrant_1")),
            ])
            .expect("distinct names");
        assert_eq!(
            normalized.keys().collect::<Vec<_>>(),
            ["x-tenant", "x-token"]
        );

        assert_eq!(
            POLICY.normalize([
                ("X-Tenant".to_owned(), literal("a")),
                ("x-tenant".to_owned(), literal("b")),
            ]),
            Err(RequestHeaderError::Duplicate {
                owner: "test",
                name: "x-tenant".to_owned(),
            })
        );
    }

    #[test]
    fn names_that_carry_a_secret_accept_only_a_credential() {
        for name in api::CREDENTIAL_ONLY_HEADER_NAMES {
            let plain = RequestHeaders::from([((*name).to_owned(), literal("sk-plain"))]);
            assert_eq!(
                POLICY.validate(&plain),
                Err(RequestHeaderError::CredentialOnly {
                    owner: "test",
                    name: (*name).to_owned(),
                })
            );
            let referenced =
                RequestHeaders::from([((*name).to_owned(), credential("authgrant_1"))]);
            assert_eq!(POLICY.validate(&referenced), Ok(()));
        }
        // Any other name takes either source.
        let other = RequestHeaders::from([
            ("x-tenant-token".to_owned(), credential("authgrant_1")),
            ("x-tenant".to_owned(), literal("acme")),
        ]);
        assert_eq!(POLICY.validate(&other), Ok(()));
    }

    #[test]
    fn written_values_are_bounded_printable_ascii() {
        for value in [
            "acme",
            "two words",
            "tab\tinside",
            "https://tracker.example",
        ] {
            assert_eq!(
                POLICY.validate_literal("x-tenant", value),
                Ok(()),
                "{value:?}"
            );
        }
        for value in ["", &"v".repeat(REQUEST_HEADER_VALUE_MAX_LEN + 1)] {
            assert!(matches!(
                POLICY.validate_literal("x-tenant", value),
                Err(RequestHeaderError::ValueLength { .. })
            ));
        }
        for value in [" leading", "trailing ", "line\nbreak", "caf\u{e9}", "nul\0"] {
            assert!(
                matches!(
                    POLICY.validate_literal("x-tenant", value),
                    Err(RequestHeaderError::ValueSyntax { .. })
                ),
                "{value:?}"
            );
        }
    }

    #[test]
    fn a_header_set_is_bounded() {
        let mut headers = RequestHeaders::new();
        for index in 0..REQUEST_HEADERS_MAX {
            headers.insert(format!("x-header-{index}"), literal("v"));
        }
        assert_eq!(POLICY.validate(&headers), Ok(()));
        headers.insert("x-one-more".to_owned(), literal("v"));
        assert_eq!(
            POLICY.validate(&headers),
            Err(RequestHeaderError::TooMany {
                owner: "test",
                max: REQUEST_HEADERS_MAX,
            })
        );
    }

    #[test]
    fn values_keep_their_tagged_storage_form() {
        assert_eq!(
            serde_json::to_value(literal("acme")).unwrap(),
            serde_json::json!({ "type": "literal", "value": "acme" })
        );
        let reference = credential("authgrant_1");
        let json = serde_json::to_value(&reference).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "type": "credential", "grant_id": "authgrant_1" })
        );
        assert_eq!(
            serde_json::from_value::<RequestHeaderValue>(json).unwrap(),
            reference
        );
        assert_eq!(reference.grant_id(), Some(&AuthGrantId::new("authgrant_1")));
        assert_eq!(literal("acme").grant_id(), None);
    }

    fn grant_draft(
        kind: AuthProviderKind,
        status: AuthGrantStatus,
        audience: Option<&str>,
    ) -> CreateAuthGrantRecord {
        CreateAuthGrantRecord {
            grant_id: AuthGrantId::new("authgrant_header"),
            provider_id: "static".to_owned(),
            provider_kind: kind,
            exposure: AuthGrantExposure::default(),
            created_by: Attribution::Local,
            display_name: None,
            subject_hint: None,
            scopes: Vec::new(),
            audience: audience.map(str::to_owned),
            access_token_secret: Some(SecretId::new("authsec_header")),
            refresh_token_secret: None,
            oauth_client: None,
            metadata: serde_json::json!({}),
            expires_at_ms: None,
            status,
            created_at_ms: 1,
        }
    }

    fn grant(
        kind: AuthProviderKind,
        status: AuthGrantStatus,
        audience: Option<&str>,
    ) -> AuthGrantRecord {
        grant_draft(kind, status, audience).into_record()
    }

    #[test]
    fn a_header_names_only_an_active_static_token_that_covers_the_target() {
        let target = "https://router.example/v1";
        let check = |grant: &AuthGrantRecord| POLICY.check_credential("x-api-key", target, grant);

        for audience in [None, Some("https://router.example"), Some(target)] {
            let grant = grant(
                AuthProviderKind::StaticToken,
                AuthGrantStatus::Active,
                audience,
            );
            assert_eq!(check(&grant), Ok(()), "{audience:?}");
        }

        assert!(matches!(
            check(&grant(
                AuthProviderKind::StaticToken,
                AuthGrantStatus::Revoked,
                None
            )),
            Err(HeaderCredentialError::NotActive {
                status: AuthGrantStatus::Revoked,
                ..
            })
        ));
        assert!(matches!(
            check(&grant(
                AuthProviderKind::McpOAuth,
                AuthGrantStatus::Active,
                None
            )),
            Err(HeaderCredentialError::NotStaticToken {
                provider_kind: AuthProviderKind::McpOAuth,
                ..
            })
        ));
        let elsewhere = check(&grant(
            AuthProviderKind::StaticToken,
            AuthGrantStatus::Active,
            Some("https://other.example"),
        ))
        .expect_err("another audience");
        assert!(matches!(
            elsewhere,
            HeaderCredentialError::AudienceMismatch { .. }
        ));
        // The refusal names the owner's wording for the target, not the URL.
        let message = elsewhere.to_string();
        assert!(message.contains("the test URL"), "{message}");
        assert!(!message.contains(target), "{message}");
    }

    #[test]
    fn a_resolved_token_becomes_a_sensitive_value_or_fails_without_being_echoed() {
        let value = credential_header_value("router-token").expect("valid token");
        assert!(value.is_sensitive());
        assert_eq!(value.to_str().expect("printable"), "router-token");

        for token in [
            "",
            "s3cr3t\nvalue",
            " s3cr3t",
            "s3cr3t\u{e9}",
            &"t".repeat(REQUEST_HEADER_VALUE_MAX_LEN + 1),
        ] {
            let error = credential_header_value(token).expect_err("unsendable token");
            assert_eq!(error, CredentialHeaderError::InvalidValue);
            assert!(!error.to_string().contains("s3cr3t"));
        }
    }

    async fn broker_with_token(
        token: &str,
        audience: Option<&str>,
    ) -> (RegistryTokenBroker, Arc<InMemoryAuthGrantStore>) {
        let secrets = Arc::new(InMemorySecretStore::new());
        let grants = Arc::new(InMemoryAuthGrantStore::new());
        secrets
            .put_secret(PutSecretRecord {
                secret_id: SecretId::new("authsec_header"),
                secret_kind: SECRET_KIND_STATIC_TOKEN.to_owned(),
                value: SecretValue::new(token),
                created_at_ms: 1,
            })
            .await
            .expect("store token");
        grants
            .create_grant(grant_draft(
                AuthProviderKind::StaticToken,
                AuthGrantStatus::Active,
                audience,
            ))
            .await
            .expect("create grant");
        let broker =
            RegistryTokenBroker::new(grants.clone(), secrets, Arc::new(InMemoryGrantLocks::new()));
        (broker, grants)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_credential_header_resolves_to_its_current_token() {
        let grant_id = AuthGrantId::new("authgrant_header");
        let target = TokenAudience::ModelProvider("https://router.example/v1".to_owned());

        for audience in [None, Some("https://router.example")] {
            let (broker, _grants) = broker_with_token("router-token", audience).await;
            let value = resolve_credential_header(&broker, &grant_id, &target)
                .await
                .expect("resolve");
            assert!(value.is_sensitive());
            assert_eq!(value.to_str().expect("printable"), "router-token");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_unusable_credential_fails_with_a_typed_error() {
        let grant_id = AuthGrantId::new("authgrant_header");
        let target = TokenAudience::McpResource("https://crm.example/mcp".to_owned());

        let (broker, grants) = broker_with_token("router-token", None).await;
        assert_eq!(
            resolve_credential_header(&broker, &AuthGrantId::new("authgrant_gone"), &target).await,
            Err(CredentialHeaderError::NotFound {
                grant_id: AuthGrantId::new("authgrant_gone"),
            })
        );
        grants
            .update_grant_status(&grant_id, AuthGrantStatus::Revoked, 2)
            .await
            .expect("revoke");
        assert!(matches!(
            resolve_credential_header(&broker, &grant_id, &target).await,
            Err(CredentialHeaderError::NotUsable { .. })
        ));

        let (broker, _grants) =
            broker_with_token("router-token", Some("https://other.example")).await;
        let mismatch = resolve_credential_header(&broker, &grant_id, &target)
            .await
            .expect_err("another audience");
        assert_eq!(
            mismatch,
            CredentialHeaderError::AudienceMismatch {
                grant_id: grant_id.clone(),
            }
        );
        // This text can reach a model: it names the credential, not the URL.
        assert!(!mismatch.to_string().contains("crm.example"));

        let (broker, _grants) = broker_with_token("s3cr3t\nvalue", None).await;
        let invalid = resolve_credential_header(&broker, &grant_id, &target)
            .await
            .expect_err("a token that is not a header value");
        assert_eq!(invalid, CredentialHeaderError::InvalidValue);
        assert!(!invalid.to_string().contains("s3cr3t"));
    }
}
