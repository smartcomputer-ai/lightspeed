//! Authorization-code flow orchestration (P69 G2).
//!
//! [`OAuthFlowService`] drives the generic flow over the store traits and the
//! [`OAuthTokenClient`]: start builds the authorization URL and persists the
//! one-time flow record; the callback consumes the flow atomically, exchanges
//! the code, stores encrypted tokens, and creates the grant. Gateways stay
//! thin adapters over this service.

use std::sync::Arc;

use crate::{
    AuthFlowId, AuthFlowRecord, AuthFlowStore, AuthGrantId, AuthGrantStatus, AuthGrantStore,
    AuthRegistryError, CreateAuthFlowRecord, CreateAuthGrantRecord, FinishAuthFlow, OAuthClientId,
    OAuthClientStore, OAuthTokenClient, OAuthTokenGrant, OAuthTokenRequest, PrincipalRef,
    PutSecretRecord, SECRET_KIND_OAUTH_ACCESS_TOKEN, SECRET_KIND_OAUTH_PKCE_VERIFIER,
    SECRET_KIND_OAUTH_REFRESH_TOKEN, SecretId, SecretStore, SecretValue, build_authorization_url,
    generate_pkce_verifier, generate_state, pkce_challenge_s256, random_auth_id, state_hash,
    validate_audience_url,
};

pub const DEFAULT_AUTH_FLOW_TTL_MS: i64 = 10 * 60 * 1000;

/// Upper bound for error strings persisted onto flow records.
const FLOW_ERROR_MAX_LEN: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartAuthFlow {
    pub client_id: OAuthClientId,
    /// Redirect URI registered for this deployment (the gateway callback).
    pub redirect_uri: String,
    /// Overrides the client's default scopes when set.
    pub scopes: Option<Vec<String>>,
    /// Overrides the client's default audience when set.
    pub audience: Option<String>,
    pub principal: PrincipalRef,
}

#[derive(Clone, Debug)]
pub struct StartedAuthFlow {
    pub flow: AuthFlowRecord,
    /// Full authorization URL including the raw `state`; shown to the user,
    /// never persisted or logged.
    pub authorize_url: String,
}

/// Query parameters delivered to the redirect URI. `code` is secret-wrapped;
/// it must never be logged.
#[derive(Clone, Debug)]
pub struct AuthCallback {
    pub state: String,
    pub code: Option<SecretValue>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

pub struct OAuthFlowService {
    clients: Arc<dyn OAuthClientStore>,
    flows: Arc<dyn AuthFlowStore>,
    grants: Arc<dyn AuthGrantStore>,
    secrets: Arc<dyn SecretStore>,
    token_client: Arc<dyn OAuthTokenClient>,
    now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
    flow_ttl_ms: i64,
}

impl OAuthFlowService {
    pub fn new(
        clients: Arc<dyn OAuthClientStore>,
        flows: Arc<dyn AuthFlowStore>,
        grants: Arc<dyn AuthGrantStore>,
        secrets: Arc<dyn SecretStore>,
        token_client: Arc<dyn OAuthTokenClient>,
    ) -> Self {
        Self {
            clients,
            flows,
            grants,
            secrets,
            token_client,
            now_ms: Arc::new(crate::broker::system_now_ms),
            flow_ttl_ms: DEFAULT_AUTH_FLOW_TTL_MS,
        }
    }

    pub fn with_now_fn(mut self, now_ms: Arc<dyn Fn() -> i64 + Send + Sync>) -> Self {
        self.now_ms = now_ms;
        self
    }

    pub fn with_flow_ttl_ms(mut self, flow_ttl_ms: i64) -> Self {
        self.flow_ttl_ms = flow_ttl_ms.max(1);
        self
    }

    /// Start an authorization-code flow: persist the PKCE verifier and the
    /// one-time flow record, and return the authorization URL to open.
    pub async fn start_flow(
        &self,
        request: StartAuthFlow,
    ) -> Result<StartedAuthFlow, AuthRegistryError> {
        let client = self.clients.read_oauth_client(&request.client_id).await?;
        request.principal.validate()?;
        let scopes = request
            .scopes
            .unwrap_or_else(|| client.scopes_default.clone());
        let audience = match request.audience.or_else(|| client.audience.clone()) {
            Some(audience) => {
                validate_audience_url(&audience)?;
                Some(audience)
            }
            None => None,
        };
        validate_audience_url(&request.redirect_uri).map_err(|error| match error {
            AuthRegistryError::InvalidInput { message } => AuthRegistryError::InvalidInput {
                message: format!("redirect uri: {message}"),
            },
            other => other,
        })?;

        let now_ms = (self.now_ms)();
        let state = generate_state();
        let verifier = generate_pkce_verifier();
        let challenge = pkce_challenge_s256(&verifier);

        let verifier_secret_id = self.random_secret_id()?;
        self.secrets
            .put_secret(PutSecretRecord {
                secret_id: verifier_secret_id.clone(),
                secret_kind: SECRET_KIND_OAUTH_PKCE_VERIFIER.to_owned(),
                value: verifier,
                created_at_ms: now_ms,
            })
            .await?;

        let flow_id = AuthFlowId::try_new(random_auth_id("authflow_")).map_err(|error| {
            AuthRegistryError::Store {
                message: format!("generate auth flow id: {error}"),
            }
        })?;
        let create = CreateAuthFlowRecord {
            flow_id,
            client_id: client.client_id.clone(),
            provider_id: client.provider_id.clone(),
            provider_kind: client.provider_kind,
            principal: request.principal,
            state_hash: state_hash(&state),
            pkce_verifier_secret: verifier_secret_id.clone(),
            redirect_uri: request.redirect_uri.clone(),
            scopes: scopes.clone(),
            audience: audience.clone(),
            expires_at_ms: now_ms.saturating_add(self.flow_ttl_ms),
            created_at_ms: now_ms,
        };
        let flow = match self.flows.create_flow(create).await {
            Ok(flow) => flow,
            Err(error) => {
                // The verifier is orphaned without its flow; clean up
                // best-effort and surface the original failure.
                let _ = self.secrets.delete_secret(&verifier_secret_id).await;
                return Err(error);
            }
        };

        let authorize_url = build_authorization_url(
            &client,
            &request.redirect_uri,
            &scopes,
            &state,
            &challenge,
            audience.as_deref(),
        );
        Ok(StartedAuthFlow {
            flow,
            authorize_url,
        })
    }

    pub async fn read_flow(
        &self,
        flow_id: &AuthFlowId,
    ) -> Result<AuthFlowRecord, AuthRegistryError> {
        self.flows.read_flow(flow_id).await
    }

    pub fn now_ms(&self) -> i64 {
        (self.now_ms)()
    }

    /// Complete an authorization callback. The flow is consumed atomically
    /// before any code exchange, so a duplicate or replayed callback fails
    /// with a typed error instead of racing. Exchange or provider failures
    /// finish the flow as `Failed`; the returned record carries the outcome.
    pub async fn complete_callback(
        &self,
        callback: AuthCallback,
    ) -> Result<AuthFlowRecord, AuthRegistryError> {
        if callback.state.is_empty() {
            return Err(AuthRegistryError::UnknownCallbackState);
        }
        let Some(flow) = self
            .flows
            .read_flow_by_state_hash(&state_hash(&callback.state))
            .await?
        else {
            return Err(AuthRegistryError::UnknownCallbackState);
        };
        let now_ms = (self.now_ms)();
        let flow = self.flows.consume_flow(&flow.flow_id, now_ms).await?;

        if let Some(error) = &callback.error {
            let message = match &callback.error_description {
                Some(description) => format!("authorization failed: {error}: {description}"),
                None => format!("authorization failed: {error}"),
            };
            return self.fail_flow(&flow, message).await;
        }
        let Some(code) = callback.code else {
            return self
                .fail_flow(
                    &flow,
                    "callback is missing the authorization code".to_owned(),
                )
                .await;
        };

        let client = match self.clients.read_oauth_client(&flow.client_id).await {
            Ok(client) => client,
            Err(AuthRegistryError::ClientNotFound { client_id }) => {
                return self
                    .fail_flow(&flow, format!("oauth client {client_id} no longer exists"))
                    .await;
            }
            Err(error) => return Err(error),
        };
        let (_, verifier) = self.secrets.read_secret(&flow.pkce_verifier_secret).await?;
        let client_secret = match &client.client_secret {
            Some(secret_id) => Some(self.secrets.read_secret(secret_id).await?.1),
            None => None,
        };

        let token_request = OAuthTokenRequest {
            token_endpoint: client.token_endpoint.clone(),
            remote_client_id: client.remote_client_id.clone(),
            client_secret,
            auth_method: client.token_endpoint_auth_method,
            grant: OAuthTokenGrant::AuthorizationCode {
                code,
                redirect_uri: flow.redirect_uri.clone(),
                code_verifier: verifier,
            },
            resource: flow.audience.clone(),
        };
        let response = match self.token_client.request_token(&token_request).await {
            Ok(response) => response,
            Err(error) => return self.fail_flow(&flow, error.to_string()).await,
        };
        if !response.token_type.eq_ignore_ascii_case("bearer") {
            return self
                .fail_flow(
                    &flow,
                    format!("unsupported token_type {:?}", response.token_type),
                )
                .await;
        }

        let now_ms = (self.now_ms)();
        let access_secret_id = self.random_secret_id()?;
        self.secrets
            .put_secret(PutSecretRecord {
                secret_id: access_secret_id.clone(),
                secret_kind: SECRET_KIND_OAUTH_ACCESS_TOKEN.to_owned(),
                value: response.access_token,
                created_at_ms: now_ms,
            })
            .await?;
        let refresh_secret_id = match response.refresh_token {
            Some(refresh_token) => {
                let refresh_secret_id = self.random_secret_id()?;
                let put = self
                    .secrets
                    .put_secret(PutSecretRecord {
                        secret_id: refresh_secret_id.clone(),
                        secret_kind: SECRET_KIND_OAUTH_REFRESH_TOKEN.to_owned(),
                        value: refresh_token,
                        created_at_ms: now_ms,
                    })
                    .await;
                if let Err(error) = put {
                    let _ = self.secrets.delete_secret(&access_secret_id).await;
                    return Err(error);
                }
                Some(refresh_secret_id)
            }
            None => None,
        };

        let scopes = match &response.scope {
            Some(scope) => scope.split_whitespace().map(str::to_owned).collect(),
            None => flow.scopes.clone(),
        };
        let grant_id = AuthGrantId::try_new(random_auth_id("authgrant_")).map_err(|error| {
            AuthRegistryError::Store {
                message: format!("generate auth grant id: {error}"),
            }
        })?;
        let create_grant = CreateAuthGrantRecord {
            grant_id: grant_id.clone(),
            provider_id: flow.provider_id.clone(),
            provider_kind: flow.provider_kind,
            principal: flow.principal.clone(),
            display_name: client.display_name.clone(),
            subject_hint: None,
            scopes,
            audience: flow.audience.clone(),
            access_token_secret: Some(access_secret_id.clone()),
            refresh_token_secret: refresh_secret_id.clone(),
            oauth_client: Some(client.client_id.clone()),
            expires_at_ms: response
                .expires_in_secs
                .map(|secs| now_ms.saturating_add(secs.saturating_mul(1000))),
            status: AuthGrantStatus::Active,
            metadata: serde_json::Value::Object(Default::default()),
            created_at_ms: now_ms,
        };
        if let Err(error) = self.grants.create_grant(create_grant).await {
            let _ = self.secrets.delete_secret(&access_secret_id).await;
            if let Some(refresh_secret_id) = &refresh_secret_id {
                let _ = self.secrets.delete_secret(refresh_secret_id).await;
            }
            return self
                .fail_flow(&flow, format!("store auth grant: {error}"))
                .await;
        }

        let finished = self
            .flows
            .finish_flow(
                &flow.flow_id,
                FinishAuthFlow {
                    grant_id: Some(grant_id),
                    error: None,
                    completed_at_ms: now_ms,
                },
            )
            .await?;
        // The verifier is single-use; it has served its purpose.
        let _ = self.secrets.delete_secret(&flow.pkce_verifier_secret).await;
        Ok(finished)
    }

    async fn fail_flow(
        &self,
        flow: &AuthFlowRecord,
        message: String,
    ) -> Result<AuthFlowRecord, AuthRegistryError> {
        let message = truncate_error(message);
        let finished = self
            .flows
            .finish_flow(
                &flow.flow_id,
                FinishAuthFlow {
                    grant_id: None,
                    error: Some(message),
                    completed_at_ms: (self.now_ms)(),
                },
            )
            .await?;
        let _ = self.secrets.delete_secret(&flow.pkce_verifier_secret).await;
        Ok(finished)
    }

    fn random_secret_id(&self) -> Result<SecretId, AuthRegistryError> {
        SecretId::try_new(random_auth_id("authsec_")).map_err(|error| AuthRegistryError::Store {
            message: format!("generate secret id: {error}"),
        })
    }
}

fn truncate_error(message: String) -> String {
    if message.len() <= FLOW_ERROR_MAX_LEN {
        return message;
    }
    let mut end = FLOW_ERROR_MAX_LEN;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &message[..end])
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use crate::{
        AuthFlowStatus, AuthProviderKind, CreateOAuthClientRecord, InMemoryAuthFlowStore,
        InMemoryAuthGrantStore, InMemoryOAuthClientStore, InMemorySecretStore, OAuthTokenError,
        OAuthTokenResponse, TokenEndpointAuthMethod,
    };

    use super::*;
    use async_trait::async_trait;

    struct FakeTokenClient {
        responses: Mutex<Vec<Result<OAuthTokenResponse, OAuthTokenError>>>,
        requests: Mutex<Vec<OAuthTokenRequest>>,
    }

    impl FakeTokenClient {
        fn with_responses(responses: Vec<Result<OAuthTokenResponse, OAuthTokenError>>) -> Self {
            Self {
                responses: Mutex::new(responses),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl OAuthTokenClient for FakeTokenClient {
        async fn request_token(
            &self,
            request: &OAuthTokenRequest,
        ) -> Result<OAuthTokenResponse, OAuthTokenError> {
            self.requests.lock().expect("lock").push(request.clone());
            self.responses.lock().expect("lock").remove(0)
        }
    }

    fn token_response(refresh: Option<&str>) -> OAuthTokenResponse {
        OAuthTokenResponse {
            access_token: SecretValue::new("at-1"),
            token_type: "Bearer".to_owned(),
            expires_in_secs: Some(3600),
            refresh_token: refresh.map(SecretValue::new),
            scope: None,
        }
    }

    struct Harness {
        service: OAuthFlowService,
        clients: Arc<InMemoryOAuthClientStore>,
        flows: Arc<InMemoryAuthFlowStore>,
        grants: Arc<InMemoryAuthGrantStore>,
        secrets: Arc<InMemorySecretStore>,
        token_client: Arc<FakeTokenClient>,
    }

    async fn harness(responses: Vec<Result<OAuthTokenResponse, OAuthTokenError>>) -> Harness {
        let clients = Arc::new(InMemoryOAuthClientStore::new());
        let flows = Arc::new(InMemoryAuthFlowStore::new());
        let grants = Arc::new(InMemoryAuthGrantStore::new());
        let secrets = Arc::new(InMemorySecretStore::new());
        let token_client = Arc::new(FakeTokenClient::with_responses(responses));
        clients
            .create_oauth_client(CreateOAuthClientRecord {
                client_id: OAuthClientId::new("crm"),
                provider_id: "crm".to_owned(),
                provider_kind: AuthProviderKind::McpOAuth,
                display_name: Some("CRM".to_owned()),
                authorization_endpoint: "https://as.example.com/authorize".to_owned(),
                token_endpoint: "https://as.example.com/token".to_owned(),
                remote_client_id: "client-1".to_owned(),
                client_secret: None,
                token_endpoint_auth_method: TokenEndpointAuthMethod::None,
                scopes_default: vec!["contacts.read".to_owned()],
                audience: Some("https://crm.example.com/mcp".to_owned()),
                created_at_ms: 10,
            })
            .await
            .expect("create client");
        let service = OAuthFlowService::new(
            clients.clone(),
            flows.clone(),
            grants.clone(),
            secrets.clone(),
            token_client.clone(),
        )
        .with_now_fn(Arc::new(|| 1_000));
        Harness {
            service,
            clients,
            flows,
            grants,
            secrets,
            token_client,
        }
    }

    fn start_request() -> StartAuthFlow {
        StartAuthFlow {
            client_id: OAuthClientId::new("crm"),
            redirect_uri: "https://lightspeed.example.com/auth/callback".to_owned(),
            scopes: None,
            audience: None,
            principal: PrincipalRef::universe_default(),
        }
    }

    fn state_from_url(url: &str) -> String {
        url.split('&')
            .chain(url.split('?'))
            .find_map(|part| part.strip_prefix("state="))
            .expect("state param")
            .to_owned()
    }

    #[tokio::test]
    async fn started_flows_persist_hash_and_verifier_but_not_state() {
        let harness = harness(Vec::new()).await;

        let started = harness
            .service
            .start_flow(start_request())
            .await
            .expect("start flow");

        let state = state_from_url(&started.authorize_url);
        assert_eq!(started.flow.state_hash, state_hash(&state));
        assert_ne!(started.flow.state_hash, state);
        assert_eq!(started.flow.status(1_001), AuthFlowStatus::Pending);
        assert_eq!(started.flow.expires_at_ms, 1_000 + DEFAULT_AUTH_FLOW_TTL_MS);
        let (meta, _) = harness
            .secrets
            .read_secret(&started.flow.pkce_verifier_secret)
            .await
            .expect("verifier stored");
        assert_eq!(meta.secret_kind, SECRET_KIND_OAUTH_PKCE_VERIFIER);
        assert!(started.authorize_url.contains("code_challenge="));
        assert!(
            started
                .authorize_url
                .contains("resource=https%3A%2F%2Fcrm.example.com%2Fmcp")
        );
    }

    #[tokio::test]
    async fn callbacks_exchange_codes_and_create_grants() {
        let harness = harness(vec![Ok(token_response(Some("rt-1")))]).await;
        let started = harness
            .service
            .start_flow(start_request())
            .await
            .expect("start flow");
        let state = state_from_url(&started.authorize_url);

        let finished = harness
            .service
            .complete_callback(AuthCallback {
                state,
                code: Some(SecretValue::new("code-1")),
                error: None,
                error_description: None,
            })
            .await
            .expect("complete callback");

        assert_eq!(finished.status(1_001), AuthFlowStatus::Completed);
        let grant_id = finished.grant_id.expect("grant id");
        let grant = harness.grants.read_grant(&grant_id).await.expect("grant");
        assert_eq!(grant.provider_kind, AuthProviderKind::McpOAuth);
        assert_eq!(
            grant.audience.as_deref(),
            Some("https://crm.example.com/mcp")
        );
        assert_eq!(grant.oauth_client, Some(OAuthClientId::new("crm")));
        assert_eq!(grant.expires_at_ms, Some(1_000 + 3_600_000));
        assert_eq!(grant.scopes, vec!["contacts.read".to_owned()]);

        let access_secret = grant.access_token_secret.expect("access secret");
        let (meta, value) = harness
            .secrets
            .read_secret(&access_secret)
            .await
            .expect("access token stored");
        assert_eq!(meta.secret_kind, SECRET_KIND_OAUTH_ACCESS_TOKEN);
        assert_eq!(value.expose(), "at-1");
        let refresh_secret = grant.refresh_token_secret.expect("refresh secret");
        let (_, value) = harness
            .secrets
            .read_secret(&refresh_secret)
            .await
            .expect("refresh token stored");
        assert_eq!(value.expose(), "rt-1");

        // The verifier is deleted after use.
        assert!(matches!(
            harness
                .secrets
                .read_secret(&started.flow.pkce_verifier_secret)
                .await,
            Err(AuthRegistryError::SecretNotFound { .. })
        ));

        // The exchange carried PKCE and the resource indicator.
        let requests = harness.token_client.requests.lock().expect("lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].resource.as_deref(),
            Some("https://crm.example.com/mcp")
        );
        assert!(matches!(
            &requests[0].grant,
            OAuthTokenGrant::AuthorizationCode { .. }
        ));
    }

    #[tokio::test]
    async fn second_callback_with_same_state_fails() {
        let harness = harness(vec![Ok(token_response(None))]).await;
        let started = harness
            .service
            .start_flow(start_request())
            .await
            .expect("start flow");
        let state = state_from_url(&started.authorize_url);

        harness
            .service
            .complete_callback(AuthCallback {
                state: state.clone(),
                code: Some(SecretValue::new("code-1")),
                error: None,
                error_description: None,
            })
            .await
            .expect("first callback");

        let error = harness
            .service
            .complete_callback(AuthCallback {
                state,
                code: Some(SecretValue::new("code-1")),
                error: None,
                error_description: None,
            })
            .await
            .expect_err("replayed callback must fail");

        assert!(matches!(
            error,
            AuthRegistryError::FlowAlreadyConsumed { .. }
        ));
    }

    #[tokio::test]
    async fn unknown_state_is_rejected_without_consuming_anything() {
        let harness = harness(Vec::new()).await;
        harness
            .service
            .start_flow(start_request())
            .await
            .expect("start flow");

        let error = harness
            .service
            .complete_callback(AuthCallback {
                state: "forged-state".to_owned(),
                code: Some(SecretValue::new("code-1")),
                error: None,
                error_description: None,
            })
            .await
            .expect_err("forged state must fail");

        assert!(matches!(error, AuthRegistryError::UnknownCallbackState));
    }

    #[tokio::test]
    async fn expired_flows_cannot_be_completed() {
        let harness = harness(Vec::new()).await;
        let service = harness.service.with_flow_ttl_ms(1);
        let started = service.start_flow(start_request()).await.expect("start");
        let state = state_from_url(&started.authorize_url);
        let service = service.with_now_fn(Arc::new(|| 5_000));

        let error = service
            .complete_callback(AuthCallback {
                state,
                code: Some(SecretValue::new("code-1")),
                error: None,
                error_description: None,
            })
            .await
            .expect_err("expired flow must fail");

        assert!(matches!(error, AuthRegistryError::FlowExpired { .. }));
    }

    #[tokio::test]
    async fn provider_denial_finishes_the_flow_as_failed() {
        let harness = harness(Vec::new()).await;
        let started = harness
            .service
            .start_flow(start_request())
            .await
            .expect("start flow");
        let state = state_from_url(&started.authorize_url);

        let finished = harness
            .service
            .complete_callback(AuthCallback {
                state,
                code: None,
                error: Some("access_denied".to_owned()),
                error_description: Some("user cancelled".to_owned()),
            })
            .await
            .expect("denied callback resolves the flow");

        assert_eq!(finished.status(1_001), AuthFlowStatus::Failed);
        let error = finished.error.expect("failure message");
        assert!(error.contains("access_denied"));
        assert!(finished.grant_id.is_none());
        // No grants and no leftover token secrets were created.
        assert!(
            harness
                .grants
                .list_grants(Default::default())
                .await
                .expect("list grants")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn failed_exchanges_finish_the_flow_as_failed() {
        let harness = harness(vec![Err(OAuthTokenError::InvalidGrant {
            description: Some("code expired".to_owned()),
        })])
        .await;
        let started = harness
            .service
            .start_flow(start_request())
            .await
            .expect("start flow");
        let state = state_from_url(&started.authorize_url);

        let finished = harness
            .service
            .complete_callback(AuthCallback {
                state,
                code: Some(SecretValue::new("code-1")),
                error: None,
                error_description: None,
            })
            .await
            .expect("failed exchange resolves the flow");

        assert_eq!(finished.status(1_001), AuthFlowStatus::Failed);
        assert!(finished.error.expect("error").contains("invalid_grant"));
        let flow = harness
            .flows
            .read_flow(&started.flow.flow_id)
            .await
            .expect("flow record");
        assert_eq!(flow.status(1_001), AuthFlowStatus::Failed);
    }

    #[tokio::test]
    async fn scope_overrides_replace_client_defaults() {
        let harness = harness(Vec::new()).await;
        let mut request = start_request();
        request.scopes = Some(vec!["contacts.write".to_owned()]);

        let started = harness
            .service
            .start_flow(request)
            .await
            .expect("start flow");

        assert_eq!(started.flow.scopes, vec!["contacts.write".to_owned()]);
        assert!(started.authorize_url.contains("scope=contacts.write"));
        // Keep the unused-field warning away and assert the client is intact.
        assert_eq!(
            harness
                .clients
                .list_oauth_clients()
                .await
                .expect("list clients")
                .len(),
            1
        );
    }
}
