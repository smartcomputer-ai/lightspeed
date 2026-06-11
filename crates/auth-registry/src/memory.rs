use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use crate::{
    AuthFlowId, AuthFlowRecord, AuthFlowStore, AuthGrantId, AuthGrantRecord, AuthGrantStatus,
    AuthGrantStore, AuthGrantTokenRefresh, AuthProviderId, AuthProviderRecord, AuthProviderStore,
    AuthRegistryError, CreateAuthFlowRecord, CreateAuthGrantRecord, CreateAuthProviderRecord,
    CreateOAuthClientRecord, FinishAuthFlow, ListAuthGrants, OAuthClientId, OAuthClientRecord,
    OAuthClientStore, PutSecretRecord, SecretId, SecretRecordMeta, SecretStore, SecretValue,
};

#[derive(Clone, Default)]
pub struct InMemoryAuthGrantStore {
    inner: Arc<RwLock<BTreeMap<AuthGrantId, AuthGrantRecord>>>,
}

impl InMemoryAuthGrantStore {
    pub fn new() -> Self {
        Self::default()
    }
}

fn lock_poisoned() -> AuthRegistryError {
    AuthRegistryError::Store {
        message: "auth registry lock poisoned".to_owned(),
    }
}

#[async_trait]
impl AuthGrantStore for InMemoryAuthGrantStore {
    async fn create_grant(
        &self,
        record: CreateAuthGrantRecord,
    ) -> Result<AuthGrantRecord, AuthRegistryError> {
        let record = record.into_record();
        record.validate()?;
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        if inner.contains_key(&record.grant_id) {
            return Err(AuthRegistryError::GrantAlreadyExists {
                grant_id: record.grant_id,
            });
        }
        inner.insert(record.grant_id.clone(), record.clone());
        Ok(record)
    }

    async fn read_grant(
        &self,
        grant_id: &AuthGrantId,
    ) -> Result<AuthGrantRecord, AuthRegistryError> {
        let inner = self.inner.read().map_err(|_| lock_poisoned())?;
        inner
            .get(grant_id)
            .cloned()
            .ok_or_else(|| AuthRegistryError::GrantNotFound {
                grant_id: grant_id.clone(),
            })
    }

    async fn list_grants(
        &self,
        request: ListAuthGrants,
    ) -> Result<Vec<AuthGrantRecord>, AuthRegistryError> {
        let inner = self.inner.read().map_err(|_| lock_poisoned())?;
        Ok(inner
            .values()
            .filter(|record| request.status.is_none_or(|status| record.status == status))
            .cloned()
            .collect())
    }

    async fn update_grant_status(
        &self,
        grant_id: &AuthGrantId,
        status: AuthGrantStatus,
        updated_at_ms: i64,
    ) -> Result<AuthGrantRecord, AuthRegistryError> {
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        let record = inner
            .get_mut(grant_id)
            .ok_or_else(|| AuthRegistryError::GrantNotFound {
                grant_id: grant_id.clone(),
            })?;
        record.status = status;
        record.updated_at_ms = updated_at_ms;
        record.validate()?;
        Ok(record.clone())
    }

    async fn record_grant_refresh(
        &self,
        grant_id: &AuthGrantId,
        refresh: AuthGrantTokenRefresh,
    ) -> Result<AuthGrantRecord, AuthRegistryError> {
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        let record = inner
            .get_mut(grant_id)
            .ok_or_else(|| AuthRegistryError::GrantNotFound {
                grant_id: grant_id.clone(),
            })?;
        record.access_token_secret = Some(refresh.access_token_secret);
        if let Some(refresh_token_secret) = refresh.refresh_token_secret {
            record.refresh_token_secret = Some(refresh_token_secret);
        }
        record.expires_at_ms = refresh.expires_at_ms;
        record.updated_at_ms = refresh.updated_at_ms;
        record.validate()?;
        Ok(record.clone())
    }

    async fn delete_grant(
        &self,
        grant_id: &AuthGrantId,
    ) -> Result<AuthGrantRecord, AuthRegistryError> {
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        inner
            .remove(grant_id)
            .ok_or_else(|| AuthRegistryError::GrantNotFound {
                grant_id: grant_id.clone(),
            })
    }
}

#[derive(Clone, Default)]
pub struct InMemoryOAuthClientStore {
    inner: Arc<RwLock<BTreeMap<OAuthClientId, OAuthClientRecord>>>,
}

impl InMemoryOAuthClientStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl OAuthClientStore for InMemoryOAuthClientStore {
    async fn create_oauth_client(
        &self,
        record: CreateOAuthClientRecord,
    ) -> Result<OAuthClientRecord, AuthRegistryError> {
        let record = record.into_record();
        record.validate()?;
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        if inner.contains_key(&record.client_id) {
            return Err(AuthRegistryError::ClientAlreadyExists {
                client_id: record.client_id,
            });
        }
        inner.insert(record.client_id.clone(), record.clone());
        Ok(record)
    }

    async fn read_oauth_client(
        &self,
        client_id: &OAuthClientId,
    ) -> Result<OAuthClientRecord, AuthRegistryError> {
        let inner = self.inner.read().map_err(|_| lock_poisoned())?;
        inner
            .get(client_id)
            .cloned()
            .ok_or_else(|| AuthRegistryError::ClientNotFound {
                client_id: client_id.clone(),
            })
    }

    async fn list_oauth_clients(&self) -> Result<Vec<OAuthClientRecord>, AuthRegistryError> {
        let inner = self.inner.read().map_err(|_| lock_poisoned())?;
        Ok(inner.values().cloned().collect())
    }

    async fn delete_oauth_client(
        &self,
        client_id: &OAuthClientId,
    ) -> Result<OAuthClientRecord, AuthRegistryError> {
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        inner
            .remove(client_id)
            .ok_or_else(|| AuthRegistryError::ClientNotFound {
                client_id: client_id.clone(),
            })
    }
}

#[derive(Clone, Default)]
pub struct InMemoryAuthProviderStore {
    inner: Arc<RwLock<BTreeMap<AuthProviderId, AuthProviderRecord>>>,
}

impl InMemoryAuthProviderStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl AuthProviderStore for InMemoryAuthProviderStore {
    async fn create_auth_provider(
        &self,
        record: CreateAuthProviderRecord,
    ) -> Result<AuthProviderRecord, AuthRegistryError> {
        let record = record.into_record();
        record.validate()?;
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        if inner.contains_key(&record.provider_id) {
            return Err(AuthRegistryError::ProviderAlreadyExists {
                provider_id: record.provider_id,
            });
        }
        inner.insert(record.provider_id.clone(), record.clone());
        Ok(record)
    }

    async fn read_auth_provider(
        &self,
        provider_id: &AuthProviderId,
    ) -> Result<AuthProviderRecord, AuthRegistryError> {
        let inner = self.inner.read().map_err(|_| lock_poisoned())?;
        inner
            .get(provider_id)
            .cloned()
            .ok_or_else(|| AuthRegistryError::ProviderNotFound {
                provider_id: provider_id.clone(),
            })
    }

    async fn list_auth_providers(&self) -> Result<Vec<AuthProviderRecord>, AuthRegistryError> {
        let inner = self.inner.read().map_err(|_| lock_poisoned())?;
        Ok(inner.values().cloned().collect())
    }

    async fn delete_auth_provider(
        &self,
        provider_id: &AuthProviderId,
    ) -> Result<AuthProviderRecord, AuthRegistryError> {
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        inner
            .remove(provider_id)
            .ok_or_else(|| AuthRegistryError::ProviderNotFound {
                provider_id: provider_id.clone(),
            })
    }
}

#[derive(Clone, Default)]
pub struct InMemoryAuthFlowStore {
    inner: Arc<RwLock<BTreeMap<AuthFlowId, AuthFlowRecord>>>,
}

impl InMemoryAuthFlowStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl AuthFlowStore for InMemoryAuthFlowStore {
    async fn create_flow(
        &self,
        record: CreateAuthFlowRecord,
    ) -> Result<AuthFlowRecord, AuthRegistryError> {
        let record = record.into_record();
        record.validate()?;
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        if inner.contains_key(&record.flow_id) {
            return Err(AuthRegistryError::FlowAlreadyExists {
                flow_id: record.flow_id,
            });
        }
        if inner
            .values()
            .any(|existing| existing.state_hash == record.state_hash)
        {
            return Err(AuthRegistryError::Store {
                message: "auth flow state hash collision".to_owned(),
            });
        }
        inner.insert(record.flow_id.clone(), record.clone());
        Ok(record)
    }

    async fn read_flow(&self, flow_id: &AuthFlowId) -> Result<AuthFlowRecord, AuthRegistryError> {
        let inner = self.inner.read().map_err(|_| lock_poisoned())?;
        inner
            .get(flow_id)
            .cloned()
            .ok_or_else(|| AuthRegistryError::FlowNotFound {
                flow_id: flow_id.clone(),
            })
    }

    async fn read_flow_by_state_hash(
        &self,
        state_hash: &str,
    ) -> Result<Option<AuthFlowRecord>, AuthRegistryError> {
        let inner = self.inner.read().map_err(|_| lock_poisoned())?;
        Ok(inner
            .values()
            .find(|record| record.state_hash == state_hash)
            .cloned())
    }

    async fn consume_flow(
        &self,
        flow_id: &AuthFlowId,
        now_ms: i64,
    ) -> Result<AuthFlowRecord, AuthRegistryError> {
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        let record = inner
            .get_mut(flow_id)
            .ok_or_else(|| AuthRegistryError::FlowNotFound {
                flow_id: flow_id.clone(),
            })?;
        if record.consumed_at_ms.is_some() {
            return Err(AuthRegistryError::FlowAlreadyConsumed {
                flow_id: flow_id.clone(),
            });
        }
        if now_ms >= record.expires_at_ms {
            return Err(AuthRegistryError::FlowExpired {
                flow_id: flow_id.clone(),
            });
        }
        record.consumed_at_ms = Some(now_ms);
        record.updated_at_ms = now_ms;
        Ok(record.clone())
    }

    async fn finish_flow(
        &self,
        flow_id: &AuthFlowId,
        outcome: FinishAuthFlow,
    ) -> Result<AuthFlowRecord, AuthRegistryError> {
        outcome.validate()?;
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        let record = inner
            .get_mut(flow_id)
            .ok_or_else(|| AuthRegistryError::FlowNotFound {
                flow_id: flow_id.clone(),
            })?;
        if record.completed_at_ms.is_some() {
            return Err(AuthRegistryError::FlowAlreadyCompleted {
                flow_id: flow_id.clone(),
            });
        }
        record.grant_id = outcome.grant_id;
        record.error = outcome.error;
        record.completed_at_ms = Some(outcome.completed_at_ms);
        record.updated_at_ms = outcome.completed_at_ms;
        record.validate()?;
        Ok(record.clone())
    }
}

/// In-memory secret store for tests. Values are held in plaintext in memory;
/// this adapter must never back a production deployment.
#[derive(Clone, Default)]
pub struct InMemorySecretStore {
    inner: Arc<RwLock<BTreeMap<SecretId, (SecretRecordMeta, SecretValue)>>>,
}

impl InMemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SecretStore for InMemorySecretStore {
    async fn put_secret(
        &self,
        record: PutSecretRecord,
    ) -> Result<SecretRecordMeta, AuthRegistryError> {
        record.validate()?;
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        if inner.contains_key(&record.secret_id) {
            return Err(AuthRegistryError::SecretAlreadyExists {
                secret_id: record.secret_id,
            });
        }
        let meta = SecretRecordMeta {
            secret_id: record.secret_id.clone(),
            secret_kind: record.secret_kind,
            created_at_ms: record.created_at_ms,
            updated_at_ms: record.created_at_ms,
        };
        inner.insert(record.secret_id, (meta.clone(), record.value));
        Ok(meta)
    }

    async fn read_secret(
        &self,
        secret_id: &SecretId,
    ) -> Result<(SecretRecordMeta, SecretValue), AuthRegistryError> {
        let inner = self.inner.read().map_err(|_| lock_poisoned())?;
        inner
            .get(secret_id)
            .cloned()
            .ok_or_else(|| AuthRegistryError::SecretNotFound {
                secret_id: secret_id.clone(),
            })
    }

    async fn delete_secret(&self, secret_id: &SecretId) -> Result<(), AuthRegistryError> {
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        inner
            .remove(secret_id)
            .map(|_| ())
            .ok_or_else(|| AuthRegistryError::SecretNotFound {
                secret_id: secret_id.clone(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AuthProviderKind, PrincipalRef, SECRET_KIND_STATIC_BEARER};

    fn grant_request(grant_id: &str) -> CreateAuthGrantRecord {
        CreateAuthGrantRecord {
            grant_id: AuthGrantId::new(grant_id),
            provider_id: "static".to_owned(),
            provider_kind: AuthProviderKind::StaticBearer,
            principal: PrincipalRef::universe_default(),
            display_name: None,
            subject_hint: None,
            scopes: Vec::new(),
            audience: None,
            access_token_secret: Some(SecretId::new("authsec_1")),
            refresh_token_secret: None,
            oauth_client: None,
            expires_at_ms: None,
            status: AuthGrantStatus::Active,
            metadata: serde_json::Value::Object(Default::default()),
            created_at_ms: 10,
        }
    }

    #[tokio::test]
    async fn in_memory_grant_store_supports_crud_and_status_updates() {
        let store = InMemoryAuthGrantStore::new();

        let created = store
            .create_grant(grant_request("authgrant_1"))
            .await
            .expect("create grant");
        assert_eq!(created.updated_at_ms, created.created_at_ms);

        assert!(matches!(
            store.create_grant(grant_request("authgrant_1")).await,
            Err(AuthRegistryError::GrantAlreadyExists { .. })
        ));

        let listed = store
            .list_grants(ListAuthGrants::default())
            .await
            .expect("list grants");
        assert_eq!(listed, vec![created.clone()]);

        let revoked = store
            .update_grant_status(&created.grant_id, AuthGrantStatus::Revoked, 20)
            .await
            .expect("revoke grant");
        assert_eq!(revoked.status, AuthGrantStatus::Revoked);
        assert_eq!(revoked.updated_at_ms, 20);

        let active = store
            .list_grants(ListAuthGrants {
                status: Some(AuthGrantStatus::Active),
            })
            .await
            .expect("list active grants");
        assert!(active.is_empty());

        store
            .delete_grant(&created.grant_id)
            .await
            .expect("delete grant");
        assert!(matches!(
            store.read_grant(&created.grant_id).await,
            Err(AuthRegistryError::GrantNotFound { .. })
        ));
    }

    #[tokio::test]
    async fn in_memory_secret_store_round_trips_values() {
        let store = InMemorySecretStore::new();

        let meta = store
            .put_secret(PutSecretRecord {
                secret_id: SecretId::new("authsec_1"),
                secret_kind: SECRET_KIND_STATIC_BEARER.to_owned(),
                value: SecretValue::new("token-123"),
                created_at_ms: 10,
            })
            .await
            .expect("put secret");
        assert_eq!(meta.secret_kind, SECRET_KIND_STATIC_BEARER);

        assert!(matches!(
            store
                .put_secret(PutSecretRecord {
                    secret_id: SecretId::new("authsec_1"),
                    secret_kind: SECRET_KIND_STATIC_BEARER.to_owned(),
                    value: SecretValue::new("token-456"),
                    created_at_ms: 11,
                })
                .await,
            Err(AuthRegistryError::SecretAlreadyExists { .. })
        ));

        let (read_meta, value) = store
            .read_secret(&SecretId::new("authsec_1"))
            .await
            .expect("read secret");
        assert_eq!(read_meta, meta);
        assert_eq!(value.expose(), "token-123");

        store
            .delete_secret(&SecretId::new("authsec_1"))
            .await
            .expect("delete secret");
        assert!(matches!(
            store.read_secret(&SecretId::new("authsec_1")).await,
            Err(AuthRegistryError::SecretNotFound { .. })
        ));
    }
}
