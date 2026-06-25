use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AuthProviderKind {
    StaticBearer,
    McpOAuth,
    GitHubApp,
    GitHubAppUser,
    GitHubOAuthApp,
    CustomOAuth,
    ModelApiKey,
    ModelOAuth,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AuthGrantStatus {
    #[default]
    Active,
    NeedsReauth,
    Revoked,
    Failed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PrincipalKind {
    User,
    ServiceAccount,
    #[default]
    UniverseDefault,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PrincipalRefView {
    #[serde(default)]
    pub kind: PrincipalKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGrantView {
    pub grant_id: String,
    pub provider_id: String,
    pub provider_kind: AuthProviderKind,
    pub principal: PrincipalRefView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    pub has_access_token: bool,
    pub has_refresh_token: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
    pub status: AuthGrantStatus,
    /// Non-secret provider-specific metadata (for GitHub App installation
    /// grants: installation id, account, permissions, repository selection).
    #[serde(default, skip_serializing_if = "metadata_is_empty")]
    pub metadata: serde_json::Value,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

fn metadata_is_empty(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

/// Import a static bearer credential as an auth grant. This is the one
/// deliberate inbound-plaintext path: `token` is encrypted on receipt and is
/// never returned by any method. `Debug` output redacts the token; request
/// logging must never echo these params.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGrantImportParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
}

impl std::fmt::Debug for AuthGrantImportParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthGrantImportParams")
            .field("grant_id", &self.grant_id)
            .field("provider_id", &self.provider_id)
            .field("token", &"<redacted>")
            .field("display_name", &self.display_name)
            .field("subject_hint", &self.subject_hint)
            .field("scopes", &self.scopes)
            .field("audience", &self.audience)
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGrantImportResponse {
    pub grant: AuthGrantView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGrantListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<AuthGrantStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGrantListResponse {
    #[serde(default)]
    pub grants: Vec<AuthGrantView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGrantReadParams {
    pub grant_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGrantReadResponse {
    pub grant: AuthGrantView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGrantRevokeParams {
    pub grant_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGrantRevokeResponse {
    pub grant: AuthGrantView,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TokenEndpointAuthMethod {
    #[default]
    ClientSecretBasic,
    ClientSecretPost,
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OAuthClientView {
    pub client_id: String,
    pub provider_id: String,
    pub provider_kind: AuthProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub remote_client_id: String,
    pub has_client_secret: bool,
    pub token_endpoint_auth_method: TokenEndpointAuthMethod,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes_default: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Register an OAuth client configuration. `client_secret` is the second
/// deliberate inbound-plaintext path after `auth/grants/import`: it is
/// encrypted on receipt and never returned by any method. `Debug` output
/// redacts it; request logging must never echo these params.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthClientCreateParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    pub provider_kind: AuthProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub remote_client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_endpoint_auth_method: Option<TokenEndpointAuthMethod>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes_default: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
}

impl std::fmt::Debug for AuthClientCreateParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthClientCreateParams")
            .field("client_id", &self.client_id)
            .field("provider_id", &self.provider_id)
            .field("provider_kind", &self.provider_kind)
            .field("display_name", &self.display_name)
            .field("authorization_endpoint", &self.authorization_endpoint)
            .field("token_endpoint", &self.token_endpoint)
            .field("remote_client_id", &self.remote_client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "token_endpoint_auth_method",
                &self.token_endpoint_auth_method,
            )
            .field("scopes_default", &self.scopes_default)
            .field("audience", &self.audience)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthClientCreateResponse {
    pub client: OAuthClientView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthClientListParams {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthClientListResponse {
    #[serde(default)]
    pub clients: Vec<OAuthClientView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthClientReadParams {
    pub client_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthClientReadResponse {
    pub client: OAuthClientView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthClientDeleteParams {
    pub client_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthClientDeleteResponse {
    pub client: OAuthClientView,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AuthFlowStatus {
    Pending,
    Completed,
    Failed,
    Expired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthFlowStartParams {
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthFlowStartResponse {
    pub flow_id: String,
    /// Authorization URL the user must open. It embeds the one-time `state`;
    /// treat it as sensitive and do not log it server-side.
    pub authorize_url: String,
    pub expires_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthFlowStatusParams {
    pub flow_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthFlowView {
    pub flow_id: String,
    pub client_id: String,
    pub provider_id: String,
    pub status: AuthFlowStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub expires_at_ms: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthFlowStatusResponse {
    pub flow: AuthFlowView,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AuthProviderStatus {
    #[default]
    Active,
    NeedsConfiguration,
    Disabled,
}

/// Non-secret, provider-specific configuration. New providers add a
/// variant, not a table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum AuthProviderConfigView {
    #[serde(rename = "githubApp", rename_all = "camelCase")]
    GitHubApp {
        app_id: String,
        api_base_url: String,
    },
    /// Stored model provider API key (`model:<provider_id>` rows). The key itself
    /// is the provider credential and never appears in views.
    #[serde(rename = "modelApiKey", rename_all = "camelCase")]
    ModelApiKey {},
    /// OAuth-grant-backed model provider credential: provider calls send the
    /// bound grant's access token as an OAuth bearer token.
    #[serde(rename = "modelOAuth", rename_all = "camelCase")]
    ModelOAuth {
        grant_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audience: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum AuthProviderConfigInput {
    #[serde(rename = "githubApp", rename_all = "camelCase")]
    GitHubApp {
        app_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_base_url: Option<String>,
    },
    /// Stored model provider API key; the key arrives via `credential` and is
    /// encrypted on receipt.
    #[serde(rename = "modelApiKey", rename_all = "camelCase")]
    ModelApiKey {},
    /// Bind an existing auth grant as a model provider credential. No
    /// `credential` is accepted; the grant's tokens stay in the grant store.
    #[serde(rename = "modelOAuth", rename_all = "camelCase")]
    ModelOAuth {
        grant_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audience: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthProviderView {
    pub provider_id: String,
    pub provider_kind: AuthProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub config: AuthProviderConfigView,
    pub has_credential: bool,
    pub status: AuthProviderStatus,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Register an auth provider. `credential` (for GitHub Apps: the private
/// key PEM) is the third deliberate inbound-plaintext path: it is encrypted
/// on receipt and never returned by any method. `Debug` output redacts it;
/// request logging must never echo these params.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthProviderCreateParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub config: AuthProviderConfigInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

impl std::fmt::Debug for AuthProviderCreateParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthProviderCreateParams")
            .field("provider_id", &self.provider_id)
            .field("display_name", &self.display_name)
            .field("config", &self.config)
            .field(
                "credential",
                &self.credential.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthProviderCreateResponse {
    pub provider: AuthProviderView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthProviderListParams {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthProviderListResponse {
    #[serde(default)]
    pub providers: Vec<AuthProviderView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthProviderReadParams {
    pub provider_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthProviderReadResponse {
    pub provider: AuthProviderView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthProviderDeleteParams {
    pub provider_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthProviderDeleteResponse {
    pub provider: AuthProviderView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GitHubInstallationView {
    pub installation_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_login: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_selection: Option<String>,
    /// Fine-grained permission map as GitHub reports it.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub permissions: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGitHubInstallationListParams {
    pub provider_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGitHubInstallationListResponse {
    #[serde(default)]
    pub installations: Vec<GitHubInstallationView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGitHubInstallationGrantParams {
    pub provider_id: String,
    pub installation_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthGitHubInstallationGrantResponse {
    pub grant: AuthGrantView,
}
