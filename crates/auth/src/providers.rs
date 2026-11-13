//! Generic auth provider configurations.
//!
//! One record shape serves every provider kind: non-secret, provider-specific
//! config is stored as JSON but decoded into the typed [`AuthProviderConfig`]
//! enum at the store boundary, so consumers never touch raw JSON. The
//! load-bearing credential reference (for GitHub Apps: the private key) is a
//! typed field; `store-pg` backs it with a foreign key into
//! `auth_secrets`. A model endpoint's extra request headers are the shared
//! configured request headers of [`crate::headers`]: a header holds a plain
//! value or names the credential whose token is sent, and `store-pg` keeps
//! them in a child table so a credential in use by a header cannot be deleted.

use async_trait::async_trait;
use std::collections::{BTreeMap, BTreeSet};

use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::{
    AuthGrantId, AuthProviderId, AuthProviderKind, AuthRegistryError, RequestHeaderError,
    RequestHeaderPolicy, RequestHeaders, SecretId, validate_audience_url,
    validate_nonempty_optional, validate_nonnegative_i64, validate_token_component,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthProviderStatus {
    #[default]
    Active,
    NeedsConfiguration,
    Disabled,
}

/// Typed, non-secret provider configuration. Stored as tagged JSON; new
/// providers add a variant here, not a table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AuthProviderConfig {
    #[serde(rename = "github_app")]
    GitHubApp(GitHubAppConfig),
    #[serde(rename = "model_api_key")]
    ModelApiKey(ModelApiKeyConfig),
    #[serde(rename = "model_oauth")]
    ModelOAuth(ModelOAuthConfig),
    #[serde(rename = "model_endpoint")]
    ModelEndpoint(ModelEndpointOnlyConfig),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GitHubAppConfig {
    /// GitHub's numeric app id (the JWT `iss` claim).
    pub app_id: String,
    /// REST API base URL; override for GitHub Enterprise Server.
    pub api_base_url: String,
}

/// Stored API key for a model provider. The key itself is the
/// provider row's credential secret; the config carries no secret material.
/// Rows use the `model:<provider_id>` provider-id convention, keyed off the
/// session's `ModelSelection.provider_id`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ModelApiKeyConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<ModelEndpointConfig>,
}

/// OAuth-grant-backed model provider credential. The referenced
/// grant's access token (refreshed by the broker as needed) authenticates
/// provider calls as an OAuth bearer token instead of an API key. The row
/// carries no credential secret of its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ModelOAuthConfig {
    /// Grant whose access token authenticates calls to this provider.
    pub grant_id: AuthGrantId,
    /// Audience URL requested from the broker, typically the provider API
    /// base URL. When omitted, only audience-unrestricted grants resolve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<ModelEndpointConfig>,
}

/// An OpenAI-compatible endpoint and its transport metadata. The provider's
/// own credential remains in the provider row's encrypted secret or OAuth
/// grant; a header that carries a secret names a credential instead of
/// holding the value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ModelEndpointConfig {
    pub base_url: String,
    /// Extra request headers sent on every request to the endpoint, keyed by
    /// lowercased header name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: RequestHeaders,
    /// Explicitly admitted Lightspeed API kinds. Empty lists are rejected.
    pub api_kinds: Vec<String>,
}

/// What a model endpoint adds to the shared header rules: the names the
/// transport or the provider's own credential set. Configured headers never
/// override them, whatever their value source.
pub const MODEL_ENDPOINT_HEADER_POLICY: RequestHeaderPolicy = RequestHeaderPolicy {
    owner: "model endpoint",
    target: "the endpoint's base URL",
    reserved_names: &[
        "authorization",
        "connection",
        "content-type",
        "cookie",
        "host",
        "set-cookie",
        "transfer-encoding",
    ],
    reserved_prefixes: &[],
};

impl From<RequestHeaderError> for AuthRegistryError {
    fn from(error: RequestHeaderError) -> Self {
        Self::InvalidInput {
            message: error.to_string(),
        }
    }
}

/// Credentialless OpenAI-compatible endpoint, primarily for loopback
/// Ollama/vLLM deployments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ModelEndpointOnlyConfig {
    pub endpoint: ModelEndpointConfig,
}

/// Provider-row id for a stored model provider credential:
/// `model:<provider_id>`.
pub fn model_auth_provider_id(provider_id: &str) -> String {
    format!("model:{provider_id}")
}

impl AuthProviderConfig {
    pub fn provider_kind(&self) -> AuthProviderKind {
        match self {
            Self::GitHubApp(_) => AuthProviderKind::GitHubApp,
            Self::ModelApiKey(_) => AuthProviderKind::ModelApiKey,
            Self::ModelOAuth(_) => AuthProviderKind::ModelOAuth,
            Self::ModelEndpoint(_) => AuthProviderKind::ModelEndpoint,
        }
    }

    pub fn to_json(&self) -> Result<serde_json::Value, AuthRegistryError> {
        serde_json::to_value(self).map_err(|error| AuthRegistryError::Store {
            message: format!("encode auth provider config: {error}"),
        })
    }

    pub fn from_json(value: &serde_json::Value) -> Result<Self, AuthRegistryError> {
        serde_json::from_value(value.clone()).map_err(|error| AuthRegistryError::Store {
            message: format!("decode auth provider config: {error}"),
        })
    }

    /// The model endpoint this configuration carries, if any.
    pub fn model_endpoint(&self) -> Option<&ModelEndpointConfig> {
        match self {
            Self::GitHubApp(_) => None,
            Self::ModelApiKey(config) => config.endpoint.as_ref(),
            Self::ModelOAuth(config) => config.endpoint.as_ref(),
            Self::ModelEndpoint(config) => Some(&config.endpoint),
        }
    }

    pub fn model_endpoint_mut(&mut self) -> Option<&mut ModelEndpointConfig> {
        match self {
            Self::GitHubApp(_) => None,
            Self::ModelApiKey(config) => config.endpoint.as_mut(),
            Self::ModelOAuth(config) => config.endpoint.as_mut(),
            Self::ModelEndpoint(config) => Some(&mut config.endpoint),
        }
    }

    pub fn validate(&self) -> Result<(), AuthRegistryError> {
        match self {
            Self::GitHubApp(config) => {
                validate_token_component("github app id", &config.app_id)?;
                if !config.app_id.chars().all(|ch| ch.is_ascii_digit()) {
                    return Err(AuthRegistryError::InvalidInput {
                        message: format!("github app id must be numeric, got {:?}", config.app_id),
                    });
                }
                validate_audience_url(&config.api_base_url).map_err(|error| match error {
                    AuthRegistryError::InvalidInput { message } => {
                        AuthRegistryError::InvalidInput {
                            message: format!("api base url: {message}"),
                        }
                    }
                    other => other,
                })
            }
            Self::ModelApiKey(config) => validate_optional_model_endpoint(config.endpoint.as_ref()),
            Self::ModelOAuth(config) => {
                if let Some(audience) = &config.audience {
                    validate_audience_url(audience).map_err(|error| match error {
                        AuthRegistryError::InvalidInput { message } => {
                            AuthRegistryError::InvalidInput {
                                message: format!("model oauth audience: {message}"),
                            }
                        }
                        other => other,
                    })?;
                }
                validate_optional_model_endpoint(config.endpoint.as_ref())
            }
            Self::ModelEndpoint(config) => validate_model_endpoint(&config.endpoint),
        }
    }
}

fn validate_optional_model_endpoint(
    endpoint: Option<&ModelEndpointConfig>,
) -> Result<(), AuthRegistryError> {
    endpoint.map_or(Ok(()), validate_model_endpoint)
}

fn validate_model_endpoint(endpoint: &ModelEndpointConfig) -> Result<(), AuthRegistryError> {
    validate_audience_url(&endpoint.base_url).map_err(|error| match error {
        AuthRegistryError::InvalidInput { message } => AuthRegistryError::InvalidInput {
            message: format!("model endpoint base URL: {message}"),
        },
        other => other,
    })?;
    let url = Url::parse(&endpoint.base_url).map_err(|error| AuthRegistryError::InvalidInput {
        message: format!("model endpoint base URL is invalid: {error}"),
    })?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AuthRegistryError::InvalidInput {
            message: "model endpoint base URL must not include credentials".to_owned(),
        });
    }
    if url.fragment().is_some() || url.query().is_some() {
        return Err(AuthRegistryError::InvalidInput {
            message: "model endpoint base URL must not include a query or fragment".to_owned(),
        });
    }
    match url.scheme() {
        "https" => {}
        "http" if is_loopback_host(url.host_str()) => {}
        "http" => {
            return Err(AuthRegistryError::InvalidInput {
                message: "model endpoint base URL must use HTTPS; HTTP is allowed only for loopback hosts"
                    .to_owned(),
            });
        }
        scheme => {
            return Err(AuthRegistryError::InvalidInput {
                message: format!("model endpoint URL scheme {scheme:?} is not supported"),
            });
        }
    }
    if endpoint.api_kinds.is_empty() {
        return Err(AuthRegistryError::InvalidInput {
            message: "model endpoint api_kinds must contain at least one API kind".to_owned(),
        });
    }
    let mut kinds = BTreeSet::new();
    for kind in &endpoint.api_kinds {
        if !matches!(
            kind.as_str(),
            "openai:responses" | "openai:completions" | "openai:audio-transcriptions"
        ) {
            return Err(AuthRegistryError::InvalidInput {
                message: format!("model endpoint API kind {kind:?} is not supported"),
            });
        }
        if !kinds.insert(kind) {
            return Err(AuthRegistryError::InvalidInput {
                message: format!("model endpoint API kind {kind:?} is duplicated"),
            });
        }
    }
    MODEL_ENDPOINT_HEADER_POLICY.validate(&endpoint.headers)?;
    Ok(())
}

fn is_loopback_host(host: Option<&str>) -> bool {
    let Some(host) = host else {
        return false;
    };
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthProviderRecord {
    pub provider_id: AuthProviderId,
    pub provider_kind: AuthProviderKind,
    pub display_name: Option<String>,
    pub config: AuthProviderConfig,
    /// The provider's long-lived credential (for GitHub Apps: the private
    /// key), referenced by id — never the value.
    pub credential_secret: Option<SecretId>,
    pub status: AuthProviderStatus,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl AuthProviderRecord {
    pub fn validate(&self) -> Result<(), AuthRegistryError> {
        if self.provider_kind != self.config.provider_kind() {
            return Err(AuthRegistryError::InvalidInput {
                message: format!(
                    "provider kind {:?} does not match config kind {:?}",
                    self.provider_kind,
                    self.config.provider_kind()
                ),
            });
        }
        validate_nonempty_optional("display_name", self.display_name.as_deref())?;
        self.config.validate()?;
        if matches!(
            self.config,
            AuthProviderConfig::ModelApiKey(_)
                | AuthProviderConfig::ModelOAuth(_)
                | AuthProviderConfig::ModelEndpoint(_)
        ) && self
            .provider_id
            .as_str()
            .strip_prefix("model:")
            .is_none_or(str::is_empty)
        {
            return Err(AuthRegistryError::InvalidInput {
                message: "model providers require a provider id of the form model:<provider_id>"
                    .to_owned(),
            });
        }
        if matches!(self.config, AuthProviderConfig::GitHubApp(_))
            && self.credential_secret.is_none()
        {
            return Err(AuthRegistryError::InvalidInput {
                message: "github_app providers require a private key credential".to_owned(),
            });
        }
        if matches!(self.config, AuthProviderConfig::ModelApiKey(_))
            && self.credential_secret.is_none()
        {
            return Err(AuthRegistryError::InvalidInput {
                message: "model_api_key providers require the API key credential".to_owned(),
            });
        }
        if matches!(self.config, AuthProviderConfig::ModelOAuth(_))
            && self.credential_secret.is_some()
        {
            return Err(AuthRegistryError::InvalidInput {
                message: "model_oauth providers bind a grant and carry no credential secret"
                    .to_owned(),
            });
        }
        if matches!(self.config, AuthProviderConfig::ModelEndpoint(_))
            && self.credential_secret.is_some()
        {
            return Err(AuthRegistryError::InvalidInput {
                message: "model_endpoint providers carry no credential secret".to_owned(),
            });
        }
        validate_nonnegative_i64(self.created_at_ms, "created_at_ms")?;
        validate_nonnegative_i64(self.updated_at_ms, "updated_at_ms")?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateAuthProviderRecord {
    pub provider_id: AuthProviderId,
    pub display_name: Option<String>,
    pub config: AuthProviderConfig,
    pub credential_secret: Option<SecretId>,
    pub status: AuthProviderStatus,
    pub created_at_ms: i64,
}

impl CreateAuthProviderRecord {
    pub fn into_record(self) -> AuthProviderRecord {
        AuthProviderRecord {
            provider_id: self.provider_id,
            provider_kind: self.config.provider_kind(),
            display_name: self.display_name,
            config: self.config,
            credential_secret: self.credential_secret,
            status: self.status,
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.created_at_ms,
        }
    }
}

#[async_trait]
pub trait AuthProviderStore: Send + Sync {
    async fn create_auth_provider(
        &self,
        record: CreateAuthProviderRecord,
    ) -> Result<AuthProviderRecord, AuthRegistryError>;

    async fn read_auth_provider(
        &self,
        provider_id: &AuthProviderId,
    ) -> Result<AuthProviderRecord, AuthRegistryError>;

    async fn list_auth_providers(&self) -> Result<Vec<AuthProviderRecord>, AuthRegistryError>;

    async fn delete_auth_provider(
        &self,
        provider_id: &AuthProviderId,
    ) -> Result<AuthProviderRecord, AuthRegistryError>;

    /// Rewrite an existing provider in place (kind, display name, config,
    /// credential secret, status, `updated_at_ms`; the id and `created_at_ms`
    /// stay), only if it still references `expected_credential`. `Ok(None)`
    /// means another writer changed it first; the caller still owns any secret
    /// it stored for the replacement.
    async fn replace_auth_provider(
        &self,
        record: AuthProviderRecord,
        expected_credential: Option<&SecretId>,
    ) -> Result<Option<AuthProviderRecord>, AuthRegistryError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DEFAULT_GITHUB_API_BASE_URL, REQUEST_HEADER_VALUE_MAX_LEN, REQUEST_HEADERS_MAX,
        RequestHeaderValue,
    };

    fn github_config() -> AuthProviderConfig {
        AuthProviderConfig::GitHubApp(GitHubAppConfig {
            app_id: "12345".to_owned(),
            api_base_url: DEFAULT_GITHUB_API_BASE_URL.to_owned(),
        })
    }

    fn create_request() -> CreateAuthProviderRecord {
        CreateAuthProviderRecord {
            provider_id: AuthProviderId::new("lightspeed-github"),
            display_name: Some("Lightspeed GitHub App".to_owned()),
            config: github_config(),
            credential_secret: Some(SecretId::new("authsec_key")),
            status: AuthProviderStatus::Active,
            created_at_ms: 10,
        }
    }

    #[test]
    fn provider_records_validate_and_derive_kind() {
        let record = create_request().into_record();

        record.validate().expect("valid provider record");
        assert_eq!(record.provider_kind, AuthProviderKind::GitHubApp);
    }

    #[test]
    fn github_providers_require_a_credential() {
        let mut request = create_request();
        request.credential_secret = None;

        assert!(matches!(
            request.into_record().validate(),
            Err(AuthRegistryError::InvalidInput { .. })
        ));
    }

    #[test]
    fn github_app_ids_must_be_numeric() {
        let config = AuthProviderConfig::GitHubApp(GitHubAppConfig {
            app_id: "Iv23abc".to_owned(),
            api_base_url: DEFAULT_GITHUB_API_BASE_URL.to_owned(),
        });

        assert!(matches!(
            config.validate(),
            Err(AuthRegistryError::InvalidInput { .. })
        ));
    }

    #[test]
    fn provider_configs_round_trip_through_tagged_json() {
        let config = github_config();

        let json = config.to_json().expect("encode config");
        assert_eq!(json["type"], "github_app");
        assert_eq!(json["app_id"], "12345");

        let decoded = AuthProviderConfig::from_json(&json).expect("decode config");
        assert_eq!(decoded, config);
    }

    #[test]
    fn model_api_key_records_validate_and_derive_kind() {
        let record = CreateAuthProviderRecord {
            provider_id: AuthProviderId::new(model_auth_provider_id("openai")),
            display_name: None,
            config: AuthProviderConfig::ModelApiKey(ModelApiKeyConfig::default()),
            credential_secret: Some(SecretId::new("authsec_key")),
            status: AuthProviderStatus::Active,
            created_at_ms: 10,
        }
        .into_record();

        record.validate().expect("valid llm api key record");
        assert_eq!(record.provider_kind, AuthProviderKind::ModelApiKey);
        assert_eq!(record.provider_id.as_str(), "model:openai");
    }

    #[test]
    fn model_api_key_providers_require_a_credential() {
        let record = CreateAuthProviderRecord {
            provider_id: AuthProviderId::new("model:openai"),
            display_name: None,
            config: AuthProviderConfig::ModelApiKey(ModelApiKeyConfig::default()),
            credential_secret: None,
            status: AuthProviderStatus::Active,
            created_at_ms: 10,
        }
        .into_record();

        assert!(matches!(
            record.validate(),
            Err(AuthRegistryError::InvalidInput { .. })
        ));
    }

    #[test]
    fn model_api_key_configs_round_trip_through_tagged_json() {
        let config = AuthProviderConfig::ModelApiKey(ModelApiKeyConfig::default());

        let json = config.to_json().expect("encode config");
        assert_eq!(json["type"], "model_api_key");

        let decoded = AuthProviderConfig::from_json(&json).expect("decode config");
        assert_eq!(decoded, config);
    }

    fn model_oauth_config(audience: Option<&str>) -> AuthProviderConfig {
        AuthProviderConfig::ModelOAuth(ModelOAuthConfig {
            grant_id: AuthGrantId::new("authgrant_1"),
            audience: audience.map(str::to_owned),
            endpoint: None,
        })
    }

    #[test]
    fn model_oauth_records_validate_and_derive_kind() {
        let record = CreateAuthProviderRecord {
            provider_id: AuthProviderId::new(model_auth_provider_id("anthropic")),
            display_name: None,
            config: model_oauth_config(Some("https://api.anthropic.com")),
            credential_secret: None,
            status: AuthProviderStatus::Active,
            created_at_ms: 10,
        }
        .into_record();

        record.validate().expect("valid model oauth record");
        assert_eq!(record.provider_kind, AuthProviderKind::ModelOAuth);
    }

    #[test]
    fn model_oauth_providers_reject_credential_secrets_and_bad_audiences() {
        let with_credential = CreateAuthProviderRecord {
            provider_id: AuthProviderId::new("model:anthropic"),
            display_name: None,
            config: model_oauth_config(None),
            credential_secret: Some(SecretId::new("authsec_key")),
            status: AuthProviderStatus::Active,
            created_at_ms: 10,
        }
        .into_record();
        assert!(matches!(
            with_credential.validate(),
            Err(AuthRegistryError::InvalidInput { .. })
        ));

        assert!(matches!(
            model_oauth_config(Some("not a url")).validate(),
            Err(AuthRegistryError::InvalidInput { .. })
        ));
    }

    #[test]
    fn model_oauth_configs_round_trip_through_tagged_json() {
        let config = model_oauth_config(Some("https://api.anthropic.com"));

        let json = config.to_json().expect("encode config");
        assert_eq!(json["type"], "model_oauth");
        assert_eq!(json["grant_id"], "authgrant_1");

        let decoded = AuthProviderConfig::from_json(&json).expect("decode config");
        assert_eq!(decoded, config);
    }

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

    fn endpoint(base_url: &str) -> ModelEndpointConfig {
        ModelEndpointConfig {
            base_url: base_url.to_owned(),
            headers: BTreeMap::from([("x-title".to_owned(), literal("Lightspeed"))]),
            api_kinds: vec!["openai:completions".to_owned()],
        }
    }

    #[test]
    fn model_endpoints_require_tls_except_for_loopback_and_reserve_transport_headers() {
        validate_model_endpoint(&endpoint("https://router.example/v1")).expect("HTTPS endpoint");
        validate_model_endpoint(&endpoint("http://127.0.0.1:11434/v1")).expect("loopback endpoint");
        validate_model_endpoint(&endpoint("http://[::1]:11434/v1"))
            .expect("IPv6 loopback endpoint");

        for invalid in [
            "http://router.example/v1",
            "https://user:secret@router.example/v1",
            "https://router.example/v1?api-version=1",
        ] {
            assert!(
                validate_model_endpoint(&endpoint(invalid)).is_err(),
                "{invalid}"
            );
        }

        // Transport-owned names are refused whatever the value source.
        for value in [literal("secret"), credential("authgrant_1")] {
            let mut reserved = endpoint("https://router.example/v1");
            reserved.headers.insert("authorization".to_owned(), value);
            assert!(matches!(
                validate_model_endpoint(&reserved),
                Err(AuthRegistryError::InvalidInput { .. })
            ));
        }
    }

    #[test]
    fn secret_bearing_header_names_accept_only_a_credential() {
        for name in ["api-key", "x-api-key", "proxy-authorization"] {
            let mut plain = endpoint("https://router.example/v1");
            plain.headers.insert(name.to_owned(), literal("sk-plain"));
            assert!(
                matches!(
                    validate_model_endpoint(&plain),
                    Err(AuthRegistryError::InvalidInput { .. })
                ),
                "{name} must not accept a literal"
            );

            let mut referenced = endpoint("https://router.example/v1");
            referenced
                .headers
                .insert(name.to_owned(), credential("authgrant_1"));
            validate_model_endpoint(&referenced)
                .unwrap_or_else(|error| panic!("{name} accepts a credential: {error}"));
        }

        // Any other name takes either source.
        let mut other = endpoint("https://router.example/v1");
        other
            .headers
            .insert("x-tenant-token".to_owned(), credential("authgrant_1"));
        validate_model_endpoint(&other).expect("credential under an ordinary name");
    }

    #[test]
    fn header_names_are_lowercase_and_normalize() {
        assert_eq!(
            MODEL_ENDPOINT_HEADER_POLICY
                .normalize_name("X-Api-Key")
                .expect("valid"),
            "x-api-key"
        );
        // The endpoint's own reserved names, in any spelling.
        for reserved in [
            "Authorization",
            "Content-Type",
            "Host",
            "Cookie",
            "Set-Cookie",
        ] {
            assert!(
                matches!(
                    MODEL_ENDPOINT_HEADER_POLICY.normalize_name(reserved),
                    Err(RequestHeaderError::Reserved { .. })
                ),
                "{reserved}"
            );
        }
        // MCP protocol headers mean nothing to a model endpoint.
        assert!(
            MODEL_ENDPOINT_HEADER_POLICY
                .normalize_name("Mcp-Session-Id")
                .is_ok()
        );

        let mut mixed_case = endpoint("https://router.example/v1");
        mixed_case
            .headers
            .insert("X-Title".to_owned(), literal("Lightspeed"));
        assert!(matches!(
            validate_model_endpoint(&mixed_case),
            Err(AuthRegistryError::InvalidInput { .. })
        ));
    }

    #[test]
    fn headers_reject_invalid_literal_values_and_excess_entries() {
        let mut invalid = endpoint("https://router.example/v1");
        invalid
            .headers
            .insert("x-tenant".to_owned(), literal("line\nbreak"));
        assert!(validate_model_endpoint(&invalid).is_err());

        let mut long = endpoint("https://router.example/v1");
        long.headers.insert(
            "x-tenant".to_owned(),
            literal(&"v".repeat(REQUEST_HEADER_VALUE_MAX_LEN + 1)),
        );
        assert!(validate_model_endpoint(&long).is_err());

        let mut crowded = endpoint("https://router.example/v1");
        crowded.headers.clear();
        for index in 0..=REQUEST_HEADERS_MAX {
            crowded
                .headers
                .insert(format!("x-header-{index}"), literal("v"));
        }
        assert!(validate_model_endpoint(&crowded).is_err());
    }

    #[test]
    fn header_values_round_trip_through_tagged_json() {
        let mut config = endpoint("https://router.example/v1");
        config
            .headers
            .insert("x-api-key".to_owned(), credential("authgrant_1"));

        let json = serde_json::to_value(&config).expect("encode endpoint");
        assert_eq!(
            json["headers"]["x-title"],
            serde_json::json!({ "type": "literal", "value": "Lightspeed" })
        );
        assert_eq!(
            json["headers"]["x-api-key"],
            serde_json::json!({ "type": "credential", "grant_id": "authgrant_1" })
        );
        assert_eq!(
            serde_json::from_value::<ModelEndpointConfig>(json).expect("decode endpoint"),
            config
        );
    }

    #[test]
    fn model_endpoint_accessors_reach_every_model_kind() {
        let endpoint = endpoint("https://router.example/v1");
        let mut configs = [
            AuthProviderConfig::ModelApiKey(ModelApiKeyConfig {
                endpoint: Some(endpoint.clone()),
            }),
            AuthProviderConfig::ModelOAuth(ModelOAuthConfig {
                grant_id: AuthGrantId::new("authgrant_1"),
                audience: None,
                endpoint: Some(endpoint.clone()),
            }),
            AuthProviderConfig::ModelEndpoint(ModelEndpointOnlyConfig {
                endpoint: endpoint.clone(),
            }),
        ];
        for config in &mut configs {
            assert_eq!(config.model_endpoint(), Some(&endpoint));
            config
                .model_endpoint_mut()
                .expect("endpoint")
                .headers
                .clear();
            assert!(
                config
                    .model_endpoint()
                    .expect("endpoint")
                    .headers
                    .is_empty()
            );
        }
        assert_eq!(github_config().model_endpoint(), None);
        assert_eq!(
            AuthProviderConfig::ModelApiKey(ModelApiKeyConfig::default()).model_endpoint(),
            None
        );
    }

    #[test]
    fn speech_endpoints_declare_the_audio_protocol() {
        let mut speech = endpoint("http://localhost:9000/v1");
        speech.api_kinds = vec!["openai:audio-transcriptions".into()];
        validate_model_endpoint(&speech).expect("speech endpoint");
    }

    #[test]
    fn credentialless_model_endpoint_validates_and_round_trips() {
        let config = AuthProviderConfig::ModelEndpoint(ModelEndpointOnlyConfig {
            endpoint: endpoint("http://localhost:11434/v1"),
        });
        let record = CreateAuthProviderRecord {
            provider_id: AuthProviderId::new("model:ollama"),
            display_name: Some("Local Ollama".to_owned()),
            config: config.clone(),
            credential_secret: None,
            status: AuthProviderStatus::Active,
            created_at_ms: 10,
        }
        .into_record();
        record.validate().expect("credentialless endpoint");
        assert_eq!(
            AuthProviderConfig::from_json(&config.to_json().expect("encode")).expect("decode"),
            config
        );
    }
}
