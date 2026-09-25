use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use auth::{
    AuthGrantRecord, AuthGrantStore, AuthProviderKind, AuthProviderStatus, AuthProviderStore,
    AuthTokenBroker, DEFAULT_GITHUB_API_BASE_URL, GitHubAppRuntime, GrantRefreshLock,
    HttpGitHubApiClient, HttpOAuthTokenClient, OAuthClientStore, OAuthRefreshRuntime,
    RegistryTokenBroker, SecretStore, TokenAudience,
};
use environment_protocol::{
    data::jobs::{StartJobsParams, StartJobsResponse},
    shared::SecretString,
};
use environments::{
    EnvironmentCredentialSource, EnvironmentCredentialStore, EnvironmentId,
    ListEnvironmentCredentials,
};
use store_pg::PgStore;
use thiserror::Error;
use tools::environment::{
    EnvironmentToolContext,
    jobs::{JobError, JobExecResult, JobExecutor},
    process::{
        ContinueProcessRequest, ProcessError, ProcessExecResult, ProcessExecutor, ProcessOutput,
        ProcessRequest,
    },
};

#[derive(Clone)]
pub(crate) struct EnvironmentCredentialResolver {
    credentials: Arc<dyn EnvironmentCredentialStore>,
    grants: Arc<dyn AuthGrantStore>,
    providers: Arc<dyn AuthProviderStore>,
    secrets: Arc<dyn SecretStore>,
    broker: Option<Arc<dyn AuthTokenBroker>>,
}

impl EnvironmentCredentialResolver {
    pub(crate) fn new(
        credentials: Arc<dyn EnvironmentCredentialStore>,
        grants: Arc<dyn AuthGrantStore>,
        providers: Arc<dyn AuthProviderStore>,
        secrets: Arc<dyn SecretStore>,
        broker: Option<Arc<dyn AuthTokenBroker>>,
    ) -> Self {
        Self {
            credentials,
            grants,
            providers,
            secrets,
            broker,
        }
    }

    pub(crate) fn from_pg_store(store: Arc<PgStore>) -> Self {
        let credentials: Arc<dyn EnvironmentCredentialStore> = store.clone();
        let grants: Arc<dyn AuthGrantStore> = store.clone();
        let providers: Arc<dyn AuthProviderStore> = store.clone();
        let secrets: Arc<dyn SecretStore> = store.clone();
        let broker = registry_token_broker(store);
        Self::new(credentials, grants, providers, secrets, broker)
    }

    pub(crate) async fn resolve_secret_env(
        &self,
        environment_id: &EnvironmentId,
        explicit_env: &BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, SecretString>, EnvironmentCredentialResolutionError> {
        let bindings = self
            .credentials
            .list_credentials(ListEnvironmentCredentials {
                environment_id: environment_id.clone(),
            })
            .await
            .map_err(|error| EnvironmentCredentialResolutionError::Store {
                message: error.to_string(),
            })?;

        let mut secret_env = BTreeMap::new();
        let mut resolved_sources: BTreeMap<EnvironmentCredentialSource, SecretString> =
            BTreeMap::new();
        for binding in bindings {
            if explicit_env.contains_key(&binding.env_name) {
                return Err(EnvironmentCredentialResolutionError::EnvCollision {
                    env_name: binding.env_name,
                });
            }
            let value = if let Some(value) = resolved_sources.get(&binding.source) {
                value.clone()
            } else {
                let value = self
                    .resolve_source(&binding.env_name, &binding.source, environment_id)
                    .await?;
                resolved_sources.insert(binding.source.clone(), value.clone());
                value
            };
            secret_env.insert(binding.env_name, value);
        }
        Ok(secret_env)
    }

    async fn resolve_source(
        &self,
        env_name: &str,
        source: &EnvironmentCredentialSource,
        environment_id: &EnvironmentId,
    ) -> Result<SecretString, EnvironmentCredentialResolutionError> {
        match source {
            EnvironmentCredentialSource::AuthGrant { grant_id } => {
                let grant = self.grants.read_grant(grant_id).await.map_err(|error| {
                    EnvironmentCredentialResolutionError::Source {
                        env_name: env_name.to_owned(),
                        message: error.to_string(),
                    }
                })?;
                let audience = token_audience_for_grant(&grant, environment_id);
                let Some(broker) = &self.broker else {
                    return Err(EnvironmentCredentialResolutionError::Source {
                        env_name: env_name.to_owned(),
                        message: "auth token broker is not configured".to_owned(),
                    });
                };
                let value = broker
                    .bearer_token(grant_id, &audience)
                    .await
                    .map_err(|error| EnvironmentCredentialResolutionError::Source {
                        env_name: env_name.to_owned(),
                        message: error.to_string(),
                    })?;
                Ok(SecretString::new(value.expose()))
            }
            EnvironmentCredentialSource::AuthProviderCredential { provider_id } => {
                let provider = self
                    .providers
                    .read_auth_provider(provider_id)
                    .await
                    .map_err(|error| EnvironmentCredentialResolutionError::Source {
                        env_name: env_name.to_owned(),
                        message: error.to_string(),
                    })?;
                if provider.status != AuthProviderStatus::Active {
                    return Err(EnvironmentCredentialResolutionError::Source {
                        env_name: env_name.to_owned(),
                        message: format!("auth provider is not active: {provider_id}"),
                    });
                }
                let Some(secret_id) = provider.credential_secret else {
                    return Err(EnvironmentCredentialResolutionError::Source {
                        env_name: env_name.to_owned(),
                        message: format!("auth provider has no credential secret: {provider_id}"),
                    });
                };
                let (_, value) = self
                    .secrets
                    .read_secret(&secret_id)
                    .await
                    .map_err(|error| EnvironmentCredentialResolutionError::Source {
                        env_name: env_name.to_owned(),
                        message: error.to_string(),
                    })?;
                Ok(SecretString::new(value.expose()))
            }
            EnvironmentCredentialSource::DirectSecret { secret_id } => {
                let (_, value) = self.secrets.read_secret(secret_id).await.map_err(|error| {
                    EnvironmentCredentialResolutionError::Source {
                        env_name: env_name.to_owned(),
                        message: error.to_string(),
                    }
                })?;
                Ok(SecretString::new(value.expose()))
            }
        }
    }

    pub(crate) fn wrap_context(
        &self,
        mut context: EnvironmentToolContext,
        environment_id: EnvironmentId,
    ) -> EnvironmentToolContext {
        if let Some(process) = context.process.take() {
            context.process = Some(Arc::new(CredentialInjectingProcessExecutor {
                inner: process,
                resolver: self.clone(),
                environment_id: environment_id.clone(),
            }));
        }
        if let Some(jobs) = context.jobs.take() {
            context.jobs = Some(Arc::new(CredentialInjectingJobExecutor {
                inner: jobs,
                resolver: self.clone(),
                environment_id,
            }));
        }
        context
    }
}

#[derive(Debug, Error)]
pub(crate) enum EnvironmentCredentialResolutionError {
    #[error("credential env collides with explicit env: {env_name}")]
    EnvCollision { env_name: String },

    #[error("credential source for env {env_name} failed: {message}")]
    Source { env_name: String, message: String },

    #[error("credential store failed: {message}")]
    Store { message: String },
}

struct CredentialInjectingProcessExecutor {
    inner: Arc<dyn ProcessExecutor>,
    resolver: EnvironmentCredentialResolver,
    environment_id: EnvironmentId,
}

#[async_trait]
impl ProcessExecutor for CredentialInjectingProcessExecutor {
    async fn run_process(&self, mut request: ProcessRequest) -> ProcessExecResult<ProcessOutput> {
        let secret_env = self
            .resolver
            .resolve_secret_env(&self.environment_id, &request.env)
            .await
            .map_err(|error| ProcessError::InvalidRequest {
                message: error.to_string(),
            })?;
        for (name, value) in secret_env {
            request.secret_env.insert(name, value);
        }
        self.inner.run_process(request).await
    }

    async fn continue_process(
        &self,
        request: ContinueProcessRequest,
    ) -> ProcessExecResult<ProcessOutput> {
        self.inner.continue_process(request).await
    }
}

struct CredentialInjectingJobExecutor {
    inner: Arc<dyn JobExecutor>,
    resolver: EnvironmentCredentialResolver,
    environment_id: EnvironmentId,
}

#[async_trait]
impl JobExecutor for CredentialInjectingJobExecutor {
    async fn start_jobs(&self, mut request: StartJobsParams) -> JobExecResult<StartJobsResponse> {
        for job in &mut request.jobs {
            let secret_env = self
                .resolver
                .resolve_secret_env(&self.environment_id, &job.env)
                .await
                .map_err(|error| JobError::InvalidRequest {
                    message: error.to_string(),
                })?;
            for (name, value) in secret_env {
                job.secret_env.insert(name, value);
            }
        }
        self.inner.start_jobs(request).await
    }

    async fn read_jobs(
        &self,
        request: environment_protocol::data::jobs::ReadJobsParams,
    ) -> JobExecResult<environment_protocol::data::jobs::ReadJobsResponse> {
        self.inner.read_jobs(request).await
    }

    async fn cancel_jobs(
        &self,
        request: environment_protocol::data::jobs::CancelJobsParams,
    ) -> JobExecResult<environment_protocol::data::jobs::CancelJobsResponse> {
        self.inner.cancel_jobs(request).await
    }
}

fn registry_token_broker(store: Arc<PgStore>) -> Option<Arc<dyn AuthTokenBroker>> {
    let grants: Arc<dyn AuthGrantStore> = store.clone();
    let secrets: Arc<dyn SecretStore> = store.clone();
    let clients: Arc<dyn OAuthClientStore> = store.clone();
    let providers: Arc<dyn AuthProviderStore> = store.clone();
    let locks: Arc<dyn GrantRefreshLock> = store;
    let token_client = HttpOAuthTokenClient::new().ok()?;
    let github_api = HttpGitHubApiClient::new().ok()?;
    let broker = RegistryTokenBroker::new(grants.clone(), secrets.clone(), locks)
        .with_oauth_refresh(OAuthRefreshRuntime::new(clients, Arc::new(token_client)))
        .with_token_source(
            AuthProviderKind::GitHubApp,
            Arc::new(GitHubAppRuntime::new(
                providers,
                Arc::new(github_api),
                grants,
                secrets,
            )),
        );
    Some(Arc::new(broker))
}

fn token_audience_for_grant(
    grant: &AuthGrantRecord,
    environment_id: &EnvironmentId,
) -> TokenAudience {
    match grant.provider_kind {
        AuthProviderKind::GitHubApp => TokenAudience::GitHubApi(
            grant
                .audience
                .clone()
                .unwrap_or_else(|| DEFAULT_GITHUB_API_BASE_URL.to_owned()),
        ),
        AuthProviderKind::ModelOAuth => TokenAudience::ModelProvider(
            grant
                .audience
                .clone()
                .unwrap_or_else(|| format!("model:{}", grant.provider_id)),
        ),
        _ => TokenAudience::McpResource(
            grant
                .audience
                .clone()
                .unwrap_or_else(|| format!("environment:{}", environment_id.as_str())),
        ),
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use auth::{
        InMemoryAuthGrantStore, InMemoryAuthProviderStore, InMemorySecretStore, PutSecretRecord,
        SECRET_KIND_STATIC_BEARER, SecretId, SecretStore, SecretValue,
    };
    use environments::{
        EnvironmentCredentialRecord, EnvironmentRegistryError, ListEnvironmentCredentials,
        PutEnvironmentCredential,
    };

    use super::*;

    struct FixedCredentialStore {
        credentials: Vec<EnvironmentCredentialRecord>,
    }

    #[async_trait]
    impl EnvironmentCredentialStore for FixedCredentialStore {
        async fn bind_credential(
            &self,
            _record: PutEnvironmentCredential,
        ) -> Result<EnvironmentCredentialRecord, EnvironmentRegistryError> {
            panic!("test credential store is read-only")
        }

        async fn list_credentials(
            &self,
            request: ListEnvironmentCredentials,
        ) -> Result<Vec<EnvironmentCredentialRecord>, EnvironmentRegistryError> {
            Ok(self
                .credentials
                .iter()
                .filter(|credential| credential.environment_id == request.environment_id)
                .cloned()
                .collect())
        }

        async fn unbind_credential(
            &self,
            _environment_id: &EnvironmentId,
            _env_name: &str,
        ) -> Result<EnvironmentCredentialRecord, EnvironmentRegistryError> {
            panic!("test credential store is read-only")
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolves_credentials_from_environment_without_session_scope() {
        let environment_id = EnvironmentId::new("environment_1");
        let secret_id = SecretId::new("secret_1");
        let secrets = Arc::new(InMemorySecretStore::new());
        secrets
            .put_secret(PutSecretRecord {
                secret_id: secret_id.clone(),
                secret_kind: SECRET_KIND_STATIC_BEARER.to_owned(),
                value: SecretValue::new("environment-token"),
                created_at_ms: 1,
            })
            .await
            .expect("store secret");
        let credentials = Arc::new(FixedCredentialStore {
            credentials: vec![EnvironmentCredentialRecord {
                environment_id: environment_id.clone(),
                env_name: "GH_TOKEN".to_owned(),
                source: EnvironmentCredentialSource::DirectSecret { secret_id },
                created_at_ms: 1,
                updated_at_ms: 1,
            }],
        });
        let resolver = EnvironmentCredentialResolver::new(
            credentials,
            Arc::new(InMemoryAuthGrantStore::new()),
            Arc::new(InMemoryAuthProviderStore::new()),
            secrets,
            None,
        );

        let resolved = resolver
            .resolve_secret_env(&environment_id, &BTreeMap::new())
            .await
            .expect("resolve environment credentials");

        assert_eq!(resolved["GH_TOKEN"].expose(), "environment-token");
    }

    #[test]
    fn default_grant_audience_is_environment_scoped() {
        let grant = AuthGrantRecord {
            grant_id: auth::AuthGrantId::new("grant_1"),
            provider_id: "static".to_owned(),
            provider_kind: AuthProviderKind::StaticBearer,
            exposure: auth::AuthGrantExposure::Brokered,
            created_by: api::Attribution::Local,
            display_name: None,
            subject_hint: None,
            scopes: Vec::new(),
            audience: None,
            access_token_secret: None,
            refresh_token_secret: None,
            oauth_client: None,
            expires_at_ms: None,
            status: auth::AuthGrantStatus::Active,
            metadata: serde_json::Value::Object(Default::default()),
            last_leased_at_ms: None,
            lease_count: 0,
            created_at_ms: 1,
            updated_at_ms: 1,
        };

        assert_eq!(
            token_audience_for_grant(&grant, &EnvironmentId::new("environment_1")),
            TokenAudience::McpResource("environment:environment_1".to_owned())
        );
    }
}
