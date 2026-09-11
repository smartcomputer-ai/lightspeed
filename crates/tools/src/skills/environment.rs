//! Environment discovery consumes generic endpoint observations independently of VFS.
use super::{SkillId, parse_skill_frontmatter};
use engine::{
    BlobRef, ContextEntryInput, CoreAgentCommand, EnvironmentSkillsFeature,
    storage::{BlobStore, BlobStoreError},
};
use environment_protocol::{
    data::inventory::{InventoryLimits, ScanParams, ScanResponse},
    shared::EnvironmentPath,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
};

pub const ENVIRONMENT_SKILL_CATALOG_CONTEXT_KEY: &str = "runtime.catalog.skills.environment";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentSkillAvailability {
    Available,
    Stale,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentSkill {
    pub skill_id: SkillId,
    pub name: String,
    pub description: String,
    pub short_description: Option<String>,
    pub skill_dir_path: String,
    pub skill_doc_path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentSkillCatalog {
    pub catalog_id: String,
    pub environment_id: String,
    pub availability: EnvironmentSkillAvailability,
    pub skills: Vec<EnvironmentSkill>,
    /// Stable per-file parsing diagnostics; scan accounting is kept outside this snapshot.
    pub warnings: Vec<String>,
}
impl EnvironmentSkillCatalog {
    pub fn unavailable(environment_id: &str) -> Self {
        Self {
            catalog_id: "environment".into(),
            environment_id: environment_id.into(),
            availability: EnvironmentSkillAvailability::Unavailable,
            skills: vec![],
            warnings: vec![],
        }
    }
}

fn absolute(base: &Path, value: &str) -> Result<PathBuf, String> {
    let path = if Path::new(value).is_absolute() {
        PathBuf::from(value)
    } else {
        base.join(value)
    };
    let mut normalized = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err("path escapes root".into());
                }
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    if !normalized.is_absolute() {
        return Err("discovery working directory must be absolute".into());
    }
    Ok(normalized)
}

pub fn environment_skill_scan_query(
    config: &EnvironmentSkillsFeature,
    default_cwd: Option<&str>,
    home: Option<&str>,
) -> Result<ScanParams, String> {
    let cwd = config
        .working_directory
        .as_deref()
        .or(default_cwd)
        .ok_or("endpoint has no default working directory")?;
    if !Path::new(cwd).is_absolute() {
        return Err("working directory must be absolute".into());
    }
    let cwd = absolute(Path::new("/"), cwd)?;
    let boundary = match &config.project_root {
        Some(root) if Path::new(root).is_absolute() => absolute(Path::new("/"), root)?,
        Some(_) => return Err("project root must be absolute".into()),
        None => cwd.clone(),
    };
    if !cwd.starts_with(&boundary) {
        return Err("working directory is outside project root".into());
    }
    let home = home.ok_or("endpoint does not advertise execution user home directory")?;
    if !Path::new(home).is_absolute() {
        return Err("endpoint home directory must be absolute".into());
    }
    let home = absolute(Path::new("/"), home)?;
    let mut roots = BTreeSet::new();
    let mut project = cwd.as_path();
    loop {
        for suffix in [
            ".agents/skills",
            ".lightspeed/skills",
            ".claude/skills",
            ".codex/skills",
        ] {
            // Home remains limited to shared roots even when it is the working
            // directory or part of the configured project ancestry.
            if project == home && matches!(suffix, ".claude/skills" | ".codex/skills") {
                continue;
            }
            roots.insert(project.join(suffix));
        }
        if project == boundary {
            break;
        }
        project = project.parent().ok_or("invalid project ancestry")?;
    }
    for suffix in [".agents/skills", ".lightspeed/skills"] {
        roots.insert(home.join(suffix));
    }
    for root in &config.additional_roots {
        roots.insert(absolute(&cwd, root)?);
    }
    if roots.len() > 32 {
        return Err("discovery exceeds 32 roots; narrow the project boundary".into());
    }
    Ok(ScanParams {
        roots: roots
            .into_iter()
            .map(|p| EnvironmentPath::new(p.to_string_lossy()).map_err(|e| e.to_string()))
            .collect::<Result<_, _>>()?,
        include_patterns: vec!["SKILL.md".into(), "**/SKILL.md".into()],
        read_content: true,
        follow_symlinks: true,
        digest_algorithm: None,
        if_none_match: None,
        limits: InventoryLimits {
            max_entries: 4096,
            max_depth: 8,
            max_file_bytes: 64 * 1024,
            max_total_bytes: 2 * 1024 * 1024,
            max_manifest_bytes: 4 * 1024 * 1024,
            max_duration_ms: 2000,
        },
    })
}

pub fn environment_skill_catalog(
    environment_id: &str,
    scan: &ScanResponse,
) -> Result<EnvironmentSkillCatalog, String> {
    if !scan.complete || scan.unchanged {
        return Err("catalog requires a complete changed observation".into());
    }
    let mut catalog = EnvironmentSkillCatalog::unavailable(environment_id);
    catalog.availability = EnvironmentSkillAvailability::Available;
    let mut seen = BTreeSet::new();
    let mut entries: Vec<_> = scan.entries.iter().collect();
    entries.sort_by_key(|entry| entry.canonical_path.as_str());
    for entry in entries {
        if !matches!(
            entry.content,
            environment_protocol::data::inventory::ScanContent::File { .. }
        ) {
            continue;
        }
        let path = Path::new(entry.canonical_path.as_str());
        if path.file_name().and_then(|name| name.to_str()) != Some("SKILL.md") {
            continue;
        }
        let Some(dir) = path.parent() else {
            continue;
        };
        if !seen.insert(dir.to_owned()) {
            continue;
        }
        let data = entry
            .data
            .as_ref()
            .ok_or("scan omitted requested file content")?;
        let parsed = std::str::from_utf8(&data.0)
            .map_err(|error| error.to_string())
            .and_then(|text| parse_skill_frontmatter(text).map_err(|e| e.to_string()));
        match parsed {
            Ok(metadata) => {
                let identity = BlobRef::from_bytes(
                    &serde_json::to_vec(&(environment_id, dir)).expect("serialize identity"),
                );
                catalog.skills.push(EnvironmentSkill {
                    skill_id: SkillId::new(format!("environment:{}", identity.as_str())),
                    name: metadata.name,
                    description: metadata.description,
                    short_description: metadata.short_description,
                    skill_dir_path: dir.to_string_lossy().into_owned(),
                    skill_doc_path: entry.canonical_path.to_string(),
                });
            }
            Err(message) => catalog
                .warnings
                .push(format!("{}: {message}", entry.canonical_path)),
        }
    }
    Ok(catalog)
}

pub async fn publish_environment_skill_catalog(
    blobs: &dyn BlobStore,
    current: Option<&ContextEntryInput>,
    catalog: &EnvironmentSkillCatalog,
) -> Result<Option<CoreAgentCommand>, BlobStoreError> {
    let snapshot = blobs
        .put_bytes(serde_json::to_vec(catalog).expect("serialize catalog"))
        .await?;
    let mut text = format!(
        "Environment skills on {} ({:?}). Read SKILL.md with environment file tools; run bundled scripts with process tools on this environment.\n",
        catalog.environment_id, catalog.availability
    );
    if catalog.availability != EnvironmentSkillAvailability::Available {
        text.push_str("Discovery is unavailable. Listed paths are the last observation for this environment and may be stale.\n");
    }
    for skill in &catalog.skills {
        text.push_str(&format!(
            "\n- {} ({})\n  description: {}\n  skill_doc_path: {}\n  skill_dir_path: {}\n",
            skill.name,
            skill.skill_id,
            skill.description,
            skill.skill_doc_path,
            skill.skill_dir_path
        ));
    }
    let mut entry =
        crate::catalog::catalog_context_input(blobs, "Environment skills", text, snapshot).await?;
    entry.origin = Some(format!("runtime.environment:{}", catalog.environment_id));
    Ok(crate::catalog::catalog_publication_command(
        current,
        ENVIRONMENT_SKILL_CATALOG_CONTEXT_KEY,
        entry,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        fs::{CreateDirectoryOptions, FileSystem, FsPath, InMemoryFileSystem},
        skills::*,
    };
    use engine::storage::InMemoryBlobStore;
    use environment_protocol::data::inventory::{ScanContent, ScanEntry};

    #[test]
    fn scope_uses_endpoint_home_and_bounded_session_ancestry() {
        let config = EnvironmentSkillsFeature {
            working_directory: Some("/repo/src/sub".into()),
            project_root: Some("/repo".into()),
            additional_roots: vec!["../installed".into()],
        };
        let query = environment_skill_scan_query(&config, Some("/ignored"), Some("/user")).unwrap();
        let roots: Vec<_> = query.roots.iter().map(|p| p.as_str()).collect();
        assert!(roots.contains(&"/repo/.agents/skills"));
        assert!(roots.contains(&"/repo/src/sub/.agents/skills"));
        assert!(roots.contains(&"/repo/src/installed"));
        assert!(roots.contains(&"/user/.agents/skills"));
        assert!(roots.contains(&"/user/.lightspeed/skills"));
        assert!(!roots.contains(&"/user/.claude/skills"));
        assert!(!roots.contains(&"/user/.codex/skills"));
        assert!(roots.contains(&"/repo/.claude/skills"));
        assert!(roots.contains(&"/repo/.codex/skills"));
        assert!(!roots.contains(&"/.agents/skills"));
        assert!(environment_skill_scan_query(&config, None, None).is_err());
        let config = EnvironmentSkillsFeature {
            project_root: Some("/another".into()),
            ..config
        };
        assert!(environment_skill_scan_query(&config, None, Some("/user")).is_err());
    }

    #[test]
    fn home_compatibility_roots_require_explicit_configuration() {
        for cwd in ["/user", "/user/project"] {
            let mut config = EnvironmentSkillsFeature {
                working_directory: Some(cwd.into()),
                project_root: Some("/user".into()),
                ..Default::default()
            };
            let query = environment_skill_scan_query(&config, None, Some("/user")).unwrap();
            for excluded in ["/user/.claude/skills", "/user/.codex/skills"] {
                assert!(!query.roots.iter().any(|root| root.as_str() == excluded));
            }
            config.additional_roots = vec!["/user/.codex/skills".into()];
            let query = environment_skill_scan_query(&config, None, Some("/user")).unwrap();
            assert!(
                query
                    .roots
                    .iter()
                    .any(|root| root.as_str() == "/user/.codex/skills")
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn copied_skill_stays_independently_visible_in_both_catalogs() {
        let doc = b"---\nname: review\ndescription: Review changes.\n---\nbody";
        let fs = InMemoryFileSystem::full_access();
        fs.create_directory(
            &FsPath::new("/skills/review").unwrap(),
            CreateDirectoryOptions::recursive(),
        )
        .await
        .unwrap();
        fs.write_file(
            &FsPath::new("/skills/review/SKILL.md").unwrap(),
            doc.to_vec(),
        )
        .await
        .unwrap();
        let blobs = InMemoryBlobStore::new();
        let vfs = prepare_skill_catalog_publication(
            &blobs,
            None,
            None,
            &[SkillCatalogRootInput {
                fs: &fs,
                root: SkillCatalogRoot {
                    root_id: "workspace".into(),
                    root_path: FsPath::new("/skills").unwrap(),
                    source: SkillCatalogRootSource::LinkedSnapshot {
                        snapshot_ref: BlobRef::from_bytes(b"snapshot"),
                        link_path: ::vfs::VfsPath::parse("/skills").unwrap(),
                    },
                    trust: SkillTrustLevel::User,
                    scope: SkillScope::Global,
                },
            }],
        )
        .await
        .unwrap();
        let scan = ScanResponse {
            fingerprint: Some("observation".into()),
            unchanged: false,
            complete: true,
            diagnostics: vec![],
            entries: vec![ScanEntry {
                root: EnvironmentPath::new("/skills").unwrap(),
                path: "review/SKILL.md".into(),
                canonical_path: EnvironmentPath::new("/skills/review/SKILL.md").unwrap(),
                content: ScanContent::File {
                    size_bytes: doc.len() as u64,
                    executable: false,
                    digest: None,
                },
                data: Some(doc.as_slice().into()),
            }],
        };
        let environment = environment_skill_catalog("machine", &scan).unwrap();
        assert_eq!(vfs.build.catalog.skills[0].name, environment.skills[0].name);
        assert_ne!(
            vfs.build.catalog.skills[0].skill_id,
            environment.skills[0].skill_id
        );
        let Some(CoreAgentCommand::UpsertContext {
            key: vfs_key,
            entry: vfs_entry,
            ..
        }) = vfs.command
        else {
            panic!("VFS publication")
        };
        let Some(CoreAgentCommand::UpsertContext {
            key: env_key,
            entry: env_entry,
            ..
        }) = publish_environment_skill_catalog(&blobs, None, &environment)
            .await
            .unwrap()
        else {
            panic!("environment publication")
        };
        assert_eq!(vfs_key.as_str(), "runtime.catalog.skills.vfs");
        assert_eq!(env_key.as_str(), ENVIRONMENT_SKILL_CATALOG_CONTEXT_KEY);
        assert_ne!(vfs_entry.provenance_ref, env_entry.provenance_ref);
        let unavailable = EnvironmentSkillCatalog::unavailable("another-machine");
        let Some(CoreAgentCommand::UpsertContext { key, .. }) =
            publish_environment_skill_catalog(&blobs, Some(&env_entry), &unavailable)
                .await
                .unwrap()
        else {
            panic!("source switch")
        };
        assert_eq!(key, env_key);
        assert_eq!(vfs.build.catalog.skills.len(), 1);
    }
}
