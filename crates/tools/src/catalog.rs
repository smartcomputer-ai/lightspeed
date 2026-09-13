//! Publication of runtime-owned catalogs as immutable text with structured provenance.

use engine::storage::{BlobStore, BlobStoreError};
use engine::{BlobRef, ContextEntryInput, ContextEntryKey, ContextEntryKind, CoreAgentCommand};

pub const VFS_CATALOG_CONTEXT_KEY: &str = "runtime.catalog.vfs";
pub const SKILL_CATALOG_CONTEXT_KEY: &str = "runtime.catalog.skills.vfs";
pub const SUBAGENT_CATALOG_CONTEXT_KEY: &str = "runtime.catalog.subagents";
pub const ENVIRONMENT_CATALOG_CONTEXT_KEY: &str = "runtime.catalog.environments";

/// Store the provider-neutral body once. The structured source remains a
/// separate durable root through provenance; its writer owns any nested edges.
pub async fn catalog_context_input(
    blobs: &dyn BlobStore,
    title: &str,
    body: String,
    snapshot_ref: BlobRef,
) -> Result<ContextEntryInput, BlobStoreError> {
    Ok(ContextEntryInput {
        kind: ContextEntryKind::Catalog {
            title: title.to_owned(),
        },
        content: engine::ContentRef {
            content_ref: blobs.put_bytes(body.into_bytes()).await?,
            media_type: Some("text/markdown".to_owned()),
            provider_kind: None,
        },
        preview: Some(title.to_owned()),
        origin: None,
        provenance_ref: Some(snapshot_ref),
        token_estimate: None,
    })
}

pub fn catalog_publication_command(
    current: Option<&ContextEntryInput>,
    key: &str,
    entry: ContextEntryInput,
) -> Option<CoreAgentCommand> {
    (current != Some(&entry)).then(|| CoreAgentCommand::UpsertContext {
        expected_revision: None,
        key: ContextEntryKey::new(key),
        entry,
    })
}

pub fn clear_catalog_command(
    current: Option<&ContextEntryInput>,
    key: &str,
) -> Option<CoreAgentCommand> {
    current.map(|_| CoreAgentCommand::RemoveContext {
        expected_revision: None,
        key: ContextEntryKey::new(key),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::storage::InMemoryBlobStore;

    #[tokio::test(flavor = "current_thread")]
    async fn publication_tracks_text_and_provenance_independently() {
        let blobs = InMemoryBlobStore::new();
        let snapshot = blobs.put_bytes(b"source v1".to_vec()).await.unwrap();
        let first = catalog_context_input(&blobs, "Menu", "body v1\n".to_owned(), snapshot.clone())
            .await
            .unwrap();
        assert_eq!(
            blobs.read_bytes(&first.content.content_ref).await.unwrap(),
            b"body v1\n"
        );
        assert_eq!(first.provenance_ref.as_ref(), Some(&snapshot));
        assert!(
            catalog_publication_command(Some(&first), VFS_CATALOG_CONTEXT_KEY, first.clone())
                .is_none()
        );

        // A renderer change must publish even if discovery returned the same source.
        let rerendered = catalog_context_input(&blobs, "Menu", "body v2\n".to_owned(), snapshot)
            .await
            .unwrap();
        assert!(
            catalog_publication_command(Some(&first), VFS_CATALOG_CONTEXT_KEY, rerendered.clone())
                .is_some()
        );
        // Old context continues to point at its original bytes.
        assert_eq!(
            blobs.read_bytes(&first.content.content_ref).await.unwrap(),
            b"body v1\n"
        );

        // Metadata may change without changing the model-facing text.
        let snapshot = blobs.put_bytes(b"source v2".to_vec()).await.unwrap();
        let updated = catalog_context_input(&blobs, "Menu", "body v2\n".to_owned(), snapshot)
            .await
            .unwrap();
        assert_eq!(updated.content, rerendered.content);
        assert!(
            catalog_publication_command(Some(&rerendered), VFS_CATALOG_CONTEXT_KEY, updated)
                .is_some()
        );
    }
}
