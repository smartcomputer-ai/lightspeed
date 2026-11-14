use super::*;
use crate::gateway::service::auth_api::map_auth_error;

use ::environments::{
    EnvironmentCredentialRecord, EnvironmentCredentialSource, EnvironmentCredentialSourceRefusal,
    EnvironmentCredentialStore, ListEnvironmentCredentials, PutEnvironmentCredential,
    require_bindable_grant, require_bindable_provider,
};
use auth::{AuthGrantId, AuthGrantStatus, AuthProviderId, AuthProviderStatus};

impl EnvironmentService {
    pub(crate) async fn bind_environment_credential_record(
        &self,
        params: EnvironmentCredentialBindParams,
    ) -> Result<EnvironmentCredentialBindResponse, AgentApiError> {
        let environment_id = parse_registry_environment_id(params.environment_id)?;
        validate_credential_env_name(&params.env_name)?;
        let source = self.credential_source_from_api(params.source).await?;
        let credential = EnvironmentCredentialStore::bind_credential(
            self.store.as_ref(),
            PutEnvironmentCredential {
                environment_id,
                env_name: params.env_name,
                source,
                created_at_ms: now_ms()?,
            },
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentCredentialBindResponse {
            credential: environment_credential_view(credential),
        })
    }

    pub(crate) async fn list_environment_credential_records(
        &self,
        params: EnvironmentCredentialListParams,
    ) -> Result<EnvironmentCredentialListResponse, AgentApiError> {
        let environment_id = parse_registry_environment_id(params.environment_id)?;
        let credentials = EnvironmentCredentialStore::list_credentials(
            self.store.as_ref(),
            ListEnvironmentCredentials { environment_id },
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentCredentialListResponse {
            credentials: credentials
                .into_iter()
                .map(environment_credential_view)
                .collect(),
        })
    }

    pub(crate) async fn unbind_environment_credential_record(
        &self,
        params: EnvironmentCredentialUnbindParams,
    ) -> Result<EnvironmentCredentialUnbindResponse, AgentApiError> {
        let environment_id = parse_registry_environment_id(params.environment_id)?;
        validate_credential_env_name(&params.env_name)?;
        let credential = EnvironmentCredentialStore::unbind_credential(
            self.store.as_ref(),
            &environment_id,
            &params.env_name,
        )
        .await
        .map_err(map_environments_error)?;
        Ok(EnvironmentCredentialUnbindResponse {
            credential: environment_credential_view(credential),
        })
    }

    /// Checks a requested source against the stored grant or provider. The
    /// credential resolver repeats the exposure and provider-kind rules each
    /// time the binding is resolved, so a binding made before a rule existed
    /// is refused there.
    pub(crate) async fn credential_source_from_api(
        &self,
        source: EnvironmentCredentialSourceView,
    ) -> Result<EnvironmentCredentialSource, AgentApiError> {
        match source {
            EnvironmentCredentialSourceView::AuthGrant { grant_id } => {
                let grant_id = AuthGrantId::try_new(grant_id).map_err(|error| {
                    AgentApiError::invalid_request(format!("invalid grant_id: {error}"))
                })?;
                let grant = auth::AuthGrantStore::read_grant(self.store.as_ref(), &grant_id)
                    .await
                    .map_err(map_auth_error)?;
                grant_credential_source(&grant)
            }
            EnvironmentCredentialSourceView::AuthProviderCredential { provider_id } => {
                let provider_id = AuthProviderId::try_new(provider_id).map_err(|error| {
                    AgentApiError::invalid_request(format!("invalid provider_id: {error}"))
                })?;
                let provider =
                    auth::AuthProviderStore::read_auth_provider(self.store.as_ref(), &provider_id)
                        .await
                        .map_err(map_auth_error)?;
                provider_credential_source(&provider)
            }
        }
    }
}

/// The binding source for a stored grant: it must be active and retrievable.
fn grant_credential_source(
    grant: &auth::AuthGrantRecord,
) -> Result<EnvironmentCredentialSource, AgentApiError> {
    if grant.status != AuthGrantStatus::Active {
        return Err(AgentApiError::rejected(format!(
            "auth grant is not active: {}",
            grant.grant_id
        )));
    }
    require_bindable_grant(&grant.grant_id, grant.exposure)
        .map_err(map_credential_source_refusal)?;
    Ok(EnvironmentCredentialSource::AuthGrant {
        grant_id: grant.grant_id.clone(),
    })
}

/// The binding source for a stored provider: it must be an active model API
/// key provider that holds a key.
fn provider_credential_source(
    provider: &auth::AuthProviderRecord,
) -> Result<EnvironmentCredentialSource, AgentApiError> {
    if provider.status != AuthProviderStatus::Active {
        return Err(AgentApiError::rejected(format!(
            "auth provider is not active: {}",
            provider.provider_id
        )));
    }
    require_bindable_provider(&provider.provider_id, provider.provider_kind)
        .map_err(map_credential_source_refusal)?;
    if provider.credential_secret.is_none() {
        return Err(AgentApiError::rejected(format!(
            "auth provider has no stored API key: {}",
            provider.provider_id
        )));
    }
    Ok(EnvironmentCredentialSource::AuthProviderCredential {
        provider_id: provider.provider_id.clone(),
    })
}

pub(crate) fn environment_credential_view(
    record: EnvironmentCredentialRecord,
) -> EnvironmentCredentialView {
    EnvironmentCredentialView {
        environment_id: record.environment_id.as_str().to_owned(),
        env_name: record.env_name,
        source: credential_source_view(record.source),
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

fn credential_source_view(source: EnvironmentCredentialSource) -> EnvironmentCredentialSourceView {
    match source {
        EnvironmentCredentialSource::AuthGrant { grant_id } => {
            EnvironmentCredentialSourceView::AuthGrant {
                grant_id: grant_id.as_str().to_owned(),
            }
        }
        EnvironmentCredentialSource::AuthProviderCredential { provider_id } => {
            EnvironmentCredentialSourceView::AuthProviderCredential {
                provider_id: provider_id.as_str().to_owned(),
            }
        }
    }
}

/// A source the binding rules refuse is a rejected request: the caller named
/// an existing credential that may not be bound, and the message says which
/// one to bind instead.
fn map_credential_source_refusal(refusal: EnvironmentCredentialSourceRefusal) -> AgentApiError {
    AgentApiError::rejected(refusal.to_string())
}

pub(crate) fn validate_credential_env_name(value: &str) -> Result<(), AgentApiError> {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(AgentApiError::invalid_request(
            "credential env_name must not be empty",
        ));
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return Err(AgentApiError::invalid_request(format!(
            "invalid credential env_name: {value}"
        )));
    }
    let len = 1 + chars
        .try_fold(0usize, |count, ch| {
            if ch == '_' || ch.is_ascii_alphanumeric() {
                Ok(count + 1)
            } else {
                Err(())
            }
        })
        .map_err(|()| {
            AgentApiError::invalid_request(format!("invalid credential env_name: {value}"))
        })?;
    if len > 128 {
        return Err(AgentApiError::invalid_request(format!(
            "credential env_name is too long: {len} bytes, max 128"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use api::AgentApiErrorKind;
    use auth::{
        AuthGrantExposure, AuthGrantRecord, AuthProviderConfig, AuthProviderKind,
        CreateAuthProviderRecord, GitHubAppConfig, ModelApiKeyConfig, SecretId,
    };

    use super::*;

    fn grant(exposure: AuthGrantExposure, status: AuthGrantStatus) -> AuthGrantRecord {
        AuthGrantRecord {
            grant_id: AuthGrantId::new("authgrant_repo"),
            provider_id: "static".to_owned(),
            provider_kind: AuthProviderKind::StaticToken,
            exposure,
            created_by: api::Attribution::Local,
            display_name: None,
            subject_hint: None,
            scopes: Vec::new(),
            audience: None,
            access_token_secret: Some(SecretId::new("authsec_repo")),
            refresh_token_secret: None,
            oauth_client: None,
            expires_at_ms: None,
            status,
            metadata: serde_json::Value::Object(Default::default()),
            last_leased_at_ms: None,
            lease_count: 0,
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    fn provider(
        provider_id: &str,
        config: AuthProviderConfig,
        credential_secret: Option<&str>,
    ) -> auth::AuthProviderRecord {
        CreateAuthProviderRecord {
            provider_id: AuthProviderId::new(provider_id),
            display_name: None,
            config,
            credential_secret: credential_secret.map(SecretId::new),
            status: AuthProviderStatus::Active,
            created_at_ms: 1,
        }
        .into_record()
    }

    #[test]
    fn a_retrievable_grant_can_be_bound() {
        assert_eq!(
            grant_credential_source(&grant(
                AuthGrantExposure::Retrievable,
                AuthGrantStatus::Active
            ))
            .expect("retrievable grant binds"),
            EnvironmentCredentialSource::AuthGrant {
                grant_id: AuthGrantId::new("authgrant_repo"),
            }
        );
    }

    #[test]
    fn a_brokered_grant_is_refused_at_bind_time() {
        let error =
            grant_credential_source(&grant(AuthGrantExposure::Brokered, AuthGrantStatus::Active))
                .expect_err("brokered grant must not bind");
        assert_eq!(error.kind, AgentApiErrorKind::Rejected);
        assert_eq!(
            error.message,
            EnvironmentCredentialSourceRefusal::BrokeredGrant {
                grant_id: AuthGrantId::new("authgrant_repo"),
            }
            .to_string()
        );
    }

    #[test]
    fn an_inactive_grant_is_refused_at_bind_time() {
        let error = grant_credential_source(&grant(
            AuthGrantExposure::Retrievable,
            AuthGrantStatus::Revoked,
        ))
        .expect_err("revoked grant must not bind");
        assert_eq!(error.kind, AgentApiErrorKind::Rejected);
    }

    #[test]
    fn a_model_api_key_provider_can_be_bound() {
        let record = provider(
            "model:openai",
            AuthProviderConfig::ModelApiKey(ModelApiKeyConfig::default()),
            Some("authsec_openai"),
        );
        assert_eq!(
            provider_credential_source(&record).expect("model API key binds"),
            EnvironmentCredentialSource::AuthProviderCredential {
                provider_id: AuthProviderId::new("model:openai"),
            }
        );
    }

    #[test]
    fn a_github_app_provider_is_refused_at_bind_time() {
        let record = provider(
            "github",
            AuthProviderConfig::GitHubApp(GitHubAppConfig {
                app_id: "12345".to_owned(),
                api_base_url: auth::DEFAULT_GITHUB_API_BASE_URL.to_owned(),
            }),
            Some("authsec_github_key"),
        );
        let error =
            provider_credential_source(&record).expect_err("an App private key must not bind");
        assert_eq!(error.kind, AgentApiErrorKind::Rejected);
        assert_eq!(
            error.message,
            EnvironmentCredentialSourceRefusal::GitHubAppProvider {
                provider_id: AuthProviderId::new("github"),
            }
            .to_string()
        );
    }

    #[test]
    fn a_model_provider_without_a_stored_key_is_refused_at_bind_time() {
        let record = provider(
            "model:openai",
            AuthProviderConfig::ModelApiKey(ModelApiKeyConfig::default()),
            None,
        );
        let error = provider_credential_source(&record).expect_err("no key to bind");
        assert_eq!(error.kind, AgentApiErrorKind::Rejected);
    }
}
