use super::*;

use auth_registry::{AuthGrantId, AuthGrantStatus, AuthProviderId, AuthProviderStatus, SecretId};
use environment_registry::{
    CreateSessionEnvironmentCredential, ListSessionEnvironmentCredentials,
    SessionEnvironmentBindingStatus, SessionEnvironmentCredentialRecord,
    SessionEnvironmentCredentialSource, SessionEnvironmentCredentialStore,
};

impl GatewayAgentApi {
    pub(super) async fn bind_session_environment_credential_record(
        &self,
        params: SessionEnvironmentCredentialBindParams,
    ) -> Result<SessionEnvironmentCredentialBindResponse, AgentApiError> {
        let session_id = parse_core_session_id(params.session_id)?;
        let env_id = parse_registry_environment_id(params.env_id)?;
        validate_credential_env_name(&params.env_name)?;
        let binding = environment_registry::SessionEnvironmentBindingStore::read_binding(
            self.store.as_ref(),
            &session_id,
            &env_id,
        )
        .await
        .map_err(map_environment_registry_error)?;
        if binding.status == SessionEnvironmentBindingStatus::Detached {
            return Err(AgentApiError::rejected(format!(
                "environment is detached: {}",
                env_id.as_str()
            )));
        }
        let source = self.credential_source_from_api(params.source).await?;
        let credential = SessionEnvironmentCredentialStore::bind_credential(
            self.store.as_ref(),
            CreateSessionEnvironmentCredential {
                session_id,
                env_id,
                env_name: params.env_name,
                source,
                created_at_ms: now_ms()?,
            },
        )
        .await
        .map_err(map_environment_registry_error)?;
        Ok(SessionEnvironmentCredentialBindResponse {
            credential: session_environment_credential_view(credential),
        })
    }

    pub(super) async fn list_session_environment_credential_records(
        &self,
        params: SessionEnvironmentCredentialListParams,
    ) -> Result<SessionEnvironmentCredentialListResponse, AgentApiError> {
        let session_id = parse_core_session_id(params.session_id)?;
        let env_id = parse_registry_environment_id(params.env_id)?;
        let credentials = SessionEnvironmentCredentialStore::list_credentials(
            self.store.as_ref(),
            ListSessionEnvironmentCredentials { session_id, env_id },
        )
        .await
        .map_err(map_environment_registry_error)?;
        Ok(SessionEnvironmentCredentialListResponse {
            credentials: credentials
                .into_iter()
                .map(session_environment_credential_view)
                .collect(),
        })
    }

    pub(super) async fn unbind_session_environment_credential_record(
        &self,
        params: SessionEnvironmentCredentialUnbindParams,
    ) -> Result<SessionEnvironmentCredentialUnbindResponse, AgentApiError> {
        let session_id = parse_core_session_id(params.session_id)?;
        let env_id = parse_registry_environment_id(params.env_id)?;
        validate_credential_env_name(&params.env_name)?;
        let credential = SessionEnvironmentCredentialStore::unbind_credential(
            self.store.as_ref(),
            &session_id,
            &env_id,
            &params.env_name,
        )
        .await
        .map_err(map_environment_registry_error)?;
        Ok(SessionEnvironmentCredentialUnbindResponse {
            credential: session_environment_credential_view(credential),
        })
    }

    async fn credential_source_from_api(
        &self,
        source: SessionEnvironmentCredentialSourceView,
    ) -> Result<SessionEnvironmentCredentialSource, AgentApiError> {
        match source {
            SessionEnvironmentCredentialSourceView::AuthGrant { grant_id } => {
                let grant_id = AuthGrantId::try_new(grant_id).map_err(|error| {
                    AgentApiError::invalid_request(format!("invalid grant_id: {error}"))
                })?;
                let grant =
                    auth_registry::AuthGrantStore::read_grant(self.store.as_ref(), &grant_id)
                        .await
                        .map_err(map_auth_registry_error)?;
                if grant.status != AuthGrantStatus::Active {
                    return Err(AgentApiError::rejected(format!(
                        "auth grant is not active: {grant_id}"
                    )));
                }
                Ok(SessionEnvironmentCredentialSource::AuthGrant { grant_id })
            }
            SessionEnvironmentCredentialSourceView::AuthProviderCredential { provider_id } => {
                let provider_id = AuthProviderId::try_new(provider_id).map_err(|error| {
                    AgentApiError::invalid_request(format!("invalid provider_id: {error}"))
                })?;
                let provider = auth_registry::AuthProviderStore::read_auth_provider(
                    self.store.as_ref(),
                    &provider_id,
                )
                .await
                .map_err(map_auth_registry_error)?;
                if provider.status != AuthProviderStatus::Active {
                    return Err(AgentApiError::rejected(format!(
                        "auth provider is not active: {provider_id}"
                    )));
                }
                if provider.credential_secret.is_none() {
                    return Err(AgentApiError::rejected(format!(
                        "auth provider has no exportable credential secret: {provider_id}"
                    )));
                }
                Ok(SessionEnvironmentCredentialSource::AuthProviderCredential { provider_id })
            }
            SessionEnvironmentCredentialSourceView::DirectSecret { secret_id } => {
                let secret_id = SecretId::try_new(secret_id).map_err(|error| {
                    AgentApiError::invalid_request(format!("invalid secret_id: {error}"))
                })?;
                let _ = auth_registry::SecretStore::read_secret(self.store.as_ref(), &secret_id)
                    .await
                    .map_err(map_auth_registry_error)?;
                Ok(SessionEnvironmentCredentialSource::DirectSecret { secret_id })
            }
        }
    }
}

pub(super) fn session_environment_credential_view(
    record: SessionEnvironmentCredentialRecord,
) -> SessionEnvironmentCredentialView {
    SessionEnvironmentCredentialView {
        session_id: record.session_id.as_str().to_owned(),
        env_id: record.env_id.as_str().to_owned(),
        env_name: record.env_name,
        source: credential_source_view(record.source),
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

fn credential_source_view(
    source: SessionEnvironmentCredentialSource,
) -> SessionEnvironmentCredentialSourceView {
    match source {
        SessionEnvironmentCredentialSource::AuthGrant { grant_id } => {
            SessionEnvironmentCredentialSourceView::AuthGrant {
                grant_id: grant_id.as_str().to_owned(),
            }
        }
        SessionEnvironmentCredentialSource::AuthProviderCredential { provider_id } => {
            SessionEnvironmentCredentialSourceView::AuthProviderCredential {
                provider_id: provider_id.as_str().to_owned(),
            }
        }
        SessionEnvironmentCredentialSource::DirectSecret { secret_id } => {
            SessionEnvironmentCredentialSourceView::DirectSecret {
                secret_id: secret_id.as_str().to_owned(),
            }
        }
    }
}

pub(super) fn validate_credential_env_name(value: &str) -> Result<(), AgentApiError> {
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
