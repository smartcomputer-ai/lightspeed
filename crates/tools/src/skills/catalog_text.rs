//! Catalog text rendered once by the publisher.

use crate::skills::{SkillCatalogSnapshot, SkillLocation, SkillMetadata};

pub(crate) fn skill_catalog_text(catalog: &SkillCatalogSnapshot) -> String {
    let mut text = String::new();
    if catalog.skills.is_empty() {
        text.push_str("No VFS skills are currently available.");
        return text;
    }

    text.push_str(
        "When a skill is relevant, read its SKILL.md through the appropriate VFS file tool before following it. VFS skill paths are not environment paths.\n\n",
    );
    for skill in &catalog.skills {
        text.push_str(&skill_catalog_entry(skill));
    }
    text
}

fn skill_catalog_entry(skill: &SkillMetadata) -> String {
    let mut entry = format!(
        "- {} ({})\n  description: {}\n  skill_doc_path: {}\n  skill_dir_path: {}",
        skill.name,
        skill.skill_id,
        skill.description,
        skill_doc_path(&skill.location),
        skill_dir_path(&skill.location)
    );
    if let Some(short_description) = &skill.short_description {
        entry.push_str(&format!("\n  short_description: {short_description}"));
    }
    entry.push('\n');
    entry
}

fn skill_doc_path(location: &SkillLocation) -> &str {
    match location {
        SkillLocation::AttachedSnapshot { skill_doc_path, .. }
        | SkillLocation::AttachedWorkspace { skill_doc_path, .. } => skill_doc_path.as_str(),
    }
}

fn skill_dir_path(location: &SkillLocation) -> &str {
    match location {
        SkillLocation::AttachedSnapshot { skill_dir_path, .. }
        | SkillLocation::AttachedWorkspace { skill_dir_path, .. } => skill_dir_path.as_str(),
    }
}
