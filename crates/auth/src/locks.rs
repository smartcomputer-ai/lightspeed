//! Per-grant token renewal serialization (P69 G3).
//!
//! Renewal (OAuth refresh, on-demand minting) must be single-flight per
//! grant: with refresh-token rotation, a double refresh is destructive
//! because several authorization servers treat refresh-token reuse as theft
//! and revoke the whole grant chain. Adapters implement [`GrantRefreshLock`];
//! `store-pg` uses a Postgres advisory lock so the guarantee holds across
//! processes. The broker acquires the lock around every renewal.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::{AuthGrantId, AuthRegistryError};

/// Held for the duration of a refresh; dropping it releases the lock.
pub struct GrantLockGuard {
    _guard: Box<dyn std::any::Any + Send>,
}

impl GrantLockGuard {
    pub fn new(guard: impl std::any::Any + Send) -> Self {
        Self {
            _guard: Box::new(guard),
        }
    }
}

#[async_trait]
pub trait GrantRefreshLock: Send + Sync {
    /// Acquire the exclusive refresh lock for `grant_id`, waiting until any
    /// concurrent holder releases it.
    async fn lock_grant(&self, grant_id: &AuthGrantId)
    -> Result<GrantLockGuard, AuthRegistryError>;
}

/// Process-local lock manager for tests and the in-memory stores. Production
/// deployments with multiple workers need a store-backed lock instead.
#[derive(Default)]
pub struct InMemoryGrantLocks {
    locks: Mutex<BTreeMap<AuthGrantId, Arc<tokio::sync::Mutex<()>>>>,
}

impl InMemoryGrantLocks {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl GrantRefreshLock for InMemoryGrantLocks {
    async fn lock_grant(
        &self,
        grant_id: &AuthGrantId,
    ) -> Result<GrantLockGuard, AuthRegistryError> {
        let lock = {
            let mut locks = self.locks.lock().map_err(|_| AuthRegistryError::Store {
                message: "grant lock table poisoned".to_owned(),
            })?;
            locks.entry(grant_id.clone()).or_default().clone()
        };
        let guard = lock.lock_owned().await;
        Ok(GrantLockGuard::new(guard))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_locks_serialize_same_grant() {
        let locks = Arc::new(InMemoryGrantLocks::new());
        let grant_id = AuthGrantId::new("authgrant_1");

        let guard = locks.lock_grant(&grant_id).await.expect("first lock");

        let locks_clone = locks.clone();
        let grant_clone = grant_id.clone();
        let contender =
            tokio::spawn(async move { locks_clone.lock_grant(&grant_clone).await.map(|_| ()) });
        // The contender cannot finish while the guard is held.
        tokio::task::yield_now().await;
        assert!(!contender.is_finished());

        drop(guard);
        contender
            .await
            .expect("join contender")
            .expect("second lock");
    }

    #[tokio::test]
    async fn in_memory_locks_do_not_block_distinct_grants() {
        let locks = InMemoryGrantLocks::new();

        let _first = locks
            .lock_grant(&AuthGrantId::new("authgrant_1"))
            .await
            .expect("first grant lock");
        let _second = locks
            .lock_grant(&AuthGrantId::new("authgrant_2"))
            .await
            .expect("second grant lock");
    }
}
