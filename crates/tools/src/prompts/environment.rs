//! Bounded environment prompt assembly from a filesystem observation.
use environment_protocol::data::scan::{ScanResponse, ScanRootResult};
use harness::{
    ContentRef, ContextEntryInput, ContextEntryKey, ContextEntryKind,
    storage::{BlobStore, BlobStoreError},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const ENVIRONMENT_PROMPT_CONTEXT_KEY: &str = "instructions.110.environment";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvironmentPromptReport {
    pub environment_id: String,
    pub available: bool,
    pub paths: Vec<String>,
    pub warnings: Vec<String>,
}

/// Prompts found by one scan, in root then name order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PromptObservation {
    pub text: String,
    pub paths: Vec<String>,
    /// Roots that could not be scanned, by path; the other roots' prompts
    /// are still assembled.
    pub warnings: Vec<String>,
}

pub fn assemble(scan: &ScanResponse) -> Result<PromptObservation, String> {
    let mut seen = BTreeSet::new();
    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    for root in &scan.roots {
        match root {
            ScanRootResult::Complete {
                path,
                canonical_path,
                entries: root_entries,
                ..
            } => {
                let base = canonical_path.as_ref().unwrap_or(path);
                for entry in root_entries {
                    entries.push((path.as_str(), entry, base));
                }
            }
            ScanRootResult::Failed { path, error } => {
                warnings.push(format!("{path}: prompt root could not be scanned: {error}"));
            }
            ScanRootResult::Unchanged { path, .. } => {
                return Err(format!(
                    "prompt root {path} answered unchanged; prompts need a full observation"
                ));
            }
            ScanRootResult::Missing { .. } => {}
        }
    }
    entries.sort_by(|a, b| (a.0, a.1.path.as_str()).cmp(&(b.0, b.1.path.as_str())));
    let mut text = String::new();
    let mut paths = Vec::new();
    for (_, entry, base) in entries {
        let name = entry.path.as_str();
        let valid =
            entry.path.segments().count() == 1 && (name.ends_with(".md") || name.ends_with(".txt"));
        let canonical = base.join(&entry.path).map_err(|e| e.to_string())?;
        if !valid || !seen.insert(canonical.to_string()) {
            continue;
        }
        let data = entry
            .data
            .as_ref()
            .ok_or("prompt scan omitted file content")?
            .as_slice()
            .to_vec();
        let body = String::from_utf8(data).map_err(|e| e.to_string())?;
        if body.chars().count() > 64 * 1024
            || text.chars().count() + body.chars().count() > 256 * 1024
        {
            return Err("environment prompt size limit exceeded".into());
        }
        if !body.trim().is_empty() {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&body);
            paths.push(canonical.to_string());
        }
    }
    Ok(PromptObservation {
        text,
        paths,
        warnings,
    })
}

pub async fn publication(
    blobs: &dyn BlobStore,
    id: &str,
    observation: Result<PromptObservation, String>,
) -> Result<BTreeMap<ContextEntryKey, ContextEntryInput>, BlobStoreError> {
    let (text, paths, warnings, available) = match observation {
        Ok(observation) => (
            observation.text,
            observation.paths,
            observation.warnings,
            true,
        ),
        Err(error) => (
            format!("Environment prompt sources are unavailable: {error}"),
            vec![],
            vec![error],
            false,
        ),
    };
    let mut result = BTreeMap::new();
    // Empty successful observations clear this source; failures publish an explicit diagnostic.
    if text.is_empty() {
        return Ok(result);
    }
    let report = EnvironmentPromptReport {
        environment_id: id.into(),
        available,
        paths,
        warnings,
    };
    let provenance_ref = blobs
        .put_bytes(serde_json::to_vec(&report).expect("serialize report"))
        .await?;
    let content_ref = blobs.put_bytes(text.into_bytes()).await?;
    result.insert(
        ContextEntryKey::new(ENVIRONMENT_PROMPT_CONTEXT_KEY),
        ContextEntryInput {
            kind: ContextEntryKind::Instructions,
            content: ContentRef::text(content_ref),
            origin: Some(format!("runtime.environment:{id}")),
            provenance_ref: Some(provenance_ref),
            preview: Some(
                if available {
                    "Environment prompts"
                } else {
                    "Environment prompts unavailable"
                }
                .into(),
            ),
            token_estimate: None,
        },
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use environment_protocol::{
        data::{
            fs::TreeContent,
            scan::{ScanEntry, ScanFingerprint},
        },
        error::ProtocolError,
        paths::{EnvironmentPath, TreePath},
    };

    use super::*;

    fn prompt(path: &str, text: &str) -> ScanEntry {
        ScanEntry {
            path: TreePath::new(path).unwrap(),
            content: TreeContent::File {
                size_bytes: text.len() as u64,
                executable: false,
                digest: None,
            },
            data: Some(text.as_bytes().to_vec().into()),
        }
    }

    #[test]
    fn a_failed_root_is_reported_and_never_hides_the_others() {
        let scan = ScanResponse {
            roots: vec![
                ScanRootResult::Failed {
                    path: EnvironmentPath::new("/home/.agents/prompts").unwrap(),
                    error: ProtocolError::deadline_exceeded("slow"),
                },
                ScanRootResult::Complete {
                    path: EnvironmentPath::new("/project/.agents/prompts").unwrap(),
                    canonical_path: None,
                    fingerprint: ScanFingerprint::new("scan:v1:x").unwrap(),
                    entries: vec![prompt("b.md", "second"), prompt("a.md", "first")],
                    skipped: vec![],
                    skipped_entry_count: 0,
                },
            ],
        };
        let observation = assemble(&scan).unwrap();
        assert_eq!(observation.text, "first\n\nsecond");
        assert_eq!(
            observation.paths,
            vec![
                "/project/.agents/prompts/a.md",
                "/project/.agents/prompts/b.md"
            ]
        );
        assert!(observation.warnings[0].starts_with("/home/.agents/prompts:"));
    }
}
