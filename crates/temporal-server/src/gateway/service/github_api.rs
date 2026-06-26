//! GitHub App provider and installation API helpers (P69 G5).
//!
//! The private key in `auth/providers/create` params is the third deliberate
//! inbound-plaintext path: it is validated (must parse as an RSA PEM) and
//! drafted into an encrypted secret record here, and never appears in views
//! or logs.

use super::*;

pub(super) fn parse_auth_provider_id(
    provider_id: String,
) -> Result<auth::AuthProviderId, AgentApiError> {
    auth::AuthProviderId::try_new(provider_id).map_err(|error| {
        AgentApiError::invalid_request(format!("invalid auth provider id: {error}"))
    })
}

pub(super) struct AuthProviderCreateDraft {
    pub(super) secret: Option<auth::PutSecretRecord>,
    pub(super) provider: auth::CreateAuthProviderRecord,
}

impl std::fmt::Debug for AuthProviderCreateDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthProviderCreateDraft")
            .field("secret", &self.secret)
            .field("provider", &self.provider.provider_id)
            .finish()
    }
}

pub(super) fn auth_provider_create_draft(
    params: AuthProviderCreateParams,
    now_ms: i64,
) -> Result<AuthProviderCreateDraft, AgentApiError> {
    let provider_id = match params.provider_id {
        Some(provider_id) => parse_auth_provider_id(provider_id)?,
        None => auth::AuthProviderId::try_new(auth::random_auth_id("authprovider_")).map_err(
            |error| AgentApiError::internal(format!("generate auth provider id: {error}")),
        )?,
    };

    let (config, secret) = match params.config {
        api::AuthProviderConfigInput::GitHubApp {
            app_id,
            api_base_url,
        } => {
            let Some(credential) = params.credential else {
                return Err(AgentApiError::invalid_request(
                    "github_app providers require the private key PEM as credential",
                ));
            };
            let private_key = auth::SecretValue::new(credential);
            // Fail at registration, not at mint time.
            auth::validate_github_app_private_key(&private_key)
                .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
            let secret_id = auth::SecretId::try_new(auth::random_auth_id("authsec_"))
                .map_err(|error| AgentApiError::internal(format!("generate secret id: {error}")))?;
            let secret = auth::PutSecretRecord {
                secret_id: secret_id.clone(),
                secret_kind: auth::SECRET_KIND_GITHUB_APP_PRIVATE_KEY.to_owned(),
                value: private_key,
                created_at_ms: now_ms,
            };
            let config = auth::AuthProviderConfig::GitHubApp(auth::GitHubAppConfig {
                app_id,
                api_base_url: api_base_url
                    .unwrap_or_else(|| auth::DEFAULT_GITHUB_API_BASE_URL.to_owned()),
            });
            (config, Some((secret_id, secret)))
        }
        api::AuthProviderConfigInput::ModelApiKey {} => {
            let Some(credential) = params.credential else {
                return Err(AgentApiError::invalid_request(
                    "model_api_key providers require the API key as credential",
                ));
            };
            let api_key = auth::SecretValue::new(credential);
            if api_key.expose().trim().is_empty() {
                return Err(AgentApiError::invalid_request(
                    "model_api_key credential must not be empty",
                ));
            }
            let secret_id = auth::SecretId::try_new(auth::random_auth_id("authsec_"))
                .map_err(|error| AgentApiError::internal(format!("generate secret id: {error}")))?;
            let secret = auth::PutSecretRecord {
                secret_id: secret_id.clone(),
                secret_kind: auth::SECRET_KIND_MODEL_API_KEY.to_owned(),
                value: api_key,
                created_at_ms: now_ms,
            };
            let config = auth::AuthProviderConfig::ModelApiKey(auth::ModelApiKeyConfig::default());
            (config, Some((secret_id, secret)))
        }
        api::AuthProviderConfigInput::ModelOAuth { grant_id, audience } => {
            if params.credential.is_some() {
                return Err(AgentApiError::invalid_request(
                    "model_oauth providers bind a grant and accept no credential",
                ));
            }
            let grant_id = auth::AuthGrantId::try_new(grant_id).map_err(|error| {
                AgentApiError::invalid_request(format!("invalid auth grant id: {error}"))
            })?;
            let config =
                auth::AuthProviderConfig::ModelOAuth(auth::ModelOAuthConfig { grant_id, audience });
            (config, None)
        }
    };

    let provider = auth::CreateAuthProviderRecord {
        provider_id,
        display_name: params.display_name,
        config,
        credential_secret: secret.as_ref().map(|(secret_id, _)| secret_id.clone()),
        status: auth::AuthProviderStatus::Active,
        created_at_ms: now_ms,
    };
    provider
        .clone()
        .into_record()
        .validate()
        .map_err(map_auth_error)?;
    Ok(AuthProviderCreateDraft {
        secret: secret.map(|(_, secret)| secret),
        provider,
    })
}

pub(super) fn auth_provider_view(record: auth::AuthProviderRecord) -> api::AuthProviderView {
    api::AuthProviderView {
        provider_id: record.provider_id.as_str().to_owned(),
        provider_kind: api_auth_provider_kind(record.provider_kind),
        display_name: record.display_name,
        config: match record.config {
            auth::AuthProviderConfig::GitHubApp(config) => api::AuthProviderConfigView::GitHubApp {
                app_id: config.app_id,
                api_base_url: config.api_base_url,
            },
            auth::AuthProviderConfig::ModelApiKey(_) => api::AuthProviderConfigView::ModelApiKey {},
            auth::AuthProviderConfig::ModelOAuth(config) => {
                api::AuthProviderConfigView::ModelOAuth {
                    grant_id: config.grant_id.as_str().to_owned(),
                    audience: config.audience,
                }
            }
        },
        has_credential: record.credential_secret.is_some(),
        status: match record.status {
            auth::AuthProviderStatus::Active => api::AuthProviderStatus::Active,
            auth::AuthProviderStatus::NeedsConfiguration => {
                api::AuthProviderStatus::NeedsConfiguration
            }
            auth::AuthProviderStatus::Disabled => api::AuthProviderStatus::Disabled,
        },
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

pub(super) fn github_installation_view(
    installation: &auth::GitHubInstallation,
) -> api::GitHubInstallationView {
    api::GitHubInstallationView {
        installation_id: installation.installation_id,
        account_login: installation.account_login.clone(),
        repository_selection: installation.repository_selection.clone(),
        permissions: installation.permissions.clone(),
    }
}

/// Draft an installation grant from a verified installation. The grant
/// represents the installation: no stored tokens, audience bound to the
/// API base URL, metadata carrying the installation facts.
pub(super) fn github_installation_grant_draft(
    provider: &auth::AuthProviderRecord,
    installation: &auth::GitHubInstallation,
    grant_id: Option<String>,
    display_name: Option<String>,
    now_ms: i64,
) -> Result<auth::CreateAuthGrantRecord, AgentApiError> {
    let grant_id = match grant_id {
        Some(grant_id) => parse_auth_grant_id(grant_id)?,
        None => auth::AuthGrantId::try_new(auth::random_auth_id("authgrant_"))
            .map_err(|error| AgentApiError::internal(format!("generate auth grant id: {error}")))?,
    };
    let auth::AuthProviderConfig::GitHubApp(config) = &provider.config else {
        return Err(AgentApiError::rejected(format!(
            "auth provider {} is not a github_app provider",
            provider.provider_id
        )));
    };
    let metadata = auth::GitHubInstallationGrantMetadata::from_installation(installation)
        .to_json()
        .map_err(map_auth_error)?;
    let create = auth::CreateAuthGrantRecord {
        grant_id,
        provider_id: provider.provider_id.as_str().to_owned(),
        provider_kind: auth::AuthProviderKind::GitHubApp,
        principal: auth::PrincipalRef::universe_default(),
        display_name,
        subject_hint: installation.account_login.clone(),
        scopes: Vec::new(),
        audience: Some(config.api_base_url.clone()),
        access_token_secret: None,
        refresh_token_secret: None,
        oauth_client: None,
        expires_at_ms: None,
        status: auth::AuthGrantStatus::Active,
        metadata,
        created_at_ms: now_ms,
    };
    create
        .clone()
        .into_record()
        .validate()
        .map_err(map_auth_error)?;
    Ok(create)
}

pub(super) fn map_github_app_error(error: auth::GitHubAppError) -> AgentApiError {
    match error {
        auth::GitHubAppError::Registry(error) => map_auth_error(error),
        auth::GitHubAppError::InvalidPrivateKey { .. } => {
            AgentApiError::invalid_request(error.to_string())
        }
        other => AgentApiError::rejected(other.to_string()),
    }
}
