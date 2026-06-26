use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use auth::{
    AuthGrantRecord, AuthGrantStore, AuthProviderKind, AuthProviderStatus, AuthProviderStore,
    AuthTokenBroker, DEFAULT_GITHUB_API_BASE_URL, GitHubAppRuntime, GrantRefreshLock,
    HttpGitHubApiClient, HttpOAuthTokenClient, OAuthClientStore, OAuthRefreshRuntime,
    RegistryTokenBroker, SecretStore, TokenAudience,
};
use engine::SessionId;
use environments::{
    EnvironmentId, ListSessionEnvironmentCredentials, SessionEnvironmentCredentialSource,
    SessionEnvironmentCredentialStore,
};
use host_protocol::{
    data::jobs::{StartJobsParams, StartJobsResponse},
    shared::SecretString,
};
use store_pg::PgStore;
use thiserror::Error;
use tools::environment::{
    EnvironmentToolContext,
    jobs::{JobError, JobExecResult, JobExecutor},
    process::{
        ProcessError, ProcessExecResult, ProcessExecutor, ProcessOutput, ProcessRequest,
        WriteProcessStdinRequest,
    },
};

#[derive(Clone)]
pub(crate) struct EnvironmentCredentialResolver {
    credentials: Arc<dyn SessionEnvironmentCredentialStore>,
    grants: Arc<dyn AuthGrantStore>,
    providers: Arc<dyn AuthProviderStore>,
    secrets: Arc<dyn SecretStore>,
    broker: Option<Arc<dyn AuthTokenBroker>>,
}

impl EnvironmentCredentialResolver {
    pub(crate) fn from_pg_store(store: Arc<PgStore>) -> Self {
        let credentials: Arc<dyn SessionEnvironmentCredentialStore> = store.clone();
        let grants: Arc<dyn AuthGrantStore> = store.clone();
        let providers: Arc<dyn AuthProviderStore> = store.clone();
        let secrets: Arc<dyn SecretStore> = store.clone();
        let broker = registry_token_broker(store);
        Self {
            credentials,
            grants,
            providers,
            secrets,
            broker,
        }
    }

    pub(crate) async fn resolve_secret_env(
        &self,
        session_id: &SessionId,
        env_id: &EnvironmentId,
        explicit_env: &BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, SecretString>, EnvironmentCredentialResolutionError> {
        let bindings = self
            .credentials
            .list_credentials(ListSessionEnvironmentCredentials {
                session_id: session_id.clone(),
                env_id: env_id.clone(),
            })
            .await
            .map_err(|error| EnvironmentCredentialResolutionError::Store {
                message: error.to_string(),
            })?;

        let mut secret_env = BTreeMap::new();
        let mut resolved_sources: BTreeMap<SessionEnvironmentCredentialSource, SecretString> =
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
                    .resolve_source(&binding.env_name, &binding.source, session_id, env_id)
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
        source: &SessionEnvironmentCredentialSource,
        session_id: &SessionId,
        env_id: &EnvironmentId,
    ) -> Result<SecretString, EnvironmentCredentialResolutionError> {
        match source {
            SessionEnvironmentCredentialSource::AuthGrant { grant_id } => {
                let grant = self.grants.read_grant(grant_id).await.map_err(|error| {
                    EnvironmentCredentialResolutionError::Source {
                        env_name: env_name.to_owned(),
                        message: error.to_string(),
                    }
                })?;
                let audience = token_audience_for_grant(&grant, session_id, env_id);
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
            SessionEnvironmentCredentialSource::AuthProviderCredential { provider_id } => {
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
            SessionEnvironmentCredentialSource::DirectSecret { secret_id } => {
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
        session_id: SessionId,
        env_id: EnvironmentId,
    ) -> EnvironmentToolContext {
        if let Some(process) = context.process.take() {
            context.process = Some(Arc::new(CredentialInjectingProcessExecutor {
                inner: process,
                resolver: self.clone(),
                session_id: session_id.clone(),
                env_id: env_id.clone(),
            }));
        }
        if let Some(jobs) = context.jobs.take() {
            context.jobs = Some(Arc::new(CredentialInjectingJobExecutor {
                inner: jobs,
                resolver: self.clone(),
                session_id,
                env_id,
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
    session_id: SessionId,
    env_id: EnvironmentId,
}

#[async_trait]
impl ProcessExecutor for CredentialInjectingProcessExecutor {
    async fn run_process(&self, mut request: ProcessRequest) -> ProcessExecResult<ProcessOutput> {
        let secret_env = self
            .resolver
            .resolve_secret_env(&self.session_id, &self.env_id, &request.env)
            .await
            .map_err(|error| ProcessError::InvalidRequest {
                message: error.to_string(),
            })?;
        for (name, value) in secret_env {
            request.secret_env.insert(name, value);
        }
        self.inner.run_process(request).await
    }

    async fn write_stdin(
        &self,
        request: WriteProcessStdinRequest,
    ) -> ProcessExecResult<ProcessOutput> {
        self.inner.write_stdin(request).await
    }
}

struct CredentialInjectingJobExecutor {
    inner: Arc<dyn JobExecutor>,
    resolver: EnvironmentCredentialResolver,
    session_id: SessionId,
    env_id: EnvironmentId,
}

#[async_trait]
impl JobExecutor for CredentialInjectingJobExecutor {
    async fn start_jobs(&self, mut request: StartJobsParams) -> JobExecResult<StartJobsResponse> {
        for job in &mut request.jobs {
            let secret_env = self
                .resolver
                .resolve_secret_env(&self.session_id, &self.env_id, &job.env)
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

    async fn list_jobs(
        &self,
        request: host_protocol::data::jobs::ListJobsParams,
    ) -> JobExecResult<host_protocol::data::jobs::ListJobsResponse> {
        self.inner.list_jobs(request).await
    }

    async fn read_jobs(
        &self,
        request: host_protocol::data::jobs::ReadJobsParams,
    ) -> JobExecResult<host_protocol::data::jobs::ReadJobsResponse> {
        self.inner.read_jobs(request).await
    }

    async fn cancel_jobs(
        &self,
        request: host_protocol::data::jobs::CancelJobsParams,
    ) -> JobExecResult<host_protocol::data::jobs::CancelJobsResponse> {
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
    session_id: &SessionId,
    env_id: &EnvironmentId,
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
        _ => {
            TokenAudience::McpResource(grant.audience.clone().unwrap_or_else(|| {
                format!("environment:{}/{}", session_id.as_str(), env_id.as_str())
            }))
        }
    }
}
