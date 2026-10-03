use async_trait::async_trait;
use harness::{
    BlobRef,
    storage::{BlobEdge, BlobGraphStore, BlobInfo, BlobStore, BlobStoreError},
};
use sqlx::Row;

use crate::{
    PgStore,
    object::direct_blob_key,
    shared::{
        blob_sql_error, blob_store_error, i64_to_u64, sha256_hex, unix_now_ms, usize_to_blob_i64,
    },
};

impl PgStore {
    /// Renew the upload grace when existing refs are admitted as new input.
    /// One indexed update for the whole batch; reads never refresh grace.
    /// Missing refs fail admission, including refs still present in a cache.
    pub async fn touch_blob_refs(&self, refs: &[BlobRef]) -> Result<(), BlobStoreError> {
        if refs.is_empty() {
            return Ok(());
        }
        let digests = refs.iter().map(sha256_hex).collect::<Result<Vec<_>, _>>()?;
        let touched: Vec<String> = sqlx::query_scalar(
            "UPDATE cas_blobs SET touched_at_ms = GREATEST(touched_at_ms, $3) \
             WHERE universe_id = $1 AND digest = ANY($2::text[]) RETURNING digest",
        )
        .bind(self.config.universe_id)
        .bind(&digests)
        .bind(unix_now_ms())
        .fetch_all(&self.pool)
        .await
        .map_err(|error| blob_sql_error("refresh input blob grace", error))?;
        let touched: std::collections::BTreeSet<_> = touched.into_iter().collect();
        for (blob_ref, digest) in refs.iter().zip(digests) {
            if !touched.contains(digest) {
                return Err(BlobStoreError::NotFound {
                    blob_ref: blob_ref.clone(),
                });
            }
        }
        Ok(())
    }

    /// Touch-or-insert. Existing content only moves `touched_at_ms` forward;
    /// bytes are never rewritten and objects never re-uploaded. The touch is
    /// what keeps a deduplicated write safe against a concurrent sweep: the
    /// sweep only considers blobs untouched for longer than its grace, so a
    /// ref handed out here has a fresh grace window for its holder to commit.
    async fn put_single_blob(&self, bytes: Vec<u8>) -> Result<BlobRef, BlobStoreError> {
        let blob_ref = BlobRef::from_bytes(&bytes);
        let digest = sha256_hex(&blob_ref)?;
        let byte_len = usize_to_blob_i64(bytes.len(), "blob byte length")?;
        let now_ms = unix_now_ms();
        // Write-through: the ref derives from these exact bytes, so the
        // hash-verified invariant of the cache holds by construction.
        if let Some(cache) = &self.blob_cache {
            cache.insert(self.config.universe_id, &blob_ref, &bytes);
        }

        let touched = sqlx::query(
            r#"
            UPDATE cas_blobs
            SET touched_at_ms = GREATEST(touched_at_ms, $3)
            WHERE universe_id = $1 AND digest = $2
            RETURNING 1
            "#,
        )
        .bind(self.config.universe_id)
        .bind(digest)
        .bind(now_ms)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| blob_sql_error("touch blob", error))?;
        if touched.is_some() {
            return Ok(blob_ref);
        }

        self.ensure_universe()
            .await
            .map_err(|error| blob_store_error("ensure universe", error))?;

        if bytes.len() <= self.config.inline_threshold_bytes {
            sqlx::query(
                r#"
                INSERT INTO cas_blobs (
                    universe_id,
                    digest,
                    byte_len,
                    storage_kind,
                    inline_bytes,
                    created_at_ms,
                    touched_at_ms
                )
                VALUES ($1, $2, $3, 'inline', $4, $5, $5)
                ON CONFLICT (universe_id, digest) DO UPDATE
                SET touched_at_ms = GREATEST(cas_blobs.touched_at_ms, EXCLUDED.touched_at_ms)
                "#,
            )
            .bind(self.config.universe_id)
            .bind(digest)
            .bind(byte_len)
            .bind(bytes)
            .bind(now_ms)
            .execute(&self.pool)
            .await
            .map_err(|error| blob_sql_error("insert inline blob", error))?;
            return Ok(blob_ref);
        }

        let object_key = direct_blob_key(&self.config, &blob_ref)?;
        let put_result = self.put_object(&object_key, bytes).await?;
        // Each upload has its own physical key. A delayed sweep can only
        // delete the previous incarnation, never this replacement's bytes.
        let stored_key: Option<String> = sqlx::query_scalar(
            r#"
            INSERT INTO cas_blobs (
                universe_id,
                digest,
                byte_len,
                storage_kind,
                object_key,
                object_etag,
                object_version,
                created_at_ms,
                touched_at_ms
            )
            VALUES ($1, $2, $3, 'object', $4, $5, $6, $7, $7)
            ON CONFLICT (universe_id, digest) DO UPDATE
            SET touched_at_ms = GREATEST(cas_blobs.touched_at_ms, EXCLUDED.touched_at_ms)
            RETURNING object_key
            "#,
        )
        .bind(self.config.universe_id)
        .bind(digest)
        .bind(byte_len)
        .bind(&object_key)
        .bind(put_result.e_tag)
        .bind(put_result.version)
        .bind(now_ms)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| blob_sql_error("insert object blob", error))?;
        if stored_key.as_deref() != Some(object_key.as_str()) {
            // Another writer won (possibly with inline storage). Only our
            // unused upload is ours to remove. A failed cleanup leaks safely.
            let cleanup = self.delete_blob_objects(&[object_key]).await;
            for (key, error) in cleanup.failures {
                tracing::warn!(%key, %error, "could not delete unused CAS upload");
            }
        }
        Ok(blob_ref)
    }

    /// Catalog timestamps of one blob: `(created_at_ms, touched_at_ms)`.
    pub async fn blob_timestamps(
        &self,
        blob_ref: &BlobRef,
    ) -> Result<Option<(u64, u64)>, BlobStoreError> {
        let digest = sha256_hex(blob_ref)?;
        let row = sqlx::query(
            r#"
            SELECT created_at_ms, touched_at_ms
            FROM cas_blobs
            WHERE universe_id = $1 AND digest = $2
            "#,
        )
        .bind(self.config.universe_id)
        .bind(digest)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| blob_sql_error("load blob timestamps", error))?;
        row.map(|row| {
            let created_at_ms: i64 = row
                .try_get("created_at_ms")
                .map_err(|error| blob_sql_error("decode blob created time", error))?;
            let touched_at_ms: i64 = row
                .try_get("touched_at_ms")
                .map_err(|error| blob_sql_error("decode blob touched time", error))?;
            Ok((
                i64_to_u64(created_at_ms, "blob created time")
                    .map_err(|message| BlobStoreError::Store { message })?,
                i64_to_u64(touched_at_ms, "blob touched time")
                    .map_err(|message| BlobStoreError::Store { message })?,
            ))
        })
        .transpose()
    }
}

#[async_trait]
impl BlobStore for PgStore {
    async fn put_bytes(&self, bytes: Vec<u8>) -> Result<BlobRef, BlobStoreError> {
        self.put_single_blob(bytes).await
    }

    async fn put_many(&self, blobs: Vec<Vec<u8>>) -> Result<Vec<BlobRef>, BlobStoreError> {
        let mut refs = Vec::with_capacity(blobs.len());
        for bytes in blobs {
            refs.push(self.put_single_blob(bytes).await?);
        }
        Ok(refs)
    }

    async fn retain_blob(&self, blob_ref: &BlobRef) -> Result<(), BlobStoreError> {
        self.touch_blob_refs(std::slice::from_ref(blob_ref)).await
    }

    async fn read_blob_range(
        &self,
        blob_ref: &BlobRef,
        offset: u64,
        max_bytes: usize,
    ) -> Result<Vec<u8>, BlobStoreError> {
        use object_store::ObjectStoreExt;
        if max_bytes > 1024 * 1024 {
            return Err(BlobStoreError::Store {
                message: "blob range limit exceeded".into(),
            });
        }
        // PostgreSQL also slices inline values at the source, even if a deployment
        // uses an unusually high inline threshold. The receiver verifies the whole hash.
        let row = sqlx::query(
            "SELECT byte_len, object_key, CASE WHEN inline_bytes IS NOT NULL \
             THEN substring(inline_bytes FROM (LEAST($3,byte_len)+1)::integer FOR $4) \
             END AS inline_bytes FROM cas_blobs WHERE universe_id=$1 AND digest=$2",
        )
        .bind(self.config.universe_id)
        .bind(sha256_hex(blob_ref)?)
        .bind(i64::try_from(offset).unwrap_or(i64::MAX))
        .bind(max_bytes as i32)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| blob_sql_error("load blob range", e))?
        .ok_or_else(|| BlobStoreError::NotFound {
            blob_ref: blob_ref.clone(),
        })?;
        let length: i64 = row
            .try_get("byte_len")
            .map_err(|e| blob_sql_error("decode blob length", e))?;
        let length = length as u64;
        if offset >= length {
            return Ok(vec![]);
        }
        let end = offset.saturating_add(max_bytes as u64).min(length);
        if let Some(bytes) = row
            .try_get::<Option<Vec<u8>>, _>("inline_bytes")
            .map_err(|e| blob_sql_error("decode inline bytes", e))?
        {
            if bytes.len() as u64 != end - offset {
                return Err(BlobStoreError::Store {
                    message: "inline blob range length mismatch".into(),
                });
            }
            return Ok(bytes);
        }
        let key: String = row
            .try_get("object_key")
            .map_err(|e| blob_sql_error("decode object key", e))?;
        let store = self
            .object_store
            .as_ref()
            .ok_or_else(|| BlobStoreError::Store {
                message: "object store unavailable".into(),
            })?;
        store
            .get_range(&object_store::path::Path::from(key.as_str()), offset..end)
            .await
            .map(|b| b.to_vec())
            .map_err(|e| crate::shared::object_store_error("read object range", &key, e))
    }

    async fn put_stream(
        &self,
        expected: &BlobRef,
        size: u64,
        source: &mut dyn harness::storage::BlobSource,
    ) -> Result<BlobRef, BlobStoreError> {
        let digest = sha256_hex(expected)?;
        // Admission renews existing content without downloading or re-uploading it.
        if self.has_blob(expected).await? {
            if self.stat_blob(expected).await?.byte_len != size {
                return Err(BlobStoreError::Store {
                    message: "existing blob size differs from stream".into(),
                });
            }
            self.retain_blob(expected).await?;
            return Ok(expected.clone());
        }
        if size <= self.config.inline_threshold_bytes.min(8 * 1024 * 1024) as u64 {
            let mut bytes = Vec::new();
            loop {
                let chunk = source.read_chunk(256 * 1024).await?;
                if chunk.is_empty() {
                    break;
                }
                if chunk.len() > 256 * 1024 || bytes.len() as u64 + chunk.len() as u64 > size {
                    return Err(BlobStoreError::Store {
                        message: "blob stream size mismatch".into(),
                    });
                }
                bytes.extend(chunk);
            }
            if bytes.len() as u64 != size || BlobRef::from_bytes(&bytes) != *expected {
                return Err(BlobStoreError::Store {
                    message: "blob stream length/digest mismatch".into(),
                });
            }
            return self.put_single_blob(bytes).await;
        }
        if size > 1024_u64.pow(4) {
            return Err(BlobStoreError::Store {
                message: "streamed blob exceeds one TiB limit".into(),
            });
        }
        self.ensure_universe()
            .await
            .map_err(|e| blob_store_error("ensure universe", e))?;
        let key = direct_blob_key(&self.config, expected)?;
        let store = self
            .object_store
            .as_ref()
            .ok_or_else(|| BlobStoreError::Store {
                message: "large blob requires object storage".into(),
            })?;
        let result =
            crate::object::put_streamed_object(store.as_ref(), &key, expected, size, source)
                .await?;
        let stored: Option<String> = sqlx::query_scalar(
            r#"
            INSERT INTO cas_blobs (
                universe_id, digest, byte_len, storage_kind, object_key,
                object_etag, object_version, created_at_ms, touched_at_ms
            )
            VALUES ($1, $2, $3, 'object', $4, $5, $6, $7, $7)
            ON CONFLICT (universe_id, digest) DO UPDATE
            SET touched_at_ms = GREATEST(cas_blobs.touched_at_ms, EXCLUDED.touched_at_ms)
            RETURNING object_key
            "#,
        )
        .bind(self.config.universe_id)
        .bind(digest)
        .bind(size as i64)
        .bind(&key)
        .bind(result.e_tag)
        .bind(result.version)
        .bind(unix_now_ms())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| blob_sql_error("publish streamed blob", e))?;
        if stored.as_deref() != Some(key.as_str()) {
            let _ = self.delete_blob_objects(&[key]).await;
        }
        Ok(expected.clone())
    }

    async fn read_bytes(&self, blob_ref: &BlobRef) -> Result<Vec<u8>, BlobStoreError> {
        let digest = sha256_hex(blob_ref)?;
        if let Some(cache) = &self.blob_cache
            && let Some(bytes) = cache.get(self.config.universe_id, blob_ref)
        {
            return Ok(bytes.to_vec());
        }
        let row = sqlx::query(
            r#"
            SELECT storage_kind, inline_bytes, object_key
            FROM cas_blobs
            WHERE universe_id = $1 AND digest = $2
            "#,
        )
        .bind(self.config.universe_id)
        .bind(digest)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| blob_sql_error("load blob", error))?;

        let Some(row) = row else {
            return Err(BlobStoreError::NotFound {
                blob_ref: blob_ref.clone(),
            });
        };

        let storage_kind: String = row
            .try_get("storage_kind")
            .map_err(|error| blob_sql_error("decode blob storage kind", error))?;
        let bytes = match storage_kind.as_str() {
            "inline" => row
                .try_get::<Option<Vec<u8>>, _>("inline_bytes")
                .map_err(|error| blob_sql_error("decode inline blob bytes", error))?
                .ok_or_else(|| BlobStoreError::Store {
                    message: format!("inline blob '{blob_ref}' has no inline bytes"),
                })?,
            "object" => {
                let object_key = row
                    .try_get::<Option<String>, _>("object_key")
                    .map_err(|error| blob_sql_error("decode blob object key", error))?
                    .ok_or_else(|| BlobStoreError::Store {
                        message: format!("object blob '{blob_ref}' has no object key"),
                    })?;
                self.get_object(&object_key, blob_ref).await?
            }
            other => {
                return Err(BlobStoreError::Store {
                    message: format!("unsupported blob storage kind '{other}' for {blob_ref}"),
                });
            }
        };

        let actual = BlobRef::from_bytes(&bytes);
        if &actual != blob_ref {
            return Err(BlobStoreError::Store {
                message: format!("blob hash mismatch: expected {blob_ref}, got {actual}"),
            });
        }
        // Only hash-verified bytes enter the cache.
        if let Some(cache) = &self.blob_cache {
            cache.insert(self.config.universe_id, blob_ref, &bytes);
        }
        Ok(bytes)
    }

    async fn has_blob(&self, blob_ref: &BlobRef) -> Result<bool, BlobStoreError> {
        let digest = sha256_hex(blob_ref)?;
        sqlx::query(
            r#"
            SELECT 1
            FROM cas_blobs
            WHERE universe_id = $1 AND digest = $2
            "#,
        )
        .bind(self.config.universe_id)
        .bind(digest)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.is_some())
        .map_err(|error| blob_sql_error("check blob existence", error))
    }

    async fn stat_blob(&self, blob_ref: &BlobRef) -> Result<BlobInfo, BlobStoreError> {
        let digest = sha256_hex(blob_ref)?;
        let row = sqlx::query(
            r#"
            SELECT byte_len
            FROM cas_blobs
            WHERE universe_id = $1 AND digest = $2
            "#,
        )
        .bind(self.config.universe_id)
        .bind(digest)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| blob_sql_error("stat blob", error))?;

        let Some(row) = row else {
            return Err(BlobStoreError::NotFound {
                blob_ref: blob_ref.clone(),
            });
        };
        let byte_len = row
            .try_get::<i64, _>("byte_len")
            .map_err(|error| blob_sql_error("decode blob byte length", error))?;
        Ok(BlobInfo {
            blob_ref: blob_ref.clone(),
            byte_len: i64_to_u64(byte_len, "blob byte length")
                .map_err(|message| BlobStoreError::Store { message })?,
        })
    }
}

#[async_trait]
impl BlobGraphStore for PgStore {
    async fn record_blob_edges(&self, edges: Vec<BlobEdge>) -> Result<(), BlobStoreError> {
        if edges.is_empty() {
            return Ok(());
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| blob_sql_error("begin blob edges transaction", error))?;
        for edge in edges {
            if edge.edge_kind.is_empty() {
                return Err(BlobStoreError::Store {
                    message: "blob edge kind must not be empty".into(),
                });
            }
            sqlx::query(
                r#"
                INSERT INTO cas_blob_edges (
                    universe_id,
                    parent_digest,
                    child_digest,
                    edge_kind
                )
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (universe_id, parent_digest, child_digest, edge_kind) DO NOTHING
                "#,
            )
            .bind(self.config.universe_id)
            .bind(sha256_hex(&edge.parent)?)
            .bind(sha256_hex(&edge.child)?)
            .bind(edge.edge_kind)
            .execute(&mut *tx)
            .await
            .map_err(|error| blob_sql_error("record blob edge", error))?;
        }
        tx.commit()
            .await
            .map_err(|error| blob_sql_error("commit blob edges transaction", error))
    }
}
