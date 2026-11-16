//! GitHub App provider and installation API helpers.
//!
//! The private key in `credentials/providers/create` params is the third deliberate
//! inbound-plaintext path: it is validated (must parse as an RSA PEM) and
//! drafted into an encrypted secret record here, and never appears in views
//! or logs.

use super::*;

pub(super) fn parse_credential_provider_id(
    provider_id: String,
) -> Result<credentials::CredentialProviderId, AgentApiError> {
    credentials::CredentialProviderId::try_new(provider_id).map_err(|error| {
        AgentApiError::invalid_request(format!("invalid credential provider id: {error}"))
    })
}

pub(super) struct CredentialProviderCreateDraft {
    pub(super) secret: Option<credentials::PutSecretRecord>,
    pub(super) provider: credentials::CreateCredentialProviderRecord,
}

impl std::fmt::Debug for CredentialProviderCreateDraft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialProviderCreateDraft")
            .field("secret", &self.secret)
            .field("provider", &self.provider.provider_id)
            .finish()
    }
}

pub(super) fn credential_provider_create_draft(
    params: CredentialProviderCreateParams,
    now_ms: i64,
) -> Result<CredentialProviderCreateDraft, AgentApiError> {
    let provider_id = match params.provider_id {
        Some(provider_id) => parse_credential_provider_id(provider_id)?,
        None => credentials::CredentialProviderId::try_new(credentials::random_credential_id(
            "authprovider_",
        ))
        .map_err(|error| {
            AgentApiError::internal(format!("generate credential provider id: {error}"))
        })?,
    };

    let (config, secret) = match params.config {
        api::CredentialProviderConfigInput::GitHubApp {
            app_id,
            api_base_url,
        } => {
            let Some(credential) = params.credential else {
                return Err(AgentApiError::invalid_request(
                    "github_app providers require the private key PEM as credential",
                ));
            };
            let private_key = credentials::SecretValue::new(credential);
            // Fail at registration, not at mint time.
            credentials::validate_github_app_private_key(&private_key)
                .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
            let secret_id =
                credentials::SecretId::try_new(credentials::random_credential_id("authsec_"))
                    .map_err(|error| {
                        AgentApiError::internal(format!("generate secret id: {error}"))
                    })?;
            let secret = credentials::PutSecretRecord {
                secret_id: secret_id.clone(),
                secret_kind: credentials::SECRET_KIND_GITHUB_APP_PRIVATE_KEY.to_owned(),
                value: private_key,
                created_at_ms: now_ms,
            };
            let config =
                credentials::CredentialProviderConfig::GitHubApp(credentials::GitHubAppConfig {
                    app_id,
                    api_base_url: api_base_url
                        .unwrap_or_else(|| credentials::DEFAULT_GITHUB_API_BASE_URL.to_owned()),
                });
            (config, Some((secret_id, secret)))
        }
        api::CredentialProviderConfigInput::ModelApiKey { endpoint } => {
            let Some(credential) = params.credential else {
                return Err(AgentApiError::invalid_request(
                    "model_api_key providers require the API key as credential",
                ));
            };
            let api_key = credentials::SecretValue::new(credential);
            if api_key.expose().trim().is_empty() {
                return Err(AgentApiError::invalid_request(
                    "model_api_key credential must not be empty",
                ));
            }
            let secret_id =
                credentials::SecretId::try_new(credentials::random_credential_id("authsec_"))
                    .map_err(|error| {
                        AgentApiError::internal(format!("generate secret id: {error}"))
                    })?;
            let secret = credentials::PutSecretRecord {
                secret_id: secret_id.clone(),
                secret_kind: credentials::SECRET_KIND_MODEL_API_KEY.to_owned(),
                value: api_key,
                created_at_ms: now_ms,
            };
            let config = credentials::CredentialProviderConfig::ModelApiKey(
                credentials::ModelApiKeyConfig {
                    endpoint: endpoint.map(model_endpoint_config).transpose()?,
                },
            );
            (config, Some((secret_id, secret)))
        }
        api::CredentialProviderConfigInput::ModelOAuth {
            grant_id,
            audience,
            endpoint,
        } => {
            if params.credential.is_some() {
                return Err(AgentApiError::invalid_request(
                    "model_ocredential providers bind a grant and accept no credential",
                ));
            }
            let grant_id = credentials::CredentialGrantId::try_new(grant_id).map_err(|error| {
                AgentApiError::invalid_request(format!("invalid credential grant id: {error}"))
            })?;
            let config =
                credentials::CredentialProviderConfig::ModelOAuth(credentials::ModelOAuthConfig {
                    grant_id,
                    audience,
                    endpoint: endpoint.map(model_endpoint_config).transpose()?,
                });
            (config, None)
        }
        api::CredentialProviderConfigInput::ModelEndpoint { endpoint } => {
            if params.credential.is_some() {
                return Err(AgentApiError::invalid_request(
                    "model_endpoint providers accept no credential",
                ));
            }
            (
                credentials::CredentialProviderConfig::ModelEndpoint(
                    credentials::ModelEndpointOnlyConfig {
                        endpoint: model_endpoint_config(endpoint)?,
                    },
                ),
                None,
            )
        }
    };

    let provider = credentials::CreateCredentialProviderRecord {
        provider_id,
        display_name: params.display_name,
        config,
        credential_secret: secret.as_ref().map(|(secret_id, _)| secret_id.clone()),
        status: credentials::CredentialProviderStatus::Active,
        created_at_ms: now_ms,
    };
    provider
        .clone()
        .into_record()
        .validate()
        .map_err(map_credential_error)?;
    Ok(CredentialProviderCreateDraft {
        secret: secret.map(|(_, secret)| secret),
        provider,
    })
}

pub(super) fn credential_provider_view(
    record: credentials::CredentialProviderRecord,
) -> api::CredentialProviderView {
    api::CredentialProviderView {
        provider_id: record.provider_id.as_str().to_owned(),
        provider_kind: api_credential_provider_kind(record.provider_kind),
        display_name: record.display_name,
        config: match record.config {
            credentials::CredentialProviderConfig::GitHubApp(config) => {
                api::CredentialProviderConfigView::GitHubApp {
                    app_id: config.app_id,
                    api_base_url: config.api_base_url,
                }
            }
            credentials::CredentialProviderConfig::ModelApiKey(config) => {
                api::CredentialProviderConfigView::ModelApiKey {
                    endpoint: config.endpoint.map(model_endpoint_view),
                }
            }
            credentials::CredentialProviderConfig::ModelOAuth(config) => {
                api::CredentialProviderConfigView::ModelOAuth {
                    grant_id: config.grant_id.as_str().to_owned(),
                    audience: config.audience,
                    endpoint: config.endpoint.map(model_endpoint_view),
                }
            }
            credentials::CredentialProviderConfig::ModelEndpoint(config) => {
                api::CredentialProviderConfigView::ModelEndpoint {
                    endpoint: model_endpoint_view(config.endpoint),
                }
            }
        },
        has_credential: record.credential_secret.is_some(),
        status: match record.status {
            credentials::CredentialProviderStatus::Active => api::CredentialProviderStatus::Active,
            credentials::CredentialProviderStatus::NeedsConfiguration => {
                api::CredentialProviderStatus::NeedsConfiguration
            }
            credentials::CredentialProviderStatus::Disabled => {
                api::CredentialProviderStatus::Disabled
            }
        },
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

fn model_endpoint_config(
    endpoint: api::ModelEndpointConfig,
) -> Result<credentials::ModelEndpointConfig, AgentApiError> {
    Ok(credentials::ModelEndpointConfig {
        base_url: endpoint.base_url,
        headers: request_headers_from_api(
            &credentials::MODEL_ENDPOINT_HEADER_POLICY,
            endpoint.headers,
        )?,
        api_kinds: endpoint.api_kinds,
    })
}

fn model_endpoint_view(endpoint: credentials::ModelEndpointConfig) -> api::ModelEndpointConfig {
    api::ModelEndpointConfig {
        base_url: endpoint.base_url,
        headers: request_headers_view(endpoint.headers),
        api_kinds: endpoint.api_kinds,
    }
}

pub(super) fn github_installation_view(
    installation: &credentials::GitHubInstallation,
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
    provider: &credentials::CredentialProviderRecord,
    installation: &credentials::GitHubInstallation,
    grant_id: Option<String>,
    display_name: Option<String>,
    exposure: credentials::CredentialGrantExposure,
    now_ms: i64,
) -> Result<credentials::CreateCredentialGrantRecord, AgentApiError> {
    let grant_id = match grant_id {
        Some(grant_id) => parse_credential_grant_id(grant_id)?,
        None => {
            credentials::CredentialGrantId::try_new(credentials::random_credential_id("authgrant_"))
                .map_err(|error| {
                    AgentApiError::internal(format!("generate credential grant id: {error}"))
                })?
        }
    };
    let credentials::CredentialProviderConfig::GitHubApp(config) = &provider.config else {
        return Err(AgentApiError::rejected(format!(
            "credential provider {} is not a github_app provider",
            provider.provider_id
        )));
    };
    let metadata = credentials::GitHubInstallationGrantMetadata::from_installation(installation)
        .to_json()
        .map_err(map_credential_error)?;
    let create = credentials::CreateCredentialGrantRecord {
        grant_id,
        provider_id: provider.provider_id.as_str().to_owned(),
        provider_kind: credentials::CredentialProviderKind::GitHubApp,
        exposure,
        created_by: crate::gateway::request_context::request_attribution()?,
        display_name,
        subject_hint: installation.account_login.clone(),
        scopes: Vec::new(),
        audience: Some(config.api_base_url.clone()),
        access_token_secret: None,
        refresh_token_secret: None,
        oauth_client: None,
        expires_at_ms: None,
        status: credentials::CredentialGrantStatus::Active,
        metadata,
        created_at_ms: now_ms,
    };
    create
        .clone()
        .into_record()
        .validate()
        .map_err(map_credential_error)?;
    Ok(create)
}

pub(super) fn map_github_app_error(error: credentials::GitHubAppError) -> AgentApiError {
    match error {
        credentials::GitHubAppError::Registry(error) => map_credential_error(error),
        credentials::GitHubAppError::InvalidPrivateKey { .. } => {
            AgentApiError::invalid_request(error.to_string())
        }
        other => AgentApiError::rejected(other.to_string()),
    }
}
