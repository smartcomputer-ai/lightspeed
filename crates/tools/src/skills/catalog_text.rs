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
    let mut skills: Vec<_> = catalog.skills.iter().collect();
    skills.sort_by_key(|skill| skill_doc_path(&skill.location));
    for skill in skills {
        text.push_str(&skill_catalog_entry(skill));
    }
    text
}

fn skill_catalog_entry(skill: &SkillMetadata) -> String {
    let mut entry = format!(
        "- {}\n  description: {}\n  path: {}",
        skill.name,
        skill.description,
        skill_doc_path(&skill.location)
    );
    entry.push('\n');
    entry
}

fn skill_doc_path(location: &SkillLocation) -> &str {
    match location {
        SkillLocation::AttachedSnapshot { skill_doc_path, .. }
        | SkillLocation::AttachedWorkspace { skill_doc_path, .. } => skill_doc_path.as_str(),
    }
}
