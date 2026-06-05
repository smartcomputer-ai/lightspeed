use std::{cmp::Ordering, collections::BTreeMap};

use engine::{
    BlobRef, ContextEntry, ContextEntryInput, ContextEntryKey, ContextEntryKind, CoreAgentCommand,
    CoreAgentState,
    storage::{BlobStore, BlobStoreError},
};
use serde::Serialize;
use thiserror::Error;
use vfs::VfsPath;

use crate::{
    host::fs::{FileSystem, FsError, FsPath},
    prompts::{
        PROMPT_INSTRUCTIONS_CONTEXT_KEY_PREFIX, PROMPT_INSTRUCTIONS_PROVIDER_KIND,
        PROMPT_SOURCE_FINGERPRINT_SCHEMA_VERSION, PromptAssemblyLimits, PromptInstructionsReport,
        PromptRoot, PromptRootSource, PromptSourceFingerprint, PromptSourceFingerprintInput,
        PromptSourceLocation, PromptSourceReport, PromptWarning, PromptWarningKind,
    },
};

const INSTRUCTIONS_FILE: &str = "instructions.md";
const INSTRUCTIONS_DIR: &str = "instructions.d";
const CONTEXT_KEY_MAX_LEN: usize = 128;

pub struct PromptRootInput<'a> {
    pub root: PromptRoot,
    pub fs: &'a dyn FileSystem,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptInstructionEntry {
    pub key: ContextEntryKey,
    pub source_id: String,
    pub path: String,
    pub content_ref: BlobRef,
    pub input: ContextEntryInput,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptInstructionsBuild {
    pub entries: Vec<PromptInstructionEntry>,
    pub report_ref: BlobRef,
    pub report: PromptInstructionsReport,
    pub report_bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptInstructionsPublication {
    pub build: PromptInstructionsBuild,
    pub command: Option<CoreAgentCommand>,
}

#[derive(Debug, Error)]
pub enum PromptInstructionsError {
    #[error(transparent)]
    BlobStore(#[from] BlobStoreError),

    #[error("failed to encode prompt instructions report: {message}")]
    Encode { message: String },

    #[error("invalid prompt path {path}: {message}")]
    InvalidPath { path: String, message: String },
}

pub struct PromptInstructionsBuilder<'a> {
    blobs: &'a dyn BlobStore,
    roots: Vec<PromptRootInput<'a>>,
    limits: PromptAssemblyLimits,
}

impl<'a> PromptInstructionsBuilder<'a> {
    pub fn new(blobs: &'a dyn BlobStore) -> Self {
        Self {
            blobs,
            roots: Vec::new(),
            limits: PromptAssemblyLimits::default(),
        }
    }

    pub fn with_root(mut self, root: PromptRootInput<'a>) -> Self {
        self.roots.push(root);
        self
    }

    pub fn with_limits(mut self, limits: PromptAssemblyLimits) -> Self {
        self.limits = limits;
        self
    }

    pub async fn build(self) -> Result<PromptInstructionsBuild, PromptInstructionsError> {
        build_prompt_instructions(self.blobs, &self.roots, self.limits).await
    }
}

pub async fn build_prompt_instructions(
    blobs: &dyn BlobStore,
    roots: &[PromptRootInput<'_>],
    limits: PromptAssemblyLimits,
) -> Result<PromptInstructionsBuild, PromptInstructionsError> {
    let mut sorted_roots = roots.iter().collect::<Vec<_>>();
    sorted_roots.sort_by(compare_roots);

    let mut sources = Vec::new();
    let mut warnings = Vec::new();
    let mut fingerprint_inputs = Vec::new();

    for input in sorted_roots {
        let scan = scan_root(input).await;
        sources.extend(scan.sources);
        warnings.extend(scan.warnings);
        fingerprint_inputs.push(source_input_for_root(&input.root)?);
    }

    sources.sort_by(compare_sources);
    warnings.sort_by(compare_warnings);
    fingerprint_inputs.sort_by(compare_fingerprint_inputs);

    let source_fingerprint = source_fingerprint(fingerprint_inputs)?;
    let selected = select_prompt_sources(&sources, &mut warnings, limits);
    let report = PromptInstructionsReport::new(
        source_fingerprint,
        selected.total_chars,
        selected.total_bytes,
        selected.reports,
        warnings,
    );
    let report_bytes = encode_json(&report)?;
    let report_ref = blobs.put_bytes(report_bytes.clone()).await?;
    let entries = selected
        .published
        .into_iter()
        .map(|source| {
            let input = prompt_source_instructions_context_input(
                source.content_ref.clone(),
                report_ref.clone(),
                prompt_source_preview(&source),
            );
            PromptInstructionEntry {
                key: source.context_key,
                source_id: source.id,
                path: source.path,
                content_ref: source.content_ref,
                input,
            }
        })
        .collect();

    Ok(PromptInstructionsBuild {
        entries,
        report_ref,
        report,
        report_bytes,
    })
}

pub async fn prepare_prompt_instructions_publication(
    blobs: &dyn BlobStore,
    state: &CoreAgentState,
    roots: &[PromptRootInput<'_>],
    limits: PromptAssemblyLimits,
) -> Result<PromptInstructionsPublication, PromptInstructionsError> {
    let build = build_prompt_instructions(blobs, roots, limits).await?;
    let desired = prompt_instruction_inputs_for_entries(&build.entries);
    let command = if active_prompt_instruction_inputs(state) == desired {
        None
    } else {
        Some(CoreAgentCommand::ReplaceContextPrefix {
            key_prefix: ContextEntryKey::new(PROMPT_INSTRUCTIONS_CONTEXT_KEY_PREFIX),
            entries: desired,
        })
    };

    Ok(PromptInstructionsPublication { build, command })
}

pub fn prompt_source_instructions_context_input(
    content_ref: BlobRef,
    report_ref: BlobRef,
    preview: impl Into<String>,
) -> ContextEntryInput {
    ContextEntryInput {
        kind: ContextEntryKind::Instructions,
        content_ref,
        media_type: Some("text/markdown".to_owned()),
        preview: Some(preview.into()),
        provider_kind: Some(PROMPT_INSTRUCTIONS_PROVIDER_KIND.to_owned()),
        provider_item_id: Some(report_ref.as_str().to_owned()),
        token_estimate: None,
    }
}

pub fn active_prompt_instruction_inputs(
    state: &CoreAgentState,
) -> BTreeMap<ContextEntryKey, ContextEntryInput> {
    active_prompt_instruction_entries(state)
        .into_iter()
        .filter_map(|entry| {
            let key = entry.key.clone()?;
            Some((key, context_entry_input_from_active(entry)))
        })
        .collect()
}

pub fn active_prompt_instruction_refs(state: &CoreAgentState) -> Vec<(ContextEntryKey, BlobRef)> {
    active_prompt_instruction_entries(state)
        .into_iter()
        .filter_map(|entry| Some((entry.key.clone()?, entry.content_ref.clone())))
        .collect()
}

pub fn active_prompt_instruction_entries(state: &CoreAgentState) -> Vec<&ContextEntry> {
    state
        .context
        .entries
        .iter()
        .filter(|entry| {
            entry.key.as_ref().is_some_and(is_prompt_instruction_key)
                && matches!(entry.kind, ContextEntryKind::Instructions)
        })
        .collect()
}

fn prompt_instruction_inputs_for_entries(
    entries: &[PromptInstructionEntry],
) -> BTreeMap<ContextEntryKey, ContextEntryInput> {
    entries
        .iter()
        .map(|entry| (entry.key.clone(), entry.input.clone()))
        .collect()
}

async fn scan_root(input: &PromptRootInput<'_>) -> RootScanResult {
    let mut scan = RootScan::new(&input.root);

    let instruction_path = match input.root.root_path.join(INSTRUCTIONS_FILE) {
        Ok(path) => path,
        Err(error) => {
            scan.warn(
                Some(input.root.root_path.as_str().to_owned()),
                PromptWarningKind::InvalidPath {
                    message: error.to_string(),
                },
            );
            return scan.finish();
        }
    };
    if let Some(source) = read_prompt_source(input, &instruction_path).await {
        scan.sources.push(source);
    }

    let instruction_dir = match input.root.root_path.join(INSTRUCTIONS_DIR) {
        Ok(path) => path,
        Err(error) => {
            scan.warn(
                Some(input.root.root_path.as_str().to_owned()),
                PromptWarningKind::InvalidPath {
                    message: error.to_string(),
                },
            );
            return scan.finish();
        }
    };
    let entries = match input.fs.read_directory(&instruction_dir).await {
        Ok(entries) => entries,
        Err(FsError::NotFound { .. }) => return scan.finish(),
        Err(error) => {
            scan.warn(
                Some(instruction_dir.as_str().to_owned()),
                PromptWarningKind::Filesystem {
                    message: error.to_string(),
                },
            );
            return scan.finish();
        }
    };
    let mut files = entries
        .into_iter()
        .filter(|entry| entry.is_file && entry.file_name.ends_with(".md"))
        .collect::<Vec<_>>();
    files.sort_by(|left, right| left.file_name.cmp(&right.file_name));
    for entry in files {
        match instruction_dir.join(&entry.file_name) {
            Ok(path) => {
                if let Some(source) = read_prompt_source(input, &path).await {
                    scan.sources.push(source);
                }
            }
            Err(error) => scan.warn(
                Some(format!("{}/{}", instruction_dir.as_str(), entry.file_name)),
                PromptWarningKind::InvalidPath {
                    message: error.to_string(),
                },
            ),
        }
    }

    scan.finish()
}

async fn read_prompt_source(
    input: &PromptRootInput<'_>,
    path: &FsPath,
) -> Option<ResolvedPromptSource> {
    let bytes = match input.fs.read_file(path).await {
        Ok(bytes) => bytes,
        Err(FsError::NotFound { .. }) => return None,
        Err(error) => {
            return Some(ResolvedPromptSource::warning(
                input.root.root_id.clone(),
                path.as_str().to_owned(),
                PromptWarningKind::Filesystem {
                    message: error.to_string(),
                },
            ));
        }
    };
    let text = match String::from_utf8(bytes.clone()) {
        Ok(text) => normalize_line_endings(&text),
        Err(error) => {
            return Some(ResolvedPromptSource::warning(
                input.root.root_id.clone(),
                path.as_str().to_owned(),
                PromptWarningKind::InvalidUtf8 {
                    message: error.to_string(),
                },
            ));
        }
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let content_ref = BlobRef::from_bytes(&bytes);
    let source = match source_location(&input.root, path) {
        Ok(source) => source,
        Err(error) => {
            return Some(ResolvedPromptSource::warning(
                input.root.root_id.clone(),
                path.as_str().to_owned(),
                PromptWarningKind::InvalidPath {
                    message: error.to_string(),
                },
            ));
        }
    };

    Some(ResolvedPromptSource::source(
        prompt_source_id(&input.root.root_id, path),
        input.root.root_id.clone(),
        path.as_str().to_owned(),
        source,
        content_ref,
        text,
        bytes.len() as u64,
        input.root.access.is_writable(),
    ))
}

fn select_prompt_sources(
    sources: &[ResolvedPromptSource],
    warnings: &mut Vec<PromptWarning>,
    limits: PromptAssemblyLimits,
) -> SelectedPromptSources {
    let valid_sources = sources
        .iter()
        .filter_map(|source| source.kind.as_source())
        .collect::<Vec<_>>();
    if valid_sources.is_empty() {
        return SelectedPromptSources::default();
    }

    let mut published = Vec::new();
    let mut reports = Vec::new();
    let mut used_chars = 0u32;
    let mut used_bytes = 0u64;

    for source in valid_sources {
        let original_chars = usize_to_u32(source.text.chars().count());
        let mut publish = true;
        let mut truncated = false;

        if original_chars > limits.max_source_chars {
            publish = false;
            truncated = true;
            warnings.push(PromptWarning::new(
                source.root_id.clone(),
                Some(source.path.clone()),
                PromptWarningKind::SourceTruncated {
                    max_chars: limits.max_source_chars,
                },
            ));
        }

        if publish {
            match used_chars.checked_add(original_chars) {
                Some(total) if total <= limits.max_total_chars => {}
                _ => {
                    publish = false;
                    truncated = true;
                    warnings.push(PromptWarning::new(
                        source.root_id.clone(),
                        Some(source.path.clone()),
                        PromptWarningKind::TotalLimitReached {
                            max_chars: limits.max_total_chars,
                        },
                    ));
                }
            }
        }

        let context_key = if publish {
            Some(prompt_instruction_context_key(published.len(), source))
        } else {
            None
        };
        reports.push(source_report(
            source,
            original_chars,
            publish,
            truncated,
            context_key.clone(),
        ));
        if publish {
            used_chars = used_chars.saturating_add(original_chars);
            used_bytes = used_bytes.saturating_add(source.bytes);
            published.push(PublishedPromptSource {
                id: source.id.clone(),
                path: source.path.clone(),
                content_ref: source.content_ref.clone(),
                context_key: context_key.expect("published source has context key"),
            });
        }
    }

    SelectedPromptSources {
        published,
        reports,
        total_chars: used_chars,
        total_bytes: used_bytes,
    }
}

fn source_report(
    source: &PromptSourceData,
    original_chars: u32,
    published: bool,
    truncated: bool,
    context_key: Option<ContextEntryKey>,
) -> PromptSourceReport {
    PromptSourceReport {
        id: source.id.clone(),
        root_id: source.root_id.clone(),
        path: source.path.clone(),
        published,
        context_key,
        source: source.source.clone(),
        content_ref: source.content_ref.clone(),
        chars: original_chars,
        bytes: source.bytes,
        sha256: source.content_ref.to_string(),
        truncated,
        writable: source.writable,
    }
}

fn prompt_source_preview(source: &PublishedPromptSource) -> String {
    format!("prompt instructions: {}", source.path)
}

fn source_location(
    root: &PromptRoot,
    path: &FsPath,
) -> Result<PromptSourceLocation, PromptInstructionsError> {
    let prompt_file_path = vfs_path(path)?;
    match &root.source {
        PromptRootSource::MountedSnapshot {
            snapshot_ref,
            mount_path,
        } => Ok(PromptSourceLocation::MountedSnapshot {
            source_snapshot_ref: snapshot_ref.clone(),
            source_mount_path: mount_path.clone(),
            prompt_file_path,
        }),
        PromptRootSource::MountedWorkspace {
            workspace_id,
            workspace_head_ref,
            workspace_revision,
            mount_path,
        } => Ok(PromptSourceLocation::MountedWorkspace {
            workspace_id: workspace_id.clone(),
            workspace_revision: *workspace_revision,
            workspace_head_ref: workspace_head_ref.clone(),
            source_mount_path: mount_path.clone(),
            prompt_file_path,
        }),
    }
}

fn source_input_for_root(
    root: &PromptRoot,
) -> Result<PromptSourceFingerprintInput, PromptInstructionsError> {
    let root_path = vfs_path(&root.root_path)?;
    match &root.source {
        PromptRootSource::MountedSnapshot { snapshot_ref, .. } => {
            Ok(PromptSourceFingerprintInput::SnapshotRoot {
                root_id: root.root_id.clone(),
                snapshot_ref: snapshot_ref.clone(),
                root_path,
            })
        }
        PromptRootSource::MountedWorkspace {
            workspace_id,
            workspace_head_ref,
            workspace_revision,
            ..
        } => Ok(PromptSourceFingerprintInput::WorkspaceRoot {
            root_id: root.root_id.clone(),
            workspace_id: workspace_id.clone(),
            workspace_head_ref: workspace_head_ref.clone(),
            workspace_revision: *workspace_revision,
            root_path,
        }),
    }
}

fn source_fingerprint(
    inputs: Vec<PromptSourceFingerprintInput>,
) -> Result<PromptSourceFingerprint, PromptInstructionsError> {
    let payload = SourceFingerprintPayload {
        schema_version: PROMPT_SOURCE_FINGERPRINT_SCHEMA_VERSION,
        inputs: &inputs,
    };
    let bytes = encode_json(&payload)?;
    Ok(PromptSourceFingerprint::sha256(
        BlobRef::from_bytes(&bytes),
        inputs,
    ))
}

fn encode_json<T: Serialize>(value: &T) -> Result<Vec<u8>, PromptInstructionsError> {
    serde_json::to_vec(value).map_err(|error| PromptInstructionsError::Encode {
        message: error.to_string(),
    })
}

fn vfs_path(path: &FsPath) -> Result<VfsPath, PromptInstructionsError> {
    VfsPath::parse(path.as_str()).map_err(|error| PromptInstructionsError::InvalidPath {
        path: path.as_str().to_owned(),
        message: error.to_string(),
    })
}

fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn prompt_source_id(root_id: &str, path: &FsPath) -> String {
    let file_stem = path
        .file_name()
        .unwrap_or("instructions")
        .trim_end_matches(".md");
    let suffix = sanitize_id_component(file_stem);
    if suffix == "instructions" {
        sanitize_id_component(root_id)
    } else {
        format!("{}.{}", sanitize_id_component(root_id), suffix)
    }
}

fn prompt_instruction_context_key(index: usize, source: &PromptSourceData) -> ContextEntryKey {
    let head = format!("{PROMPT_INSTRUCTIONS_CONTEXT_KEY_PREFIX}.{index:04}.");
    let path_hash = short_digest(&BlobRef::from_bytes(source.path.as_bytes()));
    let mut slug = sanitize_id_component(&source.id);
    let max_slug_len = CONTEXT_KEY_MAX_LEN
        .saturating_sub(head.len())
        .saturating_sub(path_hash.len() + 1);
    if slug.len() > max_slug_len {
        slug.truncate(max_slug_len);
    }
    if slug.is_empty() {
        slug = "prompt".to_owned();
    }
    ContextEntryKey::new(format!("{head}{slug}.{path_hash}"))
}

fn short_digest(blob_ref: &BlobRef) -> String {
    blob_ref
        .as_str()
        .strip_prefix("sha256:")
        .unwrap_or(blob_ref.as_str())
        .chars()
        .take(12)
        .collect()
}

fn sanitize_id_component(value: &str) -> String {
    let mut output = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':') {
            output.push(ch);
        } else {
            output.push('-');
        }
    }
    if output.is_empty() {
        "prompt".to_owned()
    } else {
        output
    }
}

fn usize_to_u32(value: usize) -> u32 {
    value.try_into().unwrap_or(u32::MAX)
}

fn compare_roots(left: &&PromptRootInput<'_>, right: &&PromptRootInput<'_>) -> Ordering {
    left.root.root_id.cmp(&right.root.root_id).then_with(|| {
        left.root
            .root_path
            .as_str()
            .cmp(right.root.root_path.as_str())
    })
}

fn compare_sources(left: &ResolvedPromptSource, right: &ResolvedPromptSource) -> Ordering {
    left.sort_key().cmp(&right.sort_key())
}

fn compare_warnings(left: &PromptWarning, right: &PromptWarning) -> Ordering {
    left.root_id
        .cmp(&right.root_id)
        .then_with(|| left.path.cmp(&right.path))
        .then_with(|| warning_kind_key(&left.kind).cmp(&warning_kind_key(&right.kind)))
}

fn warning_kind_key(kind: &PromptWarningKind) -> String {
    serde_json::to_string(kind).unwrap_or_else(|_| format!("{kind:?}"))
}

fn compare_fingerprint_inputs(
    left: &PromptSourceFingerprintInput,
    right: &PromptSourceFingerprintInput,
) -> Ordering {
    fingerprint_input_key(left).cmp(&fingerprint_input_key(right))
}

fn fingerprint_input_key(input: &PromptSourceFingerprintInput) -> String {
    match input {
        PromptSourceFingerprintInput::SnapshotRoot { root_id, .. }
        | PromptSourceFingerprintInput::WorkspaceRoot { root_id, .. } => root_id.clone(),
    }
}

fn context_entry_input_from_active(entry: &ContextEntry) -> ContextEntryInput {
    ContextEntryInput {
        kind: entry.kind.clone(),
        content_ref: entry.content_ref.clone(),
        media_type: entry.media_type.clone(),
        preview: entry.preview.clone(),
        provider_kind: entry.provider_kind.clone(),
        provider_item_id: entry.provider_item_id.clone(),
        token_estimate: entry.token_estimate.clone(),
    }
}

fn is_prompt_instruction_key(key: &ContextEntryKey) -> bool {
    key.as_str() == PROMPT_INSTRUCTIONS_CONTEXT_KEY_PREFIX
        || key
            .as_str()
            .strip_prefix(PROMPT_INSTRUCTIONS_CONTEXT_KEY_PREFIX)
            .is_some_and(|suffix| suffix.starts_with('.'))
}

struct RootScan {
    root_id: String,
    sources: Vec<ResolvedPromptSource>,
    warnings: Vec<PromptWarning>,
}

impl RootScan {
    fn new(root: &PromptRoot) -> Self {
        Self {
            root_id: root.root_id.clone(),
            sources: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn warn(&mut self, path: Option<String>, kind: PromptWarningKind) {
        self.warnings
            .push(PromptWarning::new(self.root_id.clone(), path, kind));
    }

    fn finish(self) -> RootScanResult {
        let mut sources = Vec::new();
        let mut warnings = self.warnings;
        for source in self.sources {
            match source.kind {
                ResolvedPromptSourceKind::Source(source) => {
                    for warning in source.warnings.iter().cloned() {
                        warnings.push(PromptWarning::new(
                            source.root_id.clone(),
                            Some(source.path.clone()),
                            warning,
                        ));
                    }
                    sources.push(ResolvedPromptSource {
                        root_id: source.root_id.clone(),
                        path: source.path.clone(),
                        kind: ResolvedPromptSourceKind::Source(source),
                    });
                }
                ResolvedPromptSourceKind::Warning { path, kind } => {
                    warnings.push(PromptWarning::new(self.root_id.clone(), Some(path), kind));
                }
            }
        }
        RootScanResult { sources, warnings }
    }
}

struct RootScanResult {
    sources: Vec<ResolvedPromptSource>,
    warnings: Vec<PromptWarning>,
}

#[derive(Clone, Debug)]
struct ResolvedPromptSource {
    root_id: String,
    path: String,
    kind: ResolvedPromptSourceKind,
}

impl ResolvedPromptSource {
    fn source(
        id: String,
        root_id: String,
        path: String,
        source: PromptSourceLocation,
        content_ref: BlobRef,
        text: String,
        bytes: u64,
        writable: bool,
    ) -> Self {
        Self {
            root_id: root_id.clone(),
            path: path.clone(),
            kind: ResolvedPromptSourceKind::Source(PromptSourceData {
                id,
                root_id,
                path,
                source,
                content_ref,
                text,
                bytes,
                writable,
                warnings: Vec::new(),
            }),
        }
    }

    fn warning(root_id: String, path: String, kind: PromptWarningKind) -> Self {
        Self {
            root_id,
            path: path.clone(),
            kind: ResolvedPromptSourceKind::Warning { path, kind },
        }
    }

    fn sort_key(&self) -> (String, u8, String) {
        (
            self.root_id.clone(),
            source_order(&self.path),
            self.path.clone(),
        )
    }
}

#[derive(Clone, Debug)]
enum ResolvedPromptSourceKind {
    Source(PromptSourceData),
    Warning {
        path: String,
        kind: PromptWarningKind,
    },
}

impl ResolvedPromptSourceKind {
    fn as_source(&self) -> Option<&PromptSourceData> {
        match self {
            Self::Source(source) => Some(source),
            Self::Warning { .. } => None,
        }
    }
}

#[derive(Clone, Debug)]
struct PromptSourceData {
    id: String,
    root_id: String,
    path: String,
    source: PromptSourceLocation,
    content_ref: BlobRef,
    text: String,
    bytes: u64,
    writable: bool,
    warnings: Vec<PromptWarningKind>,
}

#[derive(Clone, Debug)]
struct PublishedPromptSource {
    id: String,
    path: String,
    content_ref: BlobRef,
    context_key: ContextEntryKey,
}

#[derive(Clone, Debug, Default)]
struct SelectedPromptSources {
    published: Vec<PublishedPromptSource>,
    reports: Vec<PromptSourceReport>,
    total_chars: u32,
    total_bytes: u64,
}

#[derive(Serialize)]
struct SourceFingerprintPayload<'a> {
    schema_version: &'static str,
    inputs: &'a [PromptSourceFingerprintInput],
}

fn source_order(path: &str) -> u8 {
    if path.ends_with(INSTRUCTIONS_FILE) {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use engine::{
        ContextEntryId, ContextEntrySource,
        storage::{BlobStore, InMemoryBlobStore},
    };
    use vfs::VfsMountAccess;

    use super::*;
    use crate::host::fs::{CreateDirectoryOptions, InMemoryFileSystem};

    #[tokio::test]
    async fn builds_prompt_instructions_from_conventional_files() {
        let fs = prompt_fs(&[
            (
                "/workspace/.forge/prompts/instructions.md",
                "Base\r\nrules\n",
            ),
            (
                "/workspace/.forge/prompts/instructions.d/020-style.md",
                "Style rules\n",
            ),
            (
                "/workspace/.forge/prompts/instructions.d/010-safety.md",
                "Safety rules\n",
            ),
        ])
        .await;
        let blobs = Arc::new(InMemoryBlobStore::new());
        let build = build_prompt_instructions(
            blobs.as_ref(),
            &[root_input(&fs, "/workspace/.forge/prompts", "project")],
            PromptAssemblyLimits::default(),
        )
        .await
        .expect("build prompt");

        assert_eq!(build.entries.len(), 3);
        assert!(build.report.warnings.is_empty());
        assert_eq!(build.report.sources.len(), 3);
        assert!(build.report.sources.iter().all(|source| source.published));
        assert_eq!(
            build.entries[0].content_ref,
            BlobRef::from_bytes(b"Base\r\nrules\n")
        );
        assert_eq!(
            build.entries[1].content_ref,
            BlobRef::from_bytes(b"Safety rules\n")
        );
        assert_eq!(
            build.entries[2].content_ref,
            BlobRef::from_bytes(b"Style rules\n")
        );
        assert!(
            build.entries[0].key.as_str() < build.entries[1].key.as_str()
                && build.entries[1].key.as_str() < build.entries[2].key.as_str()
        );
        assert_eq!(
            blobs
                .read_bytes(&build.report_ref)
                .await
                .expect("report blob"),
            build.report_bytes
        );
    }

    #[tokio::test]
    async fn empty_prompt_roots_build_report_without_instruction_entries() {
        let fs = prompt_fs(&[]).await;
        fs.create_directory(
            &FsPath::new("/workspace/.forge/prompts").unwrap(),
            CreateDirectoryOptions::recursive(),
        )
        .await
        .expect("create root");
        let blobs = InMemoryBlobStore::new();

        let build = build_prompt_instructions(
            &blobs,
            &[root_input(&fs, "/workspace/.forge/prompts", "project")],
            PromptAssemblyLimits::default(),
        )
        .await
        .expect("build prompt");

        assert!(build.entries.is_empty());
        assert!(build.report.sources.is_empty());
    }

    #[tokio::test]
    async fn oversized_sources_are_reported_without_publishing_truncated_blobs() {
        let fs = prompt_fs(&[("/workspace/.forge/prompts/instructions.md", "abcdef")]).await;
        let blobs = InMemoryBlobStore::new();

        let build = build_prompt_instructions(
            &blobs,
            &[root_input(&fs, "/workspace/.forge/prompts", "project")],
            PromptAssemblyLimits {
                max_source_chars: 3,
                max_total_chars: 100,
            },
        )
        .await
        .expect("build prompt");

        assert!(build.entries.is_empty());
        assert!(!build.report.sources[0].published);
        assert!(build.report.sources[0].truncated);
        assert!(matches!(
            build.report.warnings[0].kind,
            PromptWarningKind::SourceTruncated { max_chars: 3 }
        ));
    }

    #[tokio::test]
    async fn publication_replaces_prompt_prefix_and_clears_missing_sources() {
        let fs = prompt_fs(&[("/workspace/.forge/prompts/instructions.md", "first")]).await;
        let blobs = InMemoryBlobStore::new();
        let publication = prepare_prompt_instructions_publication(
            &blobs,
            &CoreAgentState::new(),
            &[root_input(&fs, "/workspace/.forge/prompts", "project")],
            PromptAssemblyLimits::default(),
        )
        .await
        .expect("publication");

        let Some(CoreAgentCommand::ReplaceContextPrefix {
            key_prefix,
            entries,
        }) = publication.command
        else {
            panic!("expected prefix replacement");
        };
        assert_eq!(key_prefix.as_str(), PROMPT_INSTRUCTIONS_CONTEXT_KEY_PREFIX);
        assert_eq!(entries.len(), 1);
        let (key, entry) = entries.iter().next().expect("prompt entry");
        assert!(
            key.as_str()
                .starts_with(PROMPT_INSTRUCTIONS_CONTEXT_KEY_PREFIX)
        );
        assert!(matches!(entry.kind, ContextEntryKind::Instructions));

        let mut state = CoreAgentState::new();
        state.context.entries = vec![active_entry(key.clone(), entry.clone())];
        let empty_fs = prompt_fs(&[]).await;
        empty_fs
            .create_directory(
                &FsPath::new("/workspace/.forge/prompts").unwrap(),
                CreateDirectoryOptions::recursive(),
            )
            .await
            .expect("create root");

        let clear = prepare_prompt_instructions_publication(
            &blobs,
            &state,
            &[root_input(
                &empty_fs,
                "/workspace/.forge/prompts",
                "project",
            )],
            PromptAssemblyLimits::default(),
        )
        .await
        .expect("clear publication");

        assert!(matches!(
            clear.command,
            Some(CoreAgentCommand::ReplaceContextPrefix { ref key_prefix, ref entries })
                if key_prefix.as_str() == PROMPT_INSTRUCTIONS_CONTEXT_KEY_PREFIX
                    && entries.is_empty()
        ));
    }

    async fn prompt_fs(files: &[(&str, &str)]) -> InMemoryFileSystem {
        let fs = InMemoryFileSystem::full_access();
        for (path, text) in files {
            let path = FsPath::new(*path).unwrap();
            if let Some(parent) = path.parent() {
                fs.create_directory(&parent, CreateDirectoryOptions::recursive())
                    .await
                    .expect("create parent");
            }
            fs.write_file(&path, text.as_bytes().to_vec())
                .await
                .expect("write file");
        }
        fs
    }

    fn root_input<'a>(
        fs: &'a InMemoryFileSystem,
        root_path: &str,
        root_id: &str,
    ) -> PromptRootInput<'a> {
        PromptRootInput {
            root: PromptRoot {
                root_id: root_id.to_owned(),
                root_path: FsPath::new(root_path).unwrap(),
                source: PromptRootSource::MountedSnapshot {
                    snapshot_ref: BlobRef::from_bytes(b"snapshot-1"),
                    mount_path: VfsPath::parse("/workspace").unwrap(),
                },
                access: VfsMountAccess::ReadOnly,
            },
            fs,
        }
    }

    fn active_entry(key: ContextEntryKey, input: ContextEntryInput) -> ContextEntry {
        ContextEntry {
            entry_id: ContextEntryId::new(1),
            key: Some(key),
            kind: input.kind,
            source: ContextEntrySource::ContextEdit,
            content_ref: input.content_ref,
            media_type: input.media_type,
            preview: input.preview,
            provider_kind: input.provider_kind,
            provider_item_id: input.provider_item_id,
            token_estimate: input.token_estimate,
        }
    }
}
