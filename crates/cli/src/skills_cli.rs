use anyhow::{Result, anyhow, ensure};

pub(crate) fn catalog_source_label(source: &api::SkillCatalogSource) -> String {
    match source {
        api::SkillCatalogSource::Vfs => "VFS".into(),
        api::SkillCatalogSource::Environment { environment_id } => {
            format!("Environment {environment_id}")
        }
    }
}

pub(crate) fn format_skill_catalogs(response: &api::SkillListResponse) -> String {
    let mut lines = Vec::new();
    for catalog in &response.catalogs {
        lines.push(format!(
            "{} — {} (catalog {})",
            catalog_source_label(&catalog.source),
            serde_json::to_value(&catalog.availability)
                .expect("catalog availability")
                .as_str()
                .unwrap_or("unknown"),
            catalog.catalog_ref.as_deref().unwrap_or("-")
        ));
        lines.push(format!("{} skill(s)", catalog.skills.len()));
        for skill in &catalog.skills {
            let enabled = if skill.enabled { "enabled" } else { "disabled" };
            lines.push(format!("- {} [{}] {}", skill.skill_id, enabled, skill.name));
            lines.push(format!("  {}", skill.location.skill_doc_path));
            lines.push(format!("  {}", skill.description));
            if let Some(short) = &skill.short_description {
                lines.push(format!("  short {short}"));
            }
        }
        for warning in &catalog.warnings {
            lines.push(format!("  warning: {warning}"));
        }
    }
    if lines.is_empty() {
        lines.push("No skill catalogs".into());
    }
    lines.join("\n")
}

pub(crate) fn skill_selection_input(
    catalog: &api::SkillListResponse,
    skill_id: &str,
) -> Result<String> {
    let mut matches = catalog.catalogs.iter().flat_map(|catalog| {
        catalog
            .skills
            .iter()
            .filter(move |skill| skill.skill_id == skill_id)
            .map(move |skill| (catalog, skill))
    });
    let (catalog, skill) = matches
        .next()
        .ok_or_else(|| anyhow!("skill {skill_id:?} is not in the session catalog"))?;
    ensure!(
        matches.next().is_none(),
        "skill {skill_id:?} is ambiguous across catalogs"
    );
    ensure!(skill.enabled, "skill {skill_id:?} is disabled");
    ensure!(
        catalog.availability != api::SkillCatalogAvailability::Unavailable,
        "skill catalog is unavailable"
    );
    let skill_dir_path = &skill.location.skill_dir_path;
    let skill_doc_path = &skill.location.skill_doc_path;
    if let api::SkillCatalogSource::Environment { environment_id } = &catalog.source {
        return Ok(format!(
            "Use the {} skill ({}), on environment {}. Read its SKILL.md with the environment file tool at {}. Resolve supporting files relative to {} and run bundled scripts there with process tools.",
            serde_json::to_string(&skill.name)?,
            serde_json::to_string(&skill.skill_id)?,
            serde_json::to_string(environment_id)?,
            serde_json::to_string(skill_doc_path)?,
            serde_json::to_string(skill_dir_path)?,
        ));
    }
    Ok(format!(
        "Use the {} skill ({}). Read its SKILL.md through the VFS file tool at {} before following its instructions. Resolve supporting files relative to the VFS directory {}.",
        serde_json::to_string(&skill.name)?,
        serde_json::to_string(&skill.skill_id)?,
        serde_json::to_string(skill_doc_path)?,
        serde_json::to_string(skill_dir_path)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> api::SkillListResponse {
        api::SkillListResponse {
            catalogs: vec![api::SkillCatalogView {
                source: api::SkillCatalogSource::Vfs,
                availability: api::SkillCatalogAvailability::Available,
                warnings: vec![],
                catalog_ref: Some("sha256:catalog".into()),
                skills: vec![api::SkillListItem {
                    skill_id: "skill:review".into(),
                    name: "Review".into(),
                    description: "Review changes".into(),
                    short_description: None,
                    enabled: true,
                    location: api::SkillLocationView {
                        skill_dir_path: "/library/review notes".into(),
                        skill_doc_path: "/library/review notes/SKILL.md".into(),
                    },
                }],
            }],
        }
    }

    #[test]
    fn selection_instructs_an_ordinary_read_with_source_and_base_directory() {
        assert_eq!(
            skill_selection_input(&catalog(), "skill:review").unwrap(),
            "Use the \"Review\" skill (\"skill:review\"). Read its SKILL.md through the VFS file tool at \"/library/review notes/SKILL.md\" before following its instructions. Resolve supporting files relative to the VFS directory \"/library/review notes\"."
        );
    }

    #[test]
    fn selection_rejects_missing_and_disabled_skills() {
        let mut catalog = catalog();
        assert!(skill_selection_input(&catalog, "missing").is_err());
        catalog.catalogs[0].skills[0].enabled = false;
        assert!(skill_selection_input(&catalog, "skill:review").is_err());
    }

    #[test]
    fn separate_catalogs_select_their_own_reader_without_fallback() {
        let mut response = catalog();
        let mut environment = response.catalogs[0].clone();
        environment.source = api::SkillCatalogSource::Environment {
            environment_id: "machine".into(),
        };
        environment.catalog_ref = Some("sha256:environment".into());
        environment.skills[0].skill_id = "environment:review".into();
        environment.availability = api::SkillCatalogAvailability::Stale;
        environment.warnings.push("last observation".into());
        response.catalogs.push(environment);
        let rendered = format_skill_catalogs(&response);
        assert!(rendered.contains("VFS — available (catalog sha256:catalog)"));
        assert!(rendered.contains("Environment machine — stale (catalog sha256:environment)"));
        assert!(rendered.contains("warning: last observation"));
        assert_eq!(rendered.matches("[enabled] Review").count(), 2);
        assert!(
            skill_selection_input(&response, "environment:review")
                .unwrap()
                .contains("environment file tool")
        );
        assert!(
            skill_selection_input(&response, "skill:review")
                .unwrap()
                .contains("VFS file tool")
        );
        response.catalogs[1].availability = api::SkillCatalogAvailability::Unavailable;
        assert!(skill_selection_input(&response, "environment:review").is_err());
        assert!(skill_selection_input(&response, "skill:review").is_ok());
        response.catalogs[1].skills[0].skill_id = "skill:review".into();
        assert!(skill_selection_input(&response, "skill:review").is_err());
    }
}
