//! Generic auth provider configurations (P69 G5).
//!
//! One record shape serves every provider kind: non-secret, provider-specific
//! config is stored as JSON but decoded into the typed [`AuthProviderConfig`]
//! enum at the store boundary, so consumers never touch raw JSON. The
//! load-bearing credential reference (for GitHub Apps: the private key) is a
//! typed field; `store-pg` backs it with a foreign key into
//! `auth_secrets`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    AuthProviderId, AuthProviderKind, AuthRegistryError, SecretId, validate_audience_url,
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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GitHubAppConfig {
    /// GitHub's numeric app id (the JWT `iss` claim).
    pub app_id: String,
    /// REST API base URL; override for GitHub Enterprise Server.
    pub api_base_url: String,
}

/// Stored API key for an LLM provider (P69 G6). The key itself is the provider
/// row's credential secret; the config carries no secret material. Rows use
/// the `model:<provider_id>` provider-id convention, keyed off the session's
/// `ModelSelection.provider_id`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ModelApiKeyConfig {}

/// Provider-row id for a stored LLM provider key: `model:<provider_id>`.
pub fn model_auth_provider_id(provider_id: &str) -> String {
    format!("model:{provider_id}")
}

impl AuthProviderConfig {
    pub fn provider_kind(&self) -> AuthProviderKind {
        match self {
            Self::GitHubApp(_) => AuthProviderKind::GitHubApp,
            Self::ModelApiKey(_) => AuthProviderKind::ModelApiKey,
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
                        message: format!(
                            "github app id must be numeric, got {:?}",
                            config.app_id
                        ),
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
            Self::ModelApiKey(ModelApiKeyConfig {}) => Ok(()),
        }
    }
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
            provider_id: AuthProviderId::new("forge-github"),
            display_name: Some("Forge GitHub App".to_owned()),
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
}
