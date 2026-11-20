use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use crate::{
    CreateCredentialFlowRecord, CreateCredentialGrantRecord, CreateCredentialProviderRecord,
    CreateOAuthClientRecord, CredentialFlowId, CredentialFlowRecord, CredentialFlowStore,
    CredentialGrantId, CredentialGrantRecord, CredentialGrantStatus, CredentialGrantStore,
    CredentialGrantTokenRefresh, CredentialProviderId, CredentialProviderRecord,
    CredentialProviderStore, CredentialRegistryError, FinishCredentialFlow, ListCredentialGrants,
    OAuthClientId, OAuthClientRecord, OAuthClientStore, PutSecretRecord, SecretId,
    SecretRecordMeta, SecretStore, SecretValue, TokenCredentialUpdate,
};

#[derive(Clone, Default)]
pub struct InMemoryCredentialGrantStore {
    inner: Arc<RwLock<BTreeMap<CredentialGrantId, CredentialGrantRecord>>>,
}

impl InMemoryCredentialGrantStore {
    pub fn new() -> Self {
        Self::default()
    }
}

fn lock_poisoned() -> CredentialRegistryError {
    CredentialRegistryError::Store {
        message: "credential registry lock poisoned".to_owned(),
    }
}

#[async_trait]
impl CredentialGrantStore for InMemoryCredentialGrantStore {
    async fn create_grant(
        &self,
        record: CreateCredentialGrantRecord,
    ) -> Result<CredentialGrantRecord, CredentialRegistryError> {
        let record = record.into_record();
        record.validate()?;
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        if inner.contains_key(&record.grant_id) {
            return Err(CredentialRegistryError::GrantAlreadyExists {
                grant_id: record.grant_id,
            });
        }
        inner.insert(record.grant_id.clone(), record.clone());
        Ok(record)
    }

    async fn read_grant(
        &self,
        grant_id: &CredentialGrantId,
    ) -> Result<CredentialGrantRecord, CredentialRegistryError> {
        let inner = self.inner.read().map_err(|_| lock_poisoned())?;
        inner
            .get(grant_id)
            .cloned()
            .ok_or_else(|| CredentialRegistryError::GrantNotFound {
                grant_id: grant_id.clone(),
            })
    }

    async fn list_grants(
        &self,
        request: ListCredentialGrants,
    ) -> Result<Vec<CredentialGrantRecord>, CredentialRegistryError> {
        let inner = self.inner.read().map_err(|_| lock_poisoned())?;
        Ok(inner
            .values()
            .filter(|record| request.status.is_none_or(|status| record.status == status))
            .cloned()
            .collect())
    }

    async fn update_grant_status(
        &self,
        grant_id: &CredentialGrantId,
        status: CredentialGrantStatus,
        updated_at_ms: i64,
    ) -> Result<CredentialGrantRecord, CredentialRegistryError> {
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        let record =
            inner
                .get_mut(grant_id)
                .ok_or_else(|| CredentialRegistryError::GrantNotFound {
                    grant_id: grant_id.clone(),
                })?;
        record.status = status;
        record.updated_at_ms = updated_at_ms;
        record.validate()?;
        Ok(record.clone())
    }

    async fn record_grant_refresh(
        &self,
        grant_id: &CredentialGrantId,
        refresh: CredentialGrantTokenRefresh,
    ) -> Result<CredentialGrantRecord, CredentialRegistryError> {
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        let record =
            inner
                .get_mut(grant_id)
                .ok_or_else(|| CredentialRegistryError::GrantNotFound {
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

    async fn update_token_credential(
        &self,
        grant_id: &CredentialGrantId,
        expected: &SecretId,
        update: TokenCredentialUpdate,
    ) -> Result<Option<CredentialGrantRecord>, CredentialRegistryError> {
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        let record =
            inner
                .get_mut(grant_id)
                .ok_or_else(|| CredentialRegistryError::GrantNotFound {
                    grant_id: grant_id.clone(),
                })?;
        if record.access_token_secret.as_ref() != Some(expected) {
            return Ok(None);
        }
        let next = update.apply(record);
        next.validate()?;
        *record = next.clone();
        Ok(Some(next))
    }

    async fn record_grant_lease(
        &self,
        grant_id: &CredentialGrantId,
        leased_at_ms: i64,
    ) -> Result<CredentialGrantRecord, CredentialRegistryError> {
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        let record =
            inner
                .get_mut(grant_id)
                .ok_or_else(|| CredentialRegistryError::GrantNotFound {
                    grant_id: grant_id.clone(),
                })?;
        record.last_leased_at_ms = Some(leased_at_ms);
        record.lease_count = record.lease_count.saturating_add(1);
        record.validate()?;
        Ok(record.clone())
    }

    async fn record_grant_mint_expiry(
        &self,
        grant_id: &CredentialGrantId,
        expires_at_ms: i64,
        updated_at_ms: i64,
    ) -> Result<CredentialGrantRecord, CredentialRegistryError> {
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        let record =
            inner
                .get_mut(grant_id)
                .ok_or_else(|| CredentialRegistryError::GrantNotFound {
                    grant_id: grant_id.clone(),
                })?;
        record.expires_at_ms = Some(expires_at_ms);
        record.updated_at_ms = updated_at_ms;
        record.validate()?;
        Ok(record.clone())
    }

    async fn delete_grant(
        &self,
        grant_id: &CredentialGrantId,
    ) -> Result<CredentialGrantRecord, CredentialRegistryError> {
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        inner
            .remove(grant_id)
            .ok_or_else(|| CredentialRegistryError::GrantNotFound {
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
    ) -> Result<OAuthClientRecord, CredentialRegistryError> {
        let record = record.into_record();
        record.validate()?;
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        if inner.contains_key(&record.client_id) {
            return Err(CredentialRegistryError::ClientAlreadyExists {
                client_id: record.client_id,
            });
        }
        inner.insert(record.client_id.clone(), record.clone());
        Ok(record)
    }

    async fn read_oauth_client(
        &self,
        client_id: &OAuthClientId,
    ) -> Result<OAuthClientRecord, CredentialRegistryError> {
        let inner = self.inner.read().map_err(|_| lock_poisoned())?;
        inner
            .get(client_id)
            .cloned()
            .ok_or_else(|| CredentialRegistryError::ClientNotFound {
                client_id: client_id.clone(),
            })
    }

    async fn list_oauth_clients(&self) -> Result<Vec<OAuthClientRecord>, CredentialRegistryError> {
        let inner = self.inner.read().map_err(|_| lock_poisoned())?;
        Ok(inner.values().cloned().collect())
    }

    async fn delete_oauth_client(
        &self,
        client_id: &OAuthClientId,
    ) -> Result<OAuthClientRecord, CredentialRegistryError> {
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        inner
            .remove(client_id)
            .ok_or_else(|| CredentialRegistryError::ClientNotFound {
                client_id: client_id.clone(),
            })
    }
}

#[derive(Clone, Default)]
pub struct InMemoryCredentialProviderStore {
    inner: Arc<RwLock<BTreeMap<CredentialProviderId, CredentialProviderRecord>>>,
}

impl InMemoryCredentialProviderStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl CredentialProviderStore for InMemoryCredentialProviderStore {
    async fn create_credential_provider(
        &self,
        record: CreateCredentialProviderRecord,
    ) -> Result<CredentialProviderRecord, CredentialRegistryError> {
        let record = record.into_record();
        record.validate()?;
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        if inner.contains_key(&record.provider_id) {
            return Err(CredentialRegistryError::ProviderAlreadyExists {
                provider_id: record.provider_id,
            });
        }
        inner.insert(record.provider_id.clone(), record.clone());
        Ok(record)
    }

    async fn read_credential_provider(
        &self,
        provider_id: &CredentialProviderId,
    ) -> Result<CredentialProviderRecord, CredentialRegistryError> {
        let inner = self.inner.read().map_err(|_| lock_poisoned())?;
        inner
            .get(provider_id)
            .cloned()
            .ok_or_else(|| CredentialRegistryError::ProviderNotFound {
                provider_id: provider_id.clone(),
            })
    }

    async fn list_credential_providers(
        &self,
    ) -> Result<Vec<CredentialProviderRecord>, CredentialRegistryError> {
        let inner = self.inner.read().map_err(|_| lock_poisoned())?;
        Ok(inner.values().cloned().collect())
    }

    async fn delete_credential_provider(
        &self,
        provider_id: &CredentialProviderId,
    ) -> Result<CredentialProviderRecord, CredentialRegistryError> {
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        inner
            .remove(provider_id)
            .ok_or_else(|| CredentialRegistryError::ProviderNotFound {
                provider_id: provider_id.clone(),
            })
    }

    async fn replace_credential_provider(
        &self,
        record: CredentialProviderRecord,
        expected_credential: Option<&SecretId>,
    ) -> Result<Option<CredentialProviderRecord>, CredentialRegistryError> {
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        let current = inner.get_mut(&record.provider_id).ok_or_else(|| {
            CredentialRegistryError::ProviderNotFound {
                provider_id: record.provider_id.clone(),
            }
        })?;
        if current.credential_secret.as_ref() != expected_credential {
            return Ok(None);
        }
        let next = CredentialProviderRecord {
            created_at_ms: current.created_at_ms,
            ..record
        };
        next.validate()?;
        *current = next.clone();
        Ok(Some(next))
    }
}

#[derive(Clone, Default)]
pub struct InMemoryCredentialFlowStore {
    inner: Arc<RwLock<BTreeMap<CredentialFlowId, CredentialFlowRecord>>>,
}

impl InMemoryCredentialFlowStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl CredentialFlowStore for InMemoryCredentialFlowStore {
    async fn create_flow(
        &self,
        record: CreateCredentialFlowRecord,
    ) -> Result<CredentialFlowRecord, CredentialRegistryError> {
        let record = record.into_record();
        record.validate()?;
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        if inner.contains_key(&record.flow_id) {
            return Err(CredentialRegistryError::FlowAlreadyExists {
                flow_id: record.flow_id,
            });
        }
        if inner
            .values()
            .any(|existing| existing.state_hash == record.state_hash)
        {
            return Err(CredentialRegistryError::Store {
                message: "credential flow state hash collision".to_owned(),
            });
        }
        inner.insert(record.flow_id.clone(), record.clone());
        Ok(record)
    }

    async fn read_flow(
        &self,
        flow_id: &CredentialFlowId,
    ) -> Result<CredentialFlowRecord, CredentialRegistryError> {
        let inner = self.inner.read().map_err(|_| lock_poisoned())?;
        inner
            .get(flow_id)
            .cloned()
            .ok_or_else(|| CredentialRegistryError::FlowNotFound {
                flow_id: flow_id.clone(),
            })
    }

    async fn read_flow_by_state_hash(
        &self,
        state_hash: &str,
    ) -> Result<Option<CredentialFlowRecord>, CredentialRegistryError> {
        let inner = self.inner.read().map_err(|_| lock_poisoned())?;
        Ok(inner
            .values()
            .find(|record| record.state_hash == state_hash)
            .cloned())
    }

    async fn consume_flow(
        &self,
        flow_id: &CredentialFlowId,
        now_ms: i64,
    ) -> Result<CredentialFlowRecord, CredentialRegistryError> {
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        let record =
            inner
                .get_mut(flow_id)
                .ok_or_else(|| CredentialRegistryError::FlowNotFound {
                    flow_id: flow_id.clone(),
                })?;
        if record.consumed_at_ms.is_some() {
            return Err(CredentialRegistryError::FlowAlreadyConsumed {
                flow_id: flow_id.clone(),
            });
        }
        if now_ms >= record.expires_at_ms {
            return Err(CredentialRegistryError::FlowExpired {
                flow_id: flow_id.clone(),
            });
        }
        record.consumed_at_ms = Some(now_ms);
        record.updated_at_ms = now_ms;
        Ok(record.clone())
    }

    async fn finish_flow(
        &self,
        flow_id: &CredentialFlowId,
        outcome: FinishCredentialFlow,
    ) -> Result<CredentialFlowRecord, CredentialRegistryError> {
        outcome.validate()?;
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        let record =
            inner
                .get_mut(flow_id)
                .ok_or_else(|| CredentialRegistryError::FlowNotFound {
                    flow_id: flow_id.clone(),
                })?;
        if record.completed_at_ms.is_some() {
            return Err(CredentialRegistryError::FlowAlreadyCompleted {
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
    ) -> Result<SecretRecordMeta, CredentialRegistryError> {
        record.validate()?;
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        if inner.contains_key(&record.secret_id) {
            return Err(CredentialRegistryError::SecretAlreadyExists {
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
    ) -> Result<(SecretRecordMeta, SecretValue), CredentialRegistryError> {
        let inner = self.inner.read().map_err(|_| lock_poisoned())?;
        inner
            .get(secret_id)
            .cloned()
            .ok_or_else(|| CredentialRegistryError::SecretNotFound {
                secret_id: secret_id.clone(),
            })
    }

    async fn delete_secret(&self, secret_id: &SecretId) -> Result<(), CredentialRegistryError> {
        let mut inner = self.inner.write().map_err(|_| lock_poisoned())?;
        inner
            .remove(secret_id)
            .map(|_| ())
            .ok_or_else(|| CredentialRegistryError::SecretNotFound {
                secret_id: secret_id.clone(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CredentialProviderKind, SECRET_KIND_STATIC_TOKEN};

    fn grant_request(grant_id: &str) -> CreateCredentialGrantRecord {
        CreateCredentialGrantRecord {
            grant_id: CredentialGrantId::new(grant_id),
            provider_id: "static".to_owned(),
            provider_kind: CredentialProviderKind::StaticToken,
            exposure: crate::CredentialGrantExposure::Brokered,
            created_by: api::Attribution::Local,
            display_name: None,
            subject_hint: None,
            scopes: Vec::new(),
            audience: None,
            access_token_secret: Some(SecretId::new("authsec_1")),
            refresh_token_secret: None,
            oauth_client: None,
            expires_at_ms: None,
            status: CredentialGrantStatus::Active,
            metadata: serde_json::Value::Object(Default::default()),
            created_at_ms: 10,
        }
    }

    #[tokio::test]
    async fn in_memory_grant_store_supports_crud_and_status_updates() {
        let store = InMemoryCredentialGrantStore::new();

        let created = store
            .create_grant(grant_request("authgrant_1"))
            .await
            .expect("create grant");
        assert_eq!(created.updated_at_ms, created.created_at_ms);

        assert!(matches!(
            store.create_grant(grant_request("authgrant_1")).await,
            Err(CredentialRegistryError::GrantAlreadyExists { .. })
        ));

        let listed = store
            .list_grants(ListCredentialGrants::default())
            .await
            .expect("list grants");
        assert_eq!(listed, vec![created.clone()]);

        let first_lease = store
            .record_grant_lease(&created.grant_id, 15)
            .await
            .expect("record first lease");
        assert_eq!(first_lease.last_leased_at_ms, Some(15));
        assert_eq!(first_lease.lease_count, 1);
        let second_lease = store
            .record_grant_lease(&created.grant_id, 16)
            .await
            .expect("record second lease");
        assert_eq!(second_lease.last_leased_at_ms, Some(16));
        assert_eq!(second_lease.lease_count, 2);

        let revoked = store
            .update_grant_status(&created.grant_id, CredentialGrantStatus::Revoked, 20)
            .await
            .expect("revoke grant");
        assert_eq!(revoked.status, CredentialGrantStatus::Revoked);
        assert_eq!(revoked.updated_at_ms, 20);

        let active = store
            .list_grants(ListCredentialGrants {
                status: Some(CredentialGrantStatus::Active),
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
            Err(CredentialRegistryError::GrantNotFound { .. })
        ));
    }

    #[tokio::test]
    async fn in_memory_token_credential_update_writes_only_over_the_expected_value() {
        let store = InMemoryCredentialGrantStore::new();
        let grant_id = CredentialGrantId::new("authgrant_1");
        store
            .create_grant(grant_request("authgrant_1"))
            .await
            .expect("create grant");
        let replacement = |secret: &str| TokenCredentialUpdate {
            access_token_secret: SecretId::new(secret),
            display_name: Some("Renamed".into()),
            subject_hint: None,
            scopes: vec!["read".into()],
            audience: Some("https://api.example.com".into()),
            metadata: serde_json::json!({"owner": "ops"}),
            expires_at_ms: Some(50),
            updated_at_ms: 20,
        };

        let swapped = store
            .update_token_credential(
                &grant_id,
                &SecretId::new("authsec_1"),
                replacement("authsec_2"),
            )
            .await
            .expect("update")
            .expect("expected value matched");
        assert_eq!(
            swapped.access_token_secret,
            Some(SecretId::new("authsec_2"))
        );
        assert_eq!(swapped.expires_at_ms, Some(50));
        assert_eq!(swapped.display_name.as_deref(), Some("Renamed"));
        assert_eq!(swapped.scopes, ["read"]);
        assert_eq!(swapped.metadata, serde_json::json!({"owner": "ops"}));

        // Settings alone change under the value the credential already holds.
        let mut settings_only = replacement("authsec_2");
        settings_only.display_name = None;
        let kept = store
            .update_token_credential(&grant_id, &SecretId::new("authsec_2"), settings_only)
            .await
            .expect("update")
            .expect("expected value matched");
        assert_eq!(kept.access_token_secret, Some(SecretId::new("authsec_2")));
        assert_eq!(kept.display_name, None);

        // A writer that read the old value loses and changes nothing.
        let stale = store
            .update_token_credential(
                &grant_id,
                &SecretId::new("authsec_1"),
                replacement("authsec_3"),
            )
            .await
            .expect("update");
        assert!(stale.is_none());
        assert_eq!(
            store
                .read_grant(&grant_id)
                .await
                .expect("read")
                .access_token_secret,
            Some(SecretId::new("authsec_2"))
        );

        assert!(matches!(
            store
                .update_token_credential(
                    &CredentialGrantId::new("authgrant_missing"),
                    &SecretId::new("authsec_1"),
                    replacement("authsec_4"),
                )
                .await,
            Err(CredentialRegistryError::GrantNotFound { .. })
        ));
    }

    #[tokio::test]
    async fn in_memory_provider_replacement_swaps_only_from_the_expected_credential() {
        use crate::{
            CredentialProviderConfig, CredentialProviderId, CredentialProviderStatus,
            ModelApiKeyConfig,
        };

        let store = InMemoryCredentialProviderStore::new();
        let provider = |secret: &str, at: i64| CreateCredentialProviderRecord {
            provider_id: CredentialProviderId::new("model:openai"),
            display_name: None,
            config: CredentialProviderConfig::ModelApiKey(ModelApiKeyConfig::default()),
            credential_secret: Some(SecretId::new(secret)),
            status: CredentialProviderStatus::Active,
            created_at_ms: at,
        };
        store
            .create_credential_provider(provider("authsec_1", 10))
            .await
            .expect("create provider");

        let replaced = store
            .replace_credential_provider(
                provider("authsec_2", 20).into_record(),
                Some(&SecretId::new("authsec_1")),
            )
            .await
            .expect("replace")
            .expect("expected credential matched");
        assert_eq!(replaced.credential_secret, Some(SecretId::new("authsec_2")));
        assert_eq!(replaced.created_at_ms, 10, "creation time is kept");

        // A writer that read the old credential loses and changes nothing.
        assert!(
            store
                .replace_credential_provider(
                    provider("authsec_3", 30).into_record(),
                    Some(&SecretId::new("authsec_1")),
                )
                .await
                .expect("replace")
                .is_none()
        );
        assert_eq!(
            store
                .read_credential_provider(&CredentialProviderId::new("model:openai"))
                .await
                .expect("read")
                .credential_secret,
            Some(SecretId::new("authsec_2"))
        );
    }

    #[tokio::test]
    async fn in_memory_secret_store_round_trips_values() {
        let store = InMemorySecretStore::new();

        let meta = store
            .put_secret(PutSecretRecord {
                secret_id: SecretId::new("authsec_1"),
                secret_kind: SECRET_KIND_STATIC_TOKEN.to_owned(),
                value: SecretValue::new("token-123"),
                created_at_ms: 10,
            })
            .await
            .expect("put secret");
        assert_eq!(meta.secret_kind, SECRET_KIND_STATIC_TOKEN);

        assert!(matches!(
            store
                .put_secret(PutSecretRecord {
                    secret_id: SecretId::new("authsec_1"),
                    secret_kind: SECRET_KIND_STATIC_TOKEN.to_owned(),
                    value: SecretValue::new("token-456"),
                    created_at_ms: 11,
                })
                .await,
            Err(CredentialRegistryError::SecretAlreadyExists { .. })
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
            Err(CredentialRegistryError::SecretNotFound { .. })
        ));
    }
}
