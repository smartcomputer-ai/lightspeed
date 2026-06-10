//! Provider-neutral prompt text for skill catalog and activation context
//! entries. Adapters lower these entries into whatever message shape their
//! provider expects, but the visible text is identical across providers.

use engine::{BlobRef, SkillId, storage::BlobStore};
use tools::skills::{SkillCatalogSnapshot, SkillLocation, SkillMetadata};

use crate::error::{LlmAdapterError, LlmAdapterResult};

pub(crate) async fn read_skill_catalog(
    blobs: &dyn BlobStore,
    blob_ref: &BlobRef,
) -> LlmAdapterResult<SkillCatalogSnapshot> {
    let bytes = blobs.read_bytes(blob_ref).await?;
    serde_json::from_slice(&bytes).map_err(|error| LlmAdapterError::InvalidJson {
        blob_ref: blob_ref.clone(),
        message: error.to_string(),
    })
}

pub(crate) fn skill_catalog_text(catalog: &SkillCatalogSnapshot) -> String {
    let mut text = String::from("Forge skill catalog:\n\n");
    if catalog.skills.is_empty() {
        text.push_str("No Forge skills are currently available.");
        return text;
    }

    text.push_str(
        "When a skill is relevant, read its SKILL.md through the available file tool before following it.\n\n",
    );
    for skill in &catalog.skills {
        text.push_str(&skill_catalog_entry(skill));
    }
    text
}

fn skill_catalog_entry(skill: &SkillMetadata) -> String {
    let mut entry = format!(
        "- {} ({})\n  description: {}\n  skill_doc_path: {}",
        skill.name,
        skill.skill_id,
        skill.description,
        skill_doc_path(&skill.location)
    );
    if let Some(target) = &skill.target {
        entry.push_str(&format!("\n  target: {}:{}", target.namespace, target.id));
    }
    if let Some(short_description) = &skill.short_description {
        entry.push_str(&format!("\n  short_description: {short_description}"));
    }
    entry.push('\n');
    entry
}

fn skill_doc_path(location: &SkillLocation) -> &str {
    match location {
        SkillLocation::MountedSnapshot { skill_doc_path, .. }
        | SkillLocation::MountedWorkspace { skill_doc_path, .. } => skill_doc_path.as_str(),
        SkillLocation::HostFilesystem { skill_doc_path, .. } => skill_doc_path,
    }
}

pub(crate) fn skill_activation_text(skill_id: &SkillId, text: String) -> String {
    format!("Forge loaded skill ({skill_id}):\n\n{text}")
}
