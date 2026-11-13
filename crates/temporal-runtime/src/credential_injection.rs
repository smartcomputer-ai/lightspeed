use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use auth::{
    AuthGrantRecord, AuthGrantStore, AuthProviderKind, AuthProviderStatus, AuthProviderStore,
    AuthTokenBroker, DEFAULT_GITHUB_API_BASE_URL, GitHubAppRuntime, GrantRefreshLock,
    HttpGitHubApiClient, HttpOAuthTokenClient, OAuthClientStore, OAuthRefreshRuntime,
    RegistryTokenBroker, SecretStore, TokenAudience,
};
use environment_protocol::{
    bytes::SecretString,
    data::{
        handshake::JobLimits,
        job::{
            CancelJobsParams, CancelJobsResponse, ReadJobsParams, ReadJobsResponse,
            StartJobsParams, StartJobsResponse,
        },
    },
};
use environments::{
    EnvironmentCredentialSource, EnvironmentCredentialSourceRefusal, EnvironmentCredentialStore,
    EnvironmentId, ListEnvironmentCredentials, require_bindable_grant, require_bindable_provider,
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

    /// Resolves one binding to the plain value its command receives. The
    /// rules for what may be bound are checked again here, against the
    /// grant or provider as it is stored now: a binding is refused when it
    /// was made before a rule existed, and no change to a record after the
    /// binding can route a credential the rules keep in the runtime.
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
                require_bindable_grant(&grant.grant_id, grant.exposure).map_err(|refusal| {
                    EnvironmentCredentialResolutionError::Refused {
                        env_name: env_name.to_owned(),
                        refusal,
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
                require_bindable_provider(&provider.provider_id, provider.provider_kind).map_err(
                    |refusal| EnvironmentCredentialResolutionError::Refused {
                        env_name: env_name.to_owned(),
                        refusal,
                    },
                )?;
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

    #[error("credential source for env {env_name} may not be bound into an environment: {refusal}")]
    Refused {
        env_name: String,
        refusal: EnvironmentCredentialSourceRefusal,
    },

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
    fn limits(&self) -> Option<JobLimits> {
        self.inner.limits()
    }

    async fn start_jobs(&self, mut request: StartJobsParams) -> JobExecResult<StartJobsResponse> {
        for job in &mut request.jobs {
            let secret_env = self
                .resolver
                .resolve_secret_env(&self.environment_id, &job.command.env)
                .await
                .map_err(|error| JobError::InvalidRequest {
                    message: error.to_string(),
                })?;
            job.command.secret_env.extend(secret_env);
        }
        self.inner.start_jobs(request).await
    }

    async fn read_jobs(&self, request: ReadJobsParams) -> JobExecResult<ReadJobsResponse> {
        self.inner.read_jobs(request).await
    }

    async fn cancel_jobs(&self, request: CancelJobsParams) -> JobExecResult<CancelJobsResponse> {
        self.inner.cancel_jobs(request).await
    }
}

fn registry_token_broker(store: Arc<PgStore>) -> Option<Arc<dyn AuthTokenBroker>> {
    let grants: Arc<dyn AuthGrantStore> = store.clone();
    let secrets: Arc<dyn SecretStore> = store.clone();
    let clients: Arc<dyn OAuthClientStore> = store.clone();
    let providers: Arc<dyn AuthProviderStore> = store.clone();
    let locks: Arc<dyn GrantRefreshLock> = store;
    // An MCP OAuth grant leased here is refreshed under the same
    // private-network list as one used by a session, so a grant that works in
    // a run does not fail only when an environment asks for it.
    let oauth_http = crate::worker::mcp::McpPrivateNetworkPolicy::from_env()
        .ok()?
        .oauth_http_client();
    let token_client = HttpOAuthTokenClient::new_with_mcp_http(Arc::new(oauth_http)).ok()?;
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
        AuthGrantExposure, AuthGrantId, AuthProviderConfig, AuthProviderId, CreateAuthGrantRecord,
        CreateAuthProviderRecord, InMemoryAuthGrantStore, InMemoryAuthProviderStore,
        InMemorySecretStore, ModelApiKeyConfig, PutSecretRecord, SECRET_KIND_STATIC_TOKEN,
        SecretId, SecretStore, SecretValue,
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

    const MODEL_PROVIDER: &str = "model:openai";

    /// Stores holding one active model API key provider whose key is
    /// `environment-token`.
    async fn model_api_key_stores() -> (Arc<InMemoryAuthProviderStore>, Arc<InMemorySecretStore>) {
        let secret_id = SecretId::new("secret_1");
        let secrets = Arc::new(InMemorySecretStore::new());
        secrets
            .put_secret(PutSecretRecord {
                secret_id: secret_id.clone(),
                secret_kind: SECRET_KIND_STATIC_TOKEN.to_owned(),
                value: SecretValue::new("environment-token"),
                created_at_ms: 1,
            })
            .await
            .expect("store secret");
        let providers = Arc::new(InMemoryAuthProviderStore::new());
        providers
            .create_auth_provider(CreateAuthProviderRecord {
                provider_id: AuthProviderId::new(MODEL_PROVIDER),
                display_name: None,
                config: AuthProviderConfig::ModelApiKey(ModelApiKeyConfig::default()),
                credential_secret: Some(secret_id),
                status: AuthProviderStatus::Active,
                created_at_ms: 1,
            })
            .await
            .expect("create model provider");
        (providers, secrets)
    }

    fn binding(
        environment_id: &EnvironmentId,
        env_name: &str,
        source: EnvironmentCredentialSource,
    ) -> EnvironmentCredentialRecord {
        EnvironmentCredentialRecord {
            environment_id: environment_id.clone(),
            env_name: env_name.to_owned(),
            source,
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    fn model_api_key_binding(environment_id: &EnvironmentId) -> EnvironmentCredentialRecord {
        binding(
            environment_id,
            "OPENAI_API_KEY",
            EnvironmentCredentialSource::AuthProviderCredential {
                provider_id: AuthProviderId::new(MODEL_PROVIDER),
            },
        )
    }

    fn static_token_grant(grant_id: &str, exposure: AuthGrantExposure) -> CreateAuthGrantRecord {
        CreateAuthGrantRecord {
            grant_id: AuthGrantId::new(grant_id),
            provider_id: "static".to_owned(),
            provider_kind: AuthProviderKind::StaticToken,
            exposure,
            created_by: api::Attribution::Local,
            display_name: None,
            subject_hint: None,
            scopes: Vec::new(),
            audience: None,
            access_token_secret: Some(SecretId::new(format!("{grant_id}_access"))),
            refresh_token_secret: None,
            oauth_client: None,
            expires_at_ms: None,
            status: auth::AuthGrantStatus::Active,
            metadata: serde_json::Value::Object(Default::default()),
            created_at_ms: 1,
        }
    }

    /// A broker that returns one fixed token and counts how often it is asked.
    #[derive(Default)]
    struct CountingBroker {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl AuthTokenBroker for CountingBroker {
        async fn bearer_token(
            &self,
            _grant_id: &AuthGrantId,
            _audience: &TokenAudience,
        ) -> Result<SecretValue, auth::AuthBrokerError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(SecretValue::new("grant-token"))
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolves_credentials_from_environment_without_session_scope() {
        let environment_id = EnvironmentId::new("environment_1");
        let (providers, secrets) = model_api_key_stores().await;
        let credentials = Arc::new(FixedCredentialStore {
            credentials: vec![model_api_key_binding(&environment_id)],
        });
        let resolver = EnvironmentCredentialResolver::new(
            credentials,
            Arc::new(InMemoryAuthGrantStore::new()),
            providers,
            secrets,
            None,
        );

        let resolved = resolver
            .resolve_secret_env(&environment_id, &BTreeMap::new())
            .await
            .expect("resolve environment credentials");

        assert_eq!(resolved["OPENAI_API_KEY"].expose(), "environment-token");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolves_a_retrievable_grant_through_the_broker() {
        let environment_id = EnvironmentId::new("environment_1");
        let grants = Arc::new(InMemoryAuthGrantStore::new());
        grants
            .create_grant(static_token_grant(
                "authgrant_repo",
                AuthGrantExposure::Retrievable,
            ))
            .await
            .expect("create grant");
        let broker = Arc::new(CountingBroker::default());
        let resolver = EnvironmentCredentialResolver::new(
            Arc::new(FixedCredentialStore {
                credentials: vec![binding(
                    &environment_id,
                    "GH_TOKEN",
                    EnvironmentCredentialSource::AuthGrant {
                        grant_id: AuthGrantId::new("authgrant_repo"),
                    },
                )],
            }),
            grants,
            Arc::new(InMemoryAuthProviderStore::new()),
            Arc::new(InMemorySecretStore::new()),
            Some(broker.clone()),
        );

        let resolved = resolver
            .resolve_secret_env(&environment_id, &BTreeMap::new())
            .await
            .expect("resolve retrievable grant");

        assert_eq!(resolved["GH_TOKEN"].expose(), "grant-token");
        assert_eq!(broker.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_bound_brokered_grant_is_refused_before_the_broker_is_asked() {
        let environment_id = EnvironmentId::new("environment_1");
        let grants = Arc::new(InMemoryAuthGrantStore::new());
        grants
            .create_grant(static_token_grant(
                "authgrant_repo",
                AuthGrantExposure::Brokered,
            ))
            .await
            .expect("create grant");
        let broker = Arc::new(CountingBroker::default());
        let resolver = EnvironmentCredentialResolver::new(
            Arc::new(FixedCredentialStore {
                credentials: vec![binding(
                    &environment_id,
                    "GH_TOKEN",
                    EnvironmentCredentialSource::AuthGrant {
                        grant_id: AuthGrantId::new("authgrant_repo"),
                    },
                )],
            }),
            grants,
            Arc::new(InMemoryAuthProviderStore::new()),
            Arc::new(InMemorySecretStore::new()),
            Some(broker.clone()),
        );

        let error = resolver
            .resolve_secret_env(&environment_id, &BTreeMap::new())
            .await
            .expect_err("a brokered grant never reaches a machine");

        assert!(matches!(
            error,
            EnvironmentCredentialResolutionError::Refused {
                ref env_name,
                refusal: EnvironmentCredentialSourceRefusal::BrokeredGrant { ref grant_id },
            } if env_name == "GH_TOKEN" && grant_id.as_str() == "authgrant_repo"
        ));
        assert_eq!(broker.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_bound_github_app_provider_is_refused_without_reading_its_key() {
        let environment_id = EnvironmentId::new("environment_1");
        let providers = Arc::new(InMemoryAuthProviderStore::new());
        providers
            .create_auth_provider(CreateAuthProviderRecord {
                provider_id: AuthProviderId::new("github"),
                display_name: None,
                config: AuthProviderConfig::GitHubApp(auth::GitHubAppConfig {
                    app_id: "12345".to_owned(),
                    api_base_url: DEFAULT_GITHUB_API_BASE_URL.to_owned(),
                }),
                // The key is deliberately absent from the secret store: a
                // refusal that came after the read would surface as a
                // missing secret instead.
                credential_secret: Some(SecretId::new("authsec_github_key")),
                status: AuthProviderStatus::Active,
                created_at_ms: 1,
            })
            .await
            .expect("create github app provider");
        let resolver = EnvironmentCredentialResolver::new(
            Arc::new(FixedCredentialStore {
                credentials: vec![binding(
                    &environment_id,
                    "GITHUB_APP_KEY",
                    EnvironmentCredentialSource::AuthProviderCredential {
                        provider_id: AuthProviderId::new("github"),
                    },
                )],
            }),
            Arc::new(InMemoryAuthGrantStore::new()),
            providers,
            Arc::new(InMemorySecretStore::new()),
            None,
        );

        let error = resolver
            .resolve_secret_env(&environment_id, &BTreeMap::new())
            .await
            .expect_err("an App private key never reaches a machine");

        assert!(matches!(
            error,
            EnvironmentCredentialResolutionError::Refused {
                ref env_name,
                refusal: EnvironmentCredentialSourceRefusal::GitHubAppProvider { ref provider_id },
            } if env_name == "GITHUB_APP_KEY" && provider_id.as_str() == "github"
        ));
    }

    #[derive(Default)]
    struct RecordingJobExecutor {
        started: std::sync::Mutex<Vec<StartJobsParams>>,
    }

    #[async_trait]
    impl JobExecutor for RecordingJobExecutor {
        fn limits(&self) -> Option<JobLimits> {
            Some(JobLimits {
                max_read_bytes: 1024,
                max_request_jobs: 4,
                max_retained_output_bytes: 2048,
                record_retention_ms: 1,
                cancel_grace_ms: 1,
            })
        }

        async fn start_jobs(&self, request: StartJobsParams) -> JobExecResult<StartJobsResponse> {
            self.started.lock().unwrap().push(request);
            Err(JobError::Failed {
                message: "recorded".to_owned(),
            })
        }

        async fn read_jobs(&self, _request: ReadJobsParams) -> JobExecResult<ReadJobsResponse> {
            unreachable!("not read")
        }

        async fn cancel_jobs(
            &self,
            _request: CancelJobsParams,
        ) -> JobExecResult<CancelJobsResponse> {
            unreachable!("not cancelled")
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn job_wrapper_injects_credentials_into_each_command_and_keeps_limits() {
        let environment_id = EnvironmentId::new("environment_1");
        let (providers, secrets) = model_api_key_stores().await;
        let resolver = EnvironmentCredentialResolver::new(
            Arc::new(FixedCredentialStore {
                credentials: vec![model_api_key_binding(&environment_id)],
            }),
            Arc::new(InMemoryAuthGrantStore::new()),
            providers,
            secrets,
            None,
        );
        let inner = Arc::new(RecordingJobExecutor::default());
        let wrapper = CredentialInjectingJobExecutor {
            inner: inner.clone(),
            resolver,
            environment_id,
        };
        assert_eq!(
            wrapper.limits().map(|limits| limits.max_request_jobs),
            Some(4)
        );

        let job = |job_id: &str| environment_protocol::data::job::JobSpec {
            job_id: environment_protocol::ids::JobId::new(job_id).unwrap(),
            name: None,
            command: environment_protocol::data::exec::CommandSpec {
                argv: vec!["true".to_owned()],
                cwd: None,
                env: BTreeMap::from([("CI".to_owned(), "1".to_owned())]),
                secret_env: BTreeMap::new(),
                input: Default::default(),
                timeout_ms: None,
            },
            depends_on: Vec::new(),
            dependency_policy: Default::default(),
            queue_key: None,
        };
        let _ = wrapper
            .start_jobs(StartJobsParams {
                request_id: environment_protocol::ids::JobRequestId::new("req-1").unwrap(),
                jobs: vec![job("a"), job("b")],
            })
            .await;

        let started = inner.started.lock().unwrap();
        for job in &started[0].jobs {
            assert_eq!(
                job.command.secret_env["OPENAI_API_KEY"].expose(),
                "environment-token"
            );
            assert_eq!(job.command.env["CI"], "1");
        }
    }

    #[test]
    fn default_grant_audience_is_environment_scoped() {
        let grant = AuthGrantRecord {
            grant_id: auth::AuthGrantId::new("grant_1"),
            provider_id: "static".to_owned(),
            provider_kind: AuthProviderKind::StaticToken,
            exposure: auth::AuthGrantExposure::Retrievable,
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
