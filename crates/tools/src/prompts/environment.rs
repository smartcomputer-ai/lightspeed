//! Bounded environment prompt assembly from a complete filesystem observation.
use engine::{
    ContentRef, ContextEntryInput, ContextEntryKey, ContextEntryKind,
    storage::{BlobStore, BlobStoreError},
};
use environment_protocol::data::inventory::ScanResponse;
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

pub fn assemble(scan: &ScanResponse) -> Result<(String, Vec<String>), String> {
    if !scan.complete || scan.unchanged {
        return Err("prompts require a complete changed observation".into());
    }
    let mut seen = BTreeSet::new();
    let mut entries: Vec<_> = scan.entries.iter().collect();
    entries.sort_by(|a, b| {
        (a.root.as_str(), a.path != "instructions.md", &a.path).cmp(&(
            b.root.as_str(),
            b.path != "instructions.md",
            &b.path,
        ))
    });
    let mut text = String::new();
    let mut paths = Vec::new();
    for entry in entries {
        let valid = entry.path == "instructions.md"
            || entry
                .path
                .strip_prefix("instructions.d/")
                .is_some_and(|name| !name.contains('/') && name.ends_with(".md"));
        if !valid || !seen.insert(entry.canonical_path.as_str()) {
            continue;
        }
        let data = entry
            .data
            .as_ref()
            .ok_or("prompt scan omitted file content")?
            .0
            .clone();
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
            paths.push(entry.canonical_path.to_string());
        }
    }
    Ok((text, paths))
}

pub async fn publication(
    blobs: &dyn BlobStore,
    id: &str,
    observation: Result<(String, Vec<String>), String>,
) -> Result<BTreeMap<ContextEntryKey, ContextEntryInput>, BlobStoreError> {
    let (text, paths, warnings, available) = match observation {
        Ok((text, paths)) => (text, paths, vec![], true),
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
