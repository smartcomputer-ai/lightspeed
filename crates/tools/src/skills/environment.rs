//! Environment discovery consumes generic endpoint observations independently of VFS.
use super::{SkillId, parse_skill_frontmatter};
use environment_protocol::data::{
    fs::TreeContent,
    scan::{ScanFingerprint, ScanParams, ScanResponse, ScanRootResult, ScanSkip},
};
use harness::{
    BlobRef, ContextEntryInput, CoreAgentCommand, EnvironmentSkillsConfig,
    storage::{BlobStore, BlobStoreError},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
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
    /// The discovery root the skill was found under, as requested.
    pub root: String,
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
    /// Configured discovery scope, used only to validate last-observation fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_scope: Option<String>,
    /// Fingerprint of each scanned root's selection, by root path; a later
    /// scan sends them as `ifNoneMatch` and reuses this catalog's skills for
    /// roots that answer `unchanged`. Process-local scan accounting: it is
    /// never part of the published snapshot, so an edit that leaves every
    /// skill's metadata as it was (a body change) does not republish.
    #[serde(skip)]
    pub root_fingerprints: BTreeMap<String, String>,
}
impl EnvironmentSkillCatalog {
    pub fn unavailable(environment_id: &str) -> Self {
        Self {
            catalog_id: "environment".into(),
            environment_id: environment_id.into(),
            availability: EnvironmentSkillAvailability::Unavailable,
            skills: vec![],
            warnings: vec![],
            discovery_scope: None,
            root_fingerprints: BTreeMap::new(),
        }
    }
}

pub fn environment_skill_scan_query(
    config: &EnvironmentSkillsConfig,
    cwd: Option<&str>,
    home: Option<&str>,
) -> Result<ScanParams, String> {
    crate::environment::sources::scan_query(
        config.roots.as_deref(),
        cwd.ok_or("endpoint has no default working directory")?,
        home,
        "skills",
    )
}

/// Send `previous`'s per-root fingerprints, so unchanged roots answer
/// without entries.
pub fn apply_cached_fingerprints(query: &mut ScanParams, previous: &EnvironmentSkillCatalog) {
    for root in &mut query.roots {
        root.if_none_match = previous
            .root_fingerprints
            .get(root.path.as_str())
            .and_then(|fingerprint| ScanFingerprint::new(fingerprint.clone()).ok());
    }
}

/// Build the catalog from per-root scan results. A root that failed is
/// reported in `warnings` by its path and contributes nothing; every other
/// root still contributes its skills. An `unchanged` root reuses the skills
/// `previous` recorded for it, so it requires that catalog.
pub fn environment_skill_catalog(
    environment_id: &str,
    scan: &ScanResponse,
    previous: Option<&EnvironmentSkillCatalog>,
) -> Result<EnvironmentSkillCatalog, String> {
    let mut catalog = EnvironmentSkillCatalog::unavailable(environment_id);
    catalog.availability = EnvironmentSkillAvailability::Available;
    let mut documents = Vec::new();
    for root in &scan.roots {
        match root {
            ScanRootResult::Complete {
                path,
                canonical_path,
                fingerprint,
                entries,
                skipped,
                skipped_entry_count,
            } => {
                catalog
                    .root_fingerprints
                    .insert(path.to_string(), fingerprint.as_str().to_owned());
                let base = canonical_path.as_ref().unwrap_or(path);
                for entry in entries {
                    if matches!(entry.content, TreeContent::File { .. })
                        && entry.path.segments().last() == Some("SKILL.md")
                    {
                        let doc = base.join(&entry.path).map_err(|error| error.to_string())?;
                        documents.push((path.to_string(), doc.to_string(), entry.data.clone()));
                    }
                }
                if *skipped_entry_count > 0 {
                    catalog.warnings.push(skipped_warning(
                        path.as_str(),
                        skipped,
                        *skipped_entry_count,
                    ));
                }
            }
            ScanRootResult::Unchanged { path, fingerprint } => {
                let previous = previous
                    .filter(|previous| {
                        previous.root_fingerprints.get(path.as_str())
                            == Some(&fingerprint.as_str().to_owned())
                    })
                    .ok_or_else(|| {
                        format!(
                            "skill root {path} is unchanged but no matching observation is cached"
                        )
                    })?;
                catalog
                    .root_fingerprints
                    .insert(path.to_string(), fingerprint.as_str().to_owned());
                catalog.skills.extend(
                    previous
                        .skills
                        .iter()
                        .filter(|skill| skill.root == path.as_str())
                        .cloned(),
                );
                catalog.warnings.extend(
                    previous
                        .warnings
                        .iter()
                        .filter(|warning| warning.starts_with(&format!("{path}:")))
                        .cloned(),
                );
            }
            ScanRootResult::Missing { .. } => {}
            ScanRootResult::Failed { path, error } => catalog
                .warnings
                .push(format!("{path}: skill root could not be scanned: {error}")),
        }
    }
    documents.sort_by(|left, right| left.1.cmp(&right.1));
    let mut seen = BTreeSet::new();
    for (root, doc, data) in documents {
        let path = Path::new(&doc);
        let Some(dir) = path.parent() else {
            continue;
        };
        if !seen.insert(dir.to_owned()) {
            continue;
        }
        let Some(data) = data else {
            catalog
                .warnings
                .push(format!("{root}: scan omitted the content of {doc}"));
            continue;
        };
        let parsed = std::str::from_utf8(data.as_slice())
            .map_err(|error| error.to_string())
            .and_then(|text| parse_skill_frontmatter(text).map_err(|e| e.to_string()));
        match parsed {
            Ok(metadata) => {
                let identity = BlobRef::from_bytes(
                    &serde_json::to_vec(&(environment_id, dir)).expect("serialize identity"),
                );
                catalog.skills.push(EnvironmentSkill {
                    root,
                    skill_id: SkillId::new(format!("environment:{}", identity.as_str())),
                    name: metadata.name,
                    description: metadata.description,
                    short_description: metadata.short_description,
                    skill_dir_path: dir.to_string_lossy().into_owned(),
                    skill_doc_path: doc,
                });
            }
            Err(message) => catalog.warnings.push(format!("{root}: {doc}: {message}")),
        }
    }
    catalog
        .skills
        .sort_by(|left, right| left.skill_doc_path.cmp(&right.skill_doc_path));
    Ok(catalog)
}

fn skipped_warning(root: &str, skipped: &[ScanSkip], total: u32) -> String {
    let listed = skipped
        .iter()
        .filter_map(|skip| {
            skip.path
                .as_ref()
                .map(|path| format!("{path} ({:?})", skip.reason))
        })
        .take(5)
        .collect::<Vec<_>>();
    if listed.is_empty() {
        format!("{root}: {total} entries skipped")
    } else {
        format!(
            "{root}: {total} entries skipped, such as {}",
            listed.join(", ")
        )
    }
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
        "Skills discovered on {}. Read SKILL.md with environment file tools; run bundled scripts with process tools on this environment. Paths reflect the last discovery; file tools read their current contents.\n",
        crate::environment::handles::environment_handle(&catalog.environment_id)
    );
    if catalog.skills.is_empty() {
        text.push_str("No skills have been discovered.\n");
    }
    let mut skills: Vec<_> = catalog.skills.iter().collect();
    skills.sort_by_key(|skill| &skill.skill_doc_path);
    for skill in skills {
        text.push_str(&format!(
            "\n- {}\n  description: {}\n  path: {}\n",
            skill.name, skill.description, skill.skill_doc_path,
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
    use environment_protocol::{
        data::scan::{ScanEntry, ScanSkipReason},
        error::ProtocolError,
        paths::{EnvironmentPath, TreePath},
    };
    use harness::storage::InMemoryBlobStore;

    fn fingerprint(value: &str) -> ScanFingerprint {
        ScanFingerprint::new(format!("scan:v1:{value}")).unwrap()
    }

    fn skill_entry(path: &str, doc: &[u8]) -> ScanEntry {
        ScanEntry {
            path: TreePath::new(path).unwrap(),
            content: TreeContent::File {
                size_bytes: doc.len() as u64,
                executable: false,
                digest: None,
            },
            data: Some(doc.to_vec().into()),
        }
    }

    fn complete(root: &str, entries: Vec<ScanEntry>, skipped: Vec<ScanSkip>) -> ScanRootResult {
        ScanRootResult::Complete {
            path: EnvironmentPath::new(root).unwrap(),
            canonical_path: None,
            fingerprint: fingerprint(root),
            skipped_entry_count: skipped.len() as u32,
            entries,
            skipped,
        }
    }

    #[test]
    fn one_bad_root_or_entry_never_hides_the_other_skills() {
        let doc = b"---\nname: review\ndescription: Review changes.\n---\nbody";
        let scan = ScanResponse {
            roots: vec![
                complete(
                    "/project/.agents/skills",
                    vec![skill_entry("review/SKILL.md", doc)],
                    vec![ScanSkip {
                        path: Some(TreePath::new("huge/SKILL.md").unwrap()),
                        reason: ScanSkipReason::FileTooLarge,
                    }],
                ),
                ScanRootResult::Failed {
                    path: EnvironmentPath::new("/user/.agents/skills").unwrap(),
                    error: ProtocolError::deadline_exceeded("walk took too long"),
                },
                ScanRootResult::Missing {
                    path: EnvironmentPath::new("/user/.lightspeed/skills").unwrap(),
                },
            ],
        };
        let catalog = environment_skill_catalog("machine", &scan, None).unwrap();
        assert_eq!(
            catalog.availability,
            EnvironmentSkillAvailability::Available
        );
        assert_eq!(catalog.skills.len(), 1);
        assert_eq!(
            catalog.skills[0].skill_doc_path,
            "/project/.agents/skills/review/SKILL.md"
        );
        assert!(catalog.warnings.iter().any(|warning| {
            warning.starts_with("/project/.agents/skills:") && warning.contains("huge/SKILL.md")
        }));
        assert!(
            catalog
                .warnings
                .iter()
                .any(|warning| warning.starts_with("/user/.agents/skills:"))
        );

        let mut query = environment_skill_scan_query(
            &EnvironmentSkillsConfig::default(),
            Some("/project"),
            Some("/user"),
        )
        .unwrap();
        apply_cached_fingerprints(&mut query, &catalog);
        assert_eq!(
            query.roots[0].if_none_match,
            Some(fingerprint("/project/.agents/skills"))
        );
        let rescan = ScanResponse {
            roots: vec![ScanRootResult::Unchanged {
                path: EnvironmentPath::new("/project/.agents/skills").unwrap(),
                fingerprint: fingerprint("/project/.agents/skills"),
            }],
        };
        let reused = environment_skill_catalog("machine", &rescan, Some(&catalog)).unwrap();
        assert_eq!(reused.skills, catalog.skills);
        assert!(environment_skill_catalog("machine", &rescan, None).is_err());
    }

    #[test]
    fn scope_defaults_and_overrides_are_independent_of_home() {
        let config = EnvironmentSkillsConfig::default();
        let query = environment_skill_scan_query(&config, Some("/project"), Some("/user")).unwrap();
        assert_eq!(
            query
                .roots
                .iter()
                .map(|p| p.path.as_str())
                .collect::<Vec<_>>(),
            vec![
                "/project/.agents/skills",
                "/project/.lightspeed/skills",
                "/user/.agents/skills",
                "/user/.lightspeed/skills"
            ]
        );
        let config = EnvironmentSkillsConfig {
            roots: Some(vec!["./custom".into(), "/user/.codex/skills".into()]),
        };
        let query = environment_skill_scan_query(&config, Some("/project"), None).unwrap();
        assert_eq!(
            query
                .roots
                .iter()
                .map(|p| p.path.as_str())
                .collect::<Vec<_>>(),
            vec!["/project/custom", "/user/.codex/skills"]
        );
        assert_eq!(
            environment_skill_scan_query(
                &EnvironmentSkillsConfig::default(),
                Some("/user"),
                Some("/user")
            )
            .unwrap()
            .roots
            .len(),
            2
        );
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
                    source: SkillCatalogRootSource::AttachedSnapshot {
                        snapshot_ref: BlobRef::from_bytes(b"snapshot"),
                        attachment_path: ::vfs::VfsPath::parse("/skills").unwrap(),
                    },
                    trust: SkillTrustLevel::User,
                    scope: SkillScope::Global,
                },
            }],
        )
        .await
        .unwrap();
        let scan = ScanResponse {
            roots: vec![complete(
                "/skills",
                vec![skill_entry("review/SKILL.md", doc)],
                vec![],
            )],
        };
        let environment = environment_skill_catalog("machine", &scan, None).unwrap();
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
