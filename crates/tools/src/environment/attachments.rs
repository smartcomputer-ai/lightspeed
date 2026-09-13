//! The environment catalog: the session's attached environments as the model
//! sees them, with this session's access on each and which one is active.
//! Built from the admitted grant and registry records, never from a live
//! machine, and published like the sub-agent catalog.

use engine::storage::{BlobStore, BlobStoreError};
use engine::{
    BlobRef, ContextEntryInput, CoreAgentCommand, EnvironmentAccess, EnvironmentsFeature,
};
use serde::{Deserialize, Serialize};

use crate::catalog::{
    ENVIRONMENT_CATALOG_CONTEXT_KEY, catalog_context_input, catalog_publication_command,
};
use crate::environment::control::{
    ENVIRONMENT_ACTIVATE_TOOL_NAME, ENVIRONMENT_LIST_TOOL_NAME, ENVIRONMENT_READ_TOOL_NAME,
};

pub const ENVIRONMENT_CATALOG_SCHEMA_VERSION: &str = "lightspeed.environments.catalog.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentCatalogSnapshot {
    pub schema_version: String,
    pub environments: Vec<EnvironmentCatalogEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_environment_id: Option<String>,
    pub selection: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentCatalogEntry {
    pub environment_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Lowercase lifecycle status from the registry; absent when the record
    /// is missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    pub access: EnvironmentAccess,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub default: bool,
}

/// A registry fact joined onto an attachment by the publisher.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvironmentCatalogRecord {
    pub display_name: Option<String>,
    pub status: Option<String>,
}

impl EnvironmentCatalogSnapshot {
    pub fn new(
        feature: &EnvironmentsFeature,
        active_environment_id: Option<&str>,
        record: impl Fn(&str) -> EnvironmentCatalogRecord,
    ) -> Self {
        Self {
            schema_version: ENVIRONMENT_CATALOG_SCHEMA_VERSION.to_owned(),
            environments: feature
                .environments
                .iter()
                .map(|attachment| {
                    let record = record(&attachment.environment_id);
                    EnvironmentCatalogEntry {
                        environment_id: attachment.environment_id.clone(),
                        display_name: record.display_name,
                        status: record.status,
                        access: attachment.access,
                        working_directory: attachment.working_directory.clone(),
                        default: attachment.default,
                    }
                })
                .collect(),
            active_environment_id: active_environment_id.map(str::to_owned),
            selection: feature.selection,
        }
    }
}

pub(crate) fn environment_catalog_text(catalog: &EnvironmentCatalogSnapshot) -> String {
    let mut text = String::new();
    if catalog.environments.is_empty() {
        text.push_str("No environments are attached to this session.");
        return text;
    }
    text.push_str(
        "Environments attached to this session. Ordinary file, command, and job tools operate on the active environment; a call the active environment's access does not cover is rejected, and the tool list does not change when you switch.\n\n",
    );
    for entry in &catalog.environments {
        let name = entry
            .display_name
            .as_deref()
            .filter(|name| !name.trim().is_empty() && *name != entry.environment_id)
            .map(|name| format!(" ({name})"))
            .unwrap_or_default();
        let mut markers = Vec::new();
        if catalog.active_environment_id.as_deref() == Some(entry.environment_id.as_str()) {
            markers.push("active");
        }
        if entry.default {
            markers.push("default");
        }
        let markers = if markers.is_empty() {
            String::new()
        } else {
            format!(" [{}]", markers.join(", "))
        };
        text.push_str(&format!("- {}{name}{markers}\n", entry.environment_id));
        text.push_str(&format!("  access: {}", entry.access.describe()));
        if let Some(cwd) = &entry.working_directory {
            text.push_str(&format!("; working directory: {cwd}"));
        }
        match &entry.status {
            Some(status) => text.push_str(&format!("; status: {status}\n")),
            None => text.push_str("; status: unknown (record missing)\n"),
        }
    }
    if catalog.active_environment_id.is_none() {
        text.push_str("\nNo environment is active.");
    }
    if catalog.selection {
        text.push_str(&format!(
            "\nUse {ENVIRONMENT_LIST_TOOL_NAME} to see live status and {ENVIRONMENT_ACTIVATE_TOOL_NAME} to switch; {ENVIRONMENT_READ_TOOL_NAME} inspects one environment."
        ));
    } else {
        text.push_str(&format!(
            "\nThe active environment is selected outside this session; {ENVIRONMENT_READ_TOOL_NAME} inspects it."
        ));
    }
    text
}

#[derive(Debug, thiserror::Error)]
pub enum EnvironmentCatalogError {
    #[error(transparent)]
    BlobStore(#[from] BlobStoreError),

    #[error("failed to encode environment catalog: {message}")]
    Encode { message: String },
}

pub async fn environment_catalog_context_input(
    blobs: &dyn BlobStore,
    snapshot: &EnvironmentCatalogSnapshot,
    snapshot_ref: BlobRef,
) -> Result<ContextEntryInput, BlobStoreError> {
    let mut entry = catalog_context_input(
        blobs,
        "Environment catalog",
        environment_catalog_text(snapshot),
        snapshot_ref,
    )
    .await?;
    // Empty suffix records the absence of a selection. The workflow can
    // invalidate this observation after a switch without reading its blobs.
    entry.origin = Some(format!(
        "runtime.environments:{}",
        snapshot.active_environment_id.as_deref().unwrap_or("")
    ));
    Ok(entry)
}

/// Write the snapshot to CAS and return the upsert when it differs from the
/// active entry (content-addressed, so an unchanged catalog is a no-op).
pub async fn prepare_environment_catalog_publication(
    blobs: &dyn BlobStore,
    current: Option<&ContextEntryInput>,
    snapshot: &EnvironmentCatalogSnapshot,
) -> Result<Option<CoreAgentCommand>, EnvironmentCatalogError> {
    let bytes = serde_json::to_vec(snapshot).map_err(|error| EnvironmentCatalogError::Encode {
        message: error.to_string(),
    })?;
    let catalog_ref = blobs.put_bytes(bytes).await?;
    let entry = environment_catalog_context_input(blobs, snapshot, catalog_ref).await?;
    Ok(catalog_publication_command(
        current,
        ENVIRONMENT_CATALOG_CONTEXT_KEY,
        entry,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::EnvironmentAttachment;

    fn feature() -> EnvironmentsFeature {
        EnvironmentsFeature {
            selection: true,
            environments: vec![
                EnvironmentAttachment {
                    environment_id: "env_ci".to_owned(),
                    default: true,
                    access: EnvironmentAccess::Jobs,
                    working_directory: Some("/srv/app".to_owned()),
                },
                EnvironmentAttachment {
                    environment_id: "env_logs".to_owned(),
                    default: false,
                    access: EnvironmentAccess::Read,
                    working_directory: None,
                },
            ],
            ..EnvironmentsFeature::default()
        }
    }

    #[test]
    fn catalog_text_lists_access_markers_and_status() {
        let snapshot = EnvironmentCatalogSnapshot::new(&feature(), Some("env_ci"), |id| {
            EnvironmentCatalogRecord {
                display_name: (id == "env_ci").then(|| "CI runner".to_owned()),
                status: (id == "env_ci").then(|| "ready".to_owned()),
            }
        });
        let text = environment_catalog_text(&snapshot);
        assert!(text.contains("- env_ci (CI runner) [active, default]"));
        assert!(text.contains(
            "access: read, edit, exec, jobs; working directory: /srv/app; status: ready"
        ));
        assert!(text.contains("- env_logs\n  access: read; status: unknown (record missing)"));
        assert!(text.contains(ENVIRONMENT_ACTIVATE_TOOL_NAME));
        assert!(!text.contains("No environment is active"));
    }

    #[test]
    fn catalog_text_without_selection_or_active_environment() {
        let mut feature = feature();
        feature.selection = false;
        let snapshot = EnvironmentCatalogSnapshot::new(&feature, None, |_| {
            EnvironmentCatalogRecord::default()
        });
        let text = environment_catalog_text(&snapshot);
        assert!(text.contains("No environment is active"));
        assert!(text.contains("selected outside this session"));
        assert!(!text.contains(ENVIRONMENT_ACTIVATE_TOOL_NAME));

        let empty = EnvironmentCatalogSnapshot::new(&EnvironmentsFeature::default(), None, |_| {
            EnvironmentCatalogRecord::default()
        });
        assert_eq!(
            environment_catalog_text(&empty),
            "No environments are attached to this session."
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn publication_records_selection_including_its_absence() {
        let blobs = engine::storage::InMemoryBlobStore::new();
        for active in [None, Some("env_ci"), Some("env_logs")] {
            let snapshot = EnvironmentCatalogSnapshot::new(&feature(), active, |_| {
                EnvironmentCatalogRecord::default()
            });
            let entry = environment_catalog_context_input(
                &blobs,
                &snapshot,
                BlobRef::from_bytes(b"snapshot"),
            )
            .await
            .unwrap();
            assert_eq!(
                entry.origin,
                Some(format!("runtime.environments:{}", active.unwrap_or("")))
            );
            let text = blobs.read_text(&entry.content.content_ref).await.unwrap();
            assert_eq!(text.contains("No environment is active."), active.is_none());
            if let Some(id) = active {
                assert!(text.contains(&format!("- {id} [active")));
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn publication_is_a_no_op_when_unchanged() {
        let blobs = engine::storage::InMemoryBlobStore::new();
        let snapshot = EnvironmentCatalogSnapshot::new(&feature(), None, |_| {
            EnvironmentCatalogRecord::default()
        });
        let first = prepare_environment_catalog_publication(&blobs, None, &snapshot)
            .await
            .unwrap()
            .expect("first publication upserts");
        let CoreAgentCommand::UpsertContext { entry, key, .. } = first else {
            panic!("expected an upsert");
        };
        assert_eq!(key.as_str(), ENVIRONMENT_CATALOG_CONTEXT_KEY);
        assert!(
            prepare_environment_catalog_publication(&blobs, Some(&entry), &snapshot)
                .await
                .unwrap()
                .is_none()
        );
    }
}
