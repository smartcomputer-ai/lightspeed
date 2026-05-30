use engine::{BlobRef, storage::BlobStoreError};
use object_store::{ObjectStoreExt, PutPayload, path::Path as ObjectPath};

use crate::{
    PgStore, PgStoreConfig,
    shared::{object_store_error, sha256_hex},
};

impl PgStore {
    pub(crate) async fn put_object(
        &self,
        key: &str,
        bytes: Vec<u8>,
    ) -> Result<object_store::PutResult, BlobStoreError> {
        let object_store = self
            .object_store
            .as_ref()
            .ok_or_else(|| BlobStoreError::Store {
                message: format!(
                    "blob exceeds inline threshold ({} bytes) but no object store is configured",
                    self.config.inline_threshold_bytes
                ),
            })?;
        object_store
            .put(&ObjectPath::from(key), PutPayload::from(bytes))
            .await
            .map_err(|error| object_store_error("put object", key, error))
    }

    pub(crate) async fn get_object(
        &self,
        key: &str,
        blob_ref: &BlobRef,
    ) -> Result<Vec<u8>, BlobStoreError> {
        let object_store = self
            .object_store
            .as_ref()
            .ok_or_else(|| BlobStoreError::Store {
                message: format!(
                    "blob '{blob_ref}' is object-backed but no object store is configured"
                ),
            })?;
        match object_store.get(&ObjectPath::from(key)).await {
            Ok(result) => result
                .bytes()
                .await
                .map(|bytes| bytes.to_vec())
                .map_err(|error| object_store_error("read object body", key, error)),
            Err(object_store::Error::NotFound { .. }) => Err(BlobStoreError::NotFound {
                blob_ref: blob_ref.clone(),
            }),
            Err(error) => Err(object_store_error("get object", key, error)),
        }
    }
}

pub(crate) fn direct_blob_key(
    config: &PgStoreConfig,
    blob_ref: &BlobRef,
) -> Result<String, BlobStoreError> {
    let digest = sha256_hex(blob_ref)?;
    let prefix = &digest[..2];
    Ok(prefixed_key(
        config,
        &format!(
            "universes/{}/cas/blobs/sha256/{prefix}/{digest}.bin",
            config.universe_id
        ),
    ))
}

fn prefixed_key(config: &PgStoreConfig, suffix: &str) -> String {
    let prefix = config.object_prefix.trim_matches('/');
    if prefix.is_empty() {
        suffix.to_owned()
    } else {
        format!("{prefix}/{suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PgStoreConfig;
    use uuid::Uuid;

    #[test]
    fn direct_blob_keys_are_scoped_by_universe() {
        let config = PgStoreConfig::new(Uuid::new_v4())
            .with_inline_threshold_bytes(8)
            .with_object_prefix("prefix");
        let blob_ref = BlobRef::from_bytes(b"hello");
        let key = direct_blob_key(&config, &blob_ref).expect("blob key");

        assert!(key.starts_with(&format!(
            "prefix/universes/{}/cas/blobs/sha256/",
            config.universe_id
        )));
        assert!(key.ends_with(".bin"));
    }
}
