use futures_util::TryStreamExt as _;
use harness::{BlobRef, storage::BlobStoreError};
use object_store::{ObjectStore, ObjectStoreExt, PutPayload, path::Path as ObjectPath};
use uuid::Uuid;

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
            "universes/{}/cas/blobs/sha256/{prefix}/{digest}/{}.bin",
            config.universe_id,
            Uuid::new_v4().simple()
        ),
    ))
}

fn prefixed_key(config: &PgStoreConfig, suffix: &str) -> String {
    prefix_key(&config.object_prefix, suffix)
}

fn prefix_key(object_prefix: &str, suffix: &str) -> String {
    let prefix = object_prefix.trim_matches('/');
    if prefix.is_empty() {
        suffix.to_owned()
    } else {
        format!("{prefix}/{suffix}")
    }
}

/// Object-store prefix under which every CAS object of one universe lives,
/// including the deployment's configured object prefix. Universe deletion
/// clears it wholesale so objects whose rows were already swept, or whose
/// deletion failed after the row went, do not outlive the universe.
pub fn universe_cas_object_prefix(object_prefix: &str, universe_id: Uuid) -> String {
    prefix_key(object_prefix, &format!("universes/{universe_id}/cas/"))
}

/// Delete every object under `prefix`; returns how many were removed.
pub async fn delete_objects_under_prefix(
    object_store: &dyn ObjectStore,
    prefix: &str,
) -> Result<u64, object_store::Error> {
    let prefix = ObjectPath::from(prefix.trim_end_matches('/'));
    let mut listing = object_store.list(Some(&prefix));
    let mut deleted = 0u64;
    while let Some(meta) = listing.try_next().await? {
        match object_store.delete(&meta.location).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => deleted += 1,
            Err(error) => return Err(error),
        }
    }
    Ok(deleted)
}

/// Whole-file CAS identity over a bounded pull source and bounded multipart writes.
/// Dropping the caller schedules multipart abort, including activity cancellation.
pub(crate) async fn put_streamed_object(
    store: &dyn ObjectStore,
    key: &str,
    expected: &BlobRef,
    size: u64,
    source: &mut dyn harness::storage::BlobSource,
) -> Result<object_store::PutResult, BlobStoreError> {
    use sha2::{Digest, Sha256};
    struct Guard(Option<Box<dyn object_store::MultipartUpload>>);
    impl Drop for Guard {
        fn drop(&mut self) {
            if let Some(mut upload) = self.0.take()
                && let Ok(handle) = tokio::runtime::Handle::try_current()
            {
                handle.spawn(async move {
                    let _ = upload.abort().await;
                });
            }
        }
    }
    if size > 1024_u64.pow(4) {
        return Err(BlobStoreError::Store {
            message: "streamed blob exceeds one TiB limit".into(),
        });
    }
    let mut guard = Guard(Some(
        store
            .put_multipart(&ObjectPath::from(key))
            .await
            .map_err(|e| object_store_error("begin multipart blob", key, e))?,
    ));
    let upload = &mut guard.0;
    // Keep at most one part in flight and fewer than 10,000 parts up to one TiB.
    let part_size = (8 * 1024 * 1024).max(size.div_ceil(9000) as usize);
    let result = async {
        let mut hash = Sha256::new();
        let mut length = 0u64;
        let mut part = Vec::with_capacity(part_size + 256 * 1024);
        loop {
            let chunk = source.read_chunk(256 * 1024).await?;
            if chunk.is_empty() {
                break;
            }
            length =
                length
                    .checked_add(chunk.len() as u64)
                    .ok_or_else(|| BlobStoreError::Store {
                        message: "blob length overflow".into(),
                    })?;
            if chunk.len() > 256 * 1024 || length > size {
                return Err(BlobStoreError::Store {
                    message: "blob stream exceeded size/chunk limit".into(),
                });
            }
            hash.update(&chunk);
            part.extend(chunk);
            if part.len() >= part_size {
                upload
                    .as_mut()
                    .unwrap()
                    .put_part(std::mem::take(&mut part).into())
                    .await
                    .map_err(|e| crate::shared::object_store_error("upload blob part", key, e))?;
                part = Vec::with_capacity(part_size + 256 * 1024);
            }
        }
        if length != size || format!("sha256:{}", hex::encode(hash.finalize())) != expected.as_str()
        {
            return Err(BlobStoreError::Store {
                message: "blob stream length/digest mismatch".into(),
            });
        }
        if !part.is_empty() {
            upload
                .as_mut()
                .unwrap()
                .put_part(part.into())
                .await
                .map_err(|e| crate::shared::object_store_error("upload final blob part", key, e))?;
        }
        upload
            .as_mut()
            .unwrap()
            .complete()
            .await
            .map_err(|e| crate::shared::object_store_error("complete multipart blob", key, e))
    }
    .await;
    match result {
        Ok(value) => {
            guard.0.take();
            Ok(value)
        }
        Err(error) => {
            if let Some(mut upload) = guard.0.take() {
                let _ = upload.abort().await;
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PgStoreConfig;
    use uuid::Uuid;

    struct Source {
        remaining: u64,
    }
    #[async_trait::async_trait]
    impl harness::storage::BlobSource for Source {
        async fn read_chunk(&mut self, max_bytes: usize) -> Result<Vec<u8>, BlobStoreError> {
            assert!(max_bytes <= 256 * 1024);
            let count = self.remaining.min(max_bytes as u64) as usize;
            self.remaining -= count as u64;
            Ok(vec![0x91; count])
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn multipart_stream_verifies_raw_identity_and_aborts_invalid_content() {
        let store = object_store::memory::InMemory::new();
        let size = 10 * 1024 * 1024 + 13;
        let expected = BlobRef::from_bytes(&vec![0x91; size]);
        put_streamed_object(
            &store,
            "complete",
            &expected,
            size as u64,
            &mut Source {
                remaining: size as u64,
            },
        )
        .await
        .unwrap();
        let bytes = store
            .get(&ObjectPath::from("complete"))
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert_eq!(BlobRef::from_bytes(&bytes), expected);
        for (key, length, digest) in [
            ("short", size as u64 - 1, expected.clone()),
            ("wrong", size as u64, BlobRef::from_bytes(b"wrong")),
        ] {
            assert!(
                put_streamed_object(
                    &store,
                    key,
                    &digest,
                    size as u64,
                    &mut Source { remaining: length }
                )
                .await
                .is_err()
            );
            assert!(matches!(
                store.get(&ObjectPath::from(key)).await,
                Err(object_store::Error::NotFound { .. })
            ));
        }
    }

    #[test]
    fn universe_cas_prefix_contains_every_direct_blob_key() {
        let universe_id = Uuid::new_v4();
        let config = PgStoreConfig::new(universe_id)
            .with_inline_threshold_bytes(8)
            .with_object_prefix("/prefix/");
        let key = direct_blob_key(&config, &BlobRef::from_bytes(b"hello")).expect("blob key");
        let prefix = universe_cas_object_prefix("/prefix/", universe_id);
        assert_eq!(prefix, format!("prefix/universes/{universe_id}/cas/"));
        assert!(key.starts_with(&prefix));
        assert_eq!(
            universe_cas_object_prefix("", universe_id),
            format!("universes/{universe_id}/cas/")
        );
    }

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
