use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    AuthGrantId, AuthRegistryError, OAuthClientId, SecretId, validate_audience_url,
    validate_nonempty_optional, validate_nonnegative_i64, validate_scopes,
    validate_token_component,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AuthProviderKind {
    #[serde(rename = "static_bearer")]
    StaticBearer,
    #[serde(rename = "mcp_oauth")]
    McpOAuth,
    #[serde(rename = "github_app")]
    GitHubApp,
    #[serde(rename = "github_app_user")]
    GitHubAppUser,
    #[serde(rename = "github_oauth_app")]
    GitHubOAuthApp,
    #[serde(rename = "custom_oauth")]
    CustomOAuth,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthGrantStatus {
    #[default]
    Active,
    NeedsReauth,
    Revoked,
    Failed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKind {
    User,
    ServiceAccount,
    #[default]
    UniverseDefault,
}

/// Who a grant was issued to. Forge has no user identity yet, so the default
/// principal is `UniverseDefault` with no id; the shape exists so adding
/// identity later is a data migration, not a redesign.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrincipalRef {
    pub kind: PrincipalKind,
    pub id: Option<String>,
}

impl PrincipalRef {
    pub fn universe_default() -> Self {
        Self::default()
    }

    pub fn validate(&self) -> Result<(), AuthRegistryError> {
        match (self.kind, self.id.as_deref()) {
            (PrincipalKind::UniverseDefault, None) => Ok(()),
            (PrincipalKind::UniverseDefault, Some(_)) => Err(AuthRegistryError::InvalidInput {
                message: "universe_default principal must not carry an id".to_owned(),
            }),
            (PrincipalKind::User | PrincipalKind::ServiceAccount, Some(id)) => {
                validate_token_component("principal id", id)
            }
            (PrincipalKind::User | PrincipalKind::ServiceAccount, None) => {
                Err(AuthRegistryError::InvalidInput {
                    message: "user and service_account principals require an id".to_owned(),
                })
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthGrantRecord {
    pub grant_id: AuthGrantId,
    pub provider_id: String,
    pub provider_kind: AuthProviderKind,
    pub principal: PrincipalRef,
    pub display_name: Option<String>,
    pub subject_hint: Option<String>,
    pub scopes: Vec<String>,
    /// Normalized resource the grant is bound to. `None` means unrestricted;
    /// for MCP this is the canonical server resource URL (RFC 8707 resource).
    pub audience: Option<String>,
    pub access_token_secret: Option<SecretId>,
    pub refresh_token_secret: Option<SecretId>,
    /// The OAuth client configuration this grant was minted through, when it
    /// came from an authorization flow. The broker uses it to resolve token
    /// endpoint and client credentials for refresh.
    pub oauth_client: Option<OAuthClientId>,
    /// For OAuth grants this is the access-token expiry; the broker refreshes
    /// past the margin when a refresh token exists. For static grants it is a
    /// hard expiry.
    pub expires_at_ms: Option<i64>,
    pub status: AuthGrantStatus,
    /// Non-secret, provider-specific metadata. For GitHub App installation
    /// grants this carries the installation id, account, permissions, and
    /// repository selection. Must be a JSON object; never secret values.
    #[serde(default = "empty_metadata")]
    pub metadata: serde_json::Value,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

pub(crate) fn empty_metadata() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

impl AuthGrantRecord {
    pub fn validate(&self) -> Result<(), AuthRegistryError> {
        validate_token_component("provider id", &self.provider_id)?;
        self.principal.validate()?;
        validate_nonempty_optional("display_name", self.display_name.as_deref())?;
        validate_nonempty_optional("subject_hint", self.subject_hint.as_deref())?;
        validate_scopes(&self.scopes)?;
        if let Some(audience) = &self.audience {
            validate_audience_url(audience)?;
        }
        if let Some(expires_at_ms) = self.expires_at_ms {
            validate_nonnegative_i64(expires_at_ms, "expires_at_ms")?;
        }
        if !self.metadata.is_object() {
            return Err(AuthRegistryError::InvalidInput {
                message: "grant metadata must be a JSON object".to_owned(),
            });
        }
        validate_nonnegative_i64(self.created_at_ms, "created_at_ms")?;
        validate_nonnegative_i64(self.updated_at_ms, "updated_at_ms")?;
        if self.updated_at_ms < self.created_at_ms {
            return Err(AuthRegistryError::InvalidInput {
                message: format!(
                    "updated_at_ms {} must be >= created_at_ms {}",
                    self.updated_at_ms, self.created_at_ms
                ),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateAuthGrantRecord {
    pub grant_id: AuthGrantId,
    pub provider_id: String,
    pub provider_kind: AuthProviderKind,
    pub principal: PrincipalRef,
    pub display_name: Option<String>,
    pub subject_hint: Option<String>,
    pub scopes: Vec<String>,
    pub audience: Option<String>,
    pub access_token_secret: Option<SecretId>,
    pub refresh_token_secret: Option<SecretId>,
    pub oauth_client: Option<OAuthClientId>,
    pub expires_at_ms: Option<i64>,
    pub status: AuthGrantStatus,
    #[serde(default = "empty_metadata")]
    pub metadata: serde_json::Value,
    pub created_at_ms: i64,
}

impl CreateAuthGrantRecord {
    pub fn into_record(self) -> AuthGrantRecord {
        AuthGrantRecord {
            grant_id: self.grant_id,
            provider_id: self.provider_id,
            provider_kind: self.provider_kind,
            principal: self.principal,
            display_name: self.display_name,
            subject_hint: self.subject_hint,
            scopes: self.scopes,
            audience: self.audience,
            access_token_secret: self.access_token_secret,
            refresh_token_secret: self.refresh_token_secret,
            oauth_client: self.oauth_client,
            expires_at_ms: self.expires_at_ms,
            status: self.status,
            metadata: self.metadata,
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.created_at_ms,
        }
    }
}

/// Atomic pointer swap recorded after a successful token refresh. New secret
/// values are written under fresh ids first; this update then swaps the
/// grant's references in one step. `refresh_token_secret = None` keeps the
/// existing refresh token (no rotation).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthGrantTokenRefresh {
    pub access_token_secret: SecretId,
    pub refresh_token_secret: Option<SecretId>,
    pub expires_at_ms: Option<i64>,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListAuthGrants {
    pub status: Option<AuthGrantStatus>,
}

#[async_trait]
pub trait AuthGrantStore: Send + Sync {
    async fn create_grant(
        &self,
        record: CreateAuthGrantRecord,
    ) -> Result<AuthGrantRecord, AuthRegistryError>;

    async fn read_grant(
        &self,
        grant_id: &AuthGrantId,
    ) -> Result<AuthGrantRecord, AuthRegistryError>;

    async fn list_grants(
        &self,
        request: ListAuthGrants,
    ) -> Result<Vec<AuthGrantRecord>, AuthRegistryError>;

    async fn update_grant_status(
        &self,
        grant_id: &AuthGrantId,
        status: AuthGrantStatus,
        updated_at_ms: i64,
    ) -> Result<AuthGrantRecord, AuthRegistryError>;

    /// Swap the grant's token secret references and expiry after a refresh.
    async fn record_grant_refresh(
        &self,
        grant_id: &AuthGrantId,
        refresh: AuthGrantTokenRefresh,
    ) -> Result<AuthGrantRecord, AuthRegistryError>;

    async fn delete_grant(
        &self,
        grant_id: &AuthGrantId,
    ) -> Result<AuthGrantRecord, AuthRegistryError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn create_request(grant_id: &str) -> CreateAuthGrantRecord {
        CreateAuthGrantRecord {
            grant_id: AuthGrantId::new(grant_id),
            provider_id: "static".to_owned(),
            provider_kind: AuthProviderKind::StaticBearer,
            principal: PrincipalRef::universe_default(),
            display_name: Some("CRM token".to_owned()),
            subject_hint: None,
            scopes: vec!["contacts.read".to_owned()],
            audience: Some("https://crm.example.com/mcp".to_owned()),
            access_token_secret: Some(SecretId::new("authsec_1")),
            refresh_token_secret: None,
            oauth_client: None,
            expires_at_ms: None,
            status: AuthGrantStatus::Active,
            metadata: serde_json::Value::Object(Default::default()),
            created_at_ms: 10,
        }
    }

    #[test]
    fn grant_records_validate() {
        let record = create_request("authgrant_1").into_record();

        record.validate().expect("valid grant record");
    }

    #[test]
    fn grant_records_reject_credentialed_audience() {
        let mut record = create_request("authgrant_1").into_record();
        record.audience = Some("https://user:pw@crm.example.com/mcp".to_owned());

        let error = record
            .validate()
            .expect_err("audience credentials must be rejected");

        assert!(matches!(error, AuthRegistryError::InvalidInput { .. }));
    }

    #[test]
    fn grant_records_reject_duplicate_scopes() {
        let mut record = create_request("authgrant_1").into_record();
        record.scopes = vec!["a".to_owned(), "a".to_owned()];

        let error = record.validate().expect_err("duplicate scopes rejected");

        assert!(matches!(error, AuthRegistryError::InvalidInput { .. }));
    }

    #[test]
    fn principal_refs_validate_kind_id_pairing() {
        PrincipalRef::universe_default()
            .validate()
            .expect("universe default principal");

        let user_without_id = PrincipalRef {
            kind: PrincipalKind::User,
            id: None,
        };
        assert!(matches!(
            user_without_id.validate(),
            Err(AuthRegistryError::InvalidInput { .. })
        ));

        let default_with_id = PrincipalRef {
            kind: PrincipalKind::UniverseDefault,
            id: Some("u1".to_owned()),
        };
        assert!(matches!(
            default_with_id.validate(),
            Err(AuthRegistryError::InvalidInput { .. })
        ));
    }
}
