//! Generic auth provider configurations.
//!
//! One record shape serves every provider kind: non-secret, provider-specific
//! config is stored as JSON but decoded into the typed [`AuthProviderConfig`]
//! enum at the store boundary, so consumers never touch raw JSON. The
//! load-bearing credential reference (for GitHub Apps: the private key) is a
//! typed field; `store-pg` backs it with a foreign key into
//! `auth_secrets`.

use async_trait::async_trait;
use std::collections::{BTreeMap, BTreeSet};

use reqwest::{
    Url,
    header::{HeaderName, HeaderValue},
};
use serde::{Deserialize, Serialize};

use crate::{
    AuthGrantId, AuthProviderId, AuthProviderKind, AuthRegistryError, SecretId,
    validate_audience_url, validate_nonempty_optional, validate_nonnegative_i64,
    validate_token_component,
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

/// An OpenAI-compatible endpoint and its non-secret transport metadata.
/// Credentials remain in the provider row's encrypted secret or OAuth grant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ModelEndpointConfig {
    pub base_url: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Explicitly admitted Lightspeed API kinds. Empty lists are rejected.
    pub api_kinds: Vec<String>,
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
    if endpoint.headers.len() > 32 {
        return Err(AuthRegistryError::InvalidInput {
            message: "model endpoint headers must contain at most 32 entries".to_owned(),
        });
    }
    for (name, value) in &endpoint.headers {
        if name.len() > 128 || value.len() > 4096 {
            return Err(AuthRegistryError::InvalidInput {
                message: format!(
                    "model endpoint header {name:?} exceeds the supported name or value length"
                ),
            });
        }
        let parsed_name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
            AuthRegistryError::InvalidInput {
                message: format!("model endpoint header name {name:?} is invalid"),
            }
        })?;
        if is_reserved_model_header(&parsed_name) {
            return Err(AuthRegistryError::InvalidInput {
                message: format!(
                    "model endpoint header {name:?} is transport-owned and cannot be overridden"
                ),
            });
        }
        HeaderValue::from_str(value).map_err(|_| AuthRegistryError::InvalidInput {
            message: format!("model endpoint header {name:?} has an invalid value"),
        })?;
    }
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

fn is_reserved_model_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "authorization"
            | "content-type"
            | "host"
            | "cookie"
            | "set-cookie"
            | "connection"
            | "transfer-encoding"
    )
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DEFAULT_GITHUB_API_BASE_URL;

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

    fn endpoint(base_url: &str) -> ModelEndpointConfig {
        ModelEndpointConfig {
            base_url: base_url.to_owned(),
            headers: BTreeMap::from([("x-title".to_owned(), "Lightspeed".to_owned())]),
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

        let mut reserved = endpoint("https://router.example/v1");
        reserved
            .headers
            .insert("Authorization".to_owned(), "secret".to_owned());
        assert!(validate_model_endpoint(&reserved).is_err());
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
