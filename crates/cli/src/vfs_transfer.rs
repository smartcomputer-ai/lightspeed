use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use api::{
    AgentApiError, BlobHasItem, BlobHasParams, BlobPutItem, BlobPutParams, BlobPutResult,
    BlobReadParams, BlobReadResponse, VfsSnapshotCommitParams, VfsSnapshotCommitResponse,
    VfsSnapshotReadParams, VfsSnapshotReadResponse,
};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use engine::BlobRef;
use serde::Serialize;
use vfs::{
    VfsDirectory, VfsEntry, VfsFile, VfsPath, VfsSnapshotLimits, VfsSnapshotManifest,
    create_manifest_directory, write_manifest_file_ref,
};

use crate::api_client::HttpAgentApi;

pub(crate) const DEFAULT_PUT_MANY_MAX_BATCH_BYTES: u64 = 32 * 1024 * 1024;
pub(crate) const DEFAULT_PUT_MANY_MAX_BATCH_FILES: usize = 128;
pub(crate) const DEFAULT_HAS_MANY_MAX_REFS: usize = 1_000;
pub(crate) const DEFAULT_MAX_FILES: u64 = 10_000;
pub(crate) const DEFAULT_MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
pub(crate) const DEFAULT_MAX_SINGLE_FILE_BYTES: u64 = DEFAULT_PUT_MANY_MAX_BATCH_BYTES;
pub(crate) const DEFAULT_MAX_DEPTH: usize = 64;

#[derive(Clone, Debug)]
pub(crate) struct SnapshotUploadOptions {
    pub limits: VfsSnapshotLimits,
    pub max_put_many_batch_bytes: u64,
    pub max_put_many_batch_files: usize,
    pub max_has_many_refs: usize,
}

impl Default for SnapshotUploadOptions {
    fn default() -> Self {
        Self {
            limits: VfsSnapshotLimits::new(
                DEFAULT_MAX_FILES,
                DEFAULT_MAX_TOTAL_BYTES,
                DEFAULT_MAX_SINGLE_FILE_BYTES,
                DEFAULT_MAX_DEPTH,
            ),
            max_put_many_batch_bytes: DEFAULT_PUT_MANY_MAX_BATCH_BYTES,
            max_put_many_batch_files: DEFAULT_PUT_MANY_MAX_BATCH_FILES,
            max_has_many_refs: DEFAULT_HAS_MANY_MAX_REFS,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SnapshotUploadSummary {
    pub root: String,
    pub snapshot_ref: String,
    pub files: u64,
    pub bytes: u64,
    pub uploaded_blobs: u64,
    pub uploaded_bytes: u64,
    pub reused_blobs: u64,
    pub reused_bytes: u64,
    pub skipped_paths: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<SnapshotWarning>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SnapshotMaterializeSummary {
    pub destination: String,
    pub snapshot_ref: String,
    pub files: u64,
    pub bytes: u64,
    pub directories: u64,
    pub created_directories: u64,
    pub written_files: u64,
    pub written_bytes: u64,
    pub reused_files: u64,
    pub reused_bytes: u64,
    pub downloaded_blobs: u64,
    pub downloaded_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SnapshotWarning {
    pub path: String,
    pub message: String,
}

#[derive(Clone, Debug)]
pub(crate) struct SnapshotUploadPlan {
    pub root: String,
    pub directories: Vec<VfsPath>,
    pub files: Vec<SnapshotUploadEntry>,
    pub warnings: Vec<SnapshotWarning>,
}

#[derive(Clone, Debug)]
pub(crate) struct SnapshotUploadEntry {
    pub vfs_path: VfsPath,
    pub source: SnapshotEntrySource,
    pub blob_ref: BlobRef,
    pub size_bytes: u64,
    pub media_type: Option<String>,
    pub executable: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum SnapshotEntrySource {
    HostPath(PathBuf),
    #[allow(dead_code)]
    Bytes(Vec<u8>),
}

#[async_trait]
pub(crate) trait CasVfsApi {
    async fn has_blobs(&self, blob_refs: Vec<String>) -> Result<Vec<BlobHasItem>, AgentApiError>;

    async fn put_blobs(&self, blobs: Vec<Vec<u8>>) -> Result<Vec<BlobPutResult>, AgentApiError>;

    async fn get_blob(&self, blob_ref: String) -> Result<BlobReadResponse, AgentApiError>;

    async fn commit_vfs_snapshot(
        &self,
        manifest: &VfsSnapshotManifest,
    ) -> Result<VfsSnapshotCommitResponse, AgentApiError>;

    async fn read_vfs_snapshot(
        &self,
        snapshot_ref: String,
    ) -> Result<VfsSnapshotReadResponse, AgentApiError>;
}

#[async_trait]
impl CasVfsApi for HttpAgentApi {
    async fn has_blobs(&self, blob_refs: Vec<String>) -> Result<Vec<BlobHasItem>, AgentApiError> {
        Ok(HttpAgentApi::has_blobs(self, BlobHasParams { blob_refs })
            .await?
            .result
            .blobs)
    }

    async fn put_blobs(&self, blobs: Vec<Vec<u8>>) -> Result<Vec<BlobPutResult>, AgentApiError> {
        let blobs = blobs
            .into_iter()
            .map(|bytes| BlobPutItem {
                bytes_base64: BASE64.encode(bytes),
            })
            .collect();
        Ok(HttpAgentApi::put_blobs(self, BlobPutParams { blobs })
            .await?
            .result
            .blobs)
    }

    async fn get_blob(&self, blob_ref: String) -> Result<BlobReadResponse, AgentApiError> {
        Ok(HttpAgentApi::read_blob(self, BlobReadParams { blob_ref })
            .await?
            .result)
    }

    async fn commit_vfs_snapshot(
        &self,
        manifest: &VfsSnapshotManifest,
    ) -> Result<VfsSnapshotCommitResponse, AgentApiError> {
        let manifest = serde_json::to_value(manifest).map_err(|error| {
            AgentApiError::invalid_request(format!("failed to encode manifest: {error}"))
        })?;
        Ok(
            HttpAgentApi::commit_vfs_snapshot(self, VfsSnapshotCommitParams { manifest })
                .await?
                .result,
        )
    }

    async fn read_vfs_snapshot(
        &self,
        snapshot_ref: String,
    ) -> Result<VfsSnapshotReadResponse, AgentApiError> {
        Ok(
            HttpAgentApi::read_vfs_snapshot(self, VfsSnapshotReadParams { snapshot_ref })
                .await?
                .result,
        )
    }
}

pub(crate) fn scan_snapshot_directory(
    root: impl AsRef<Path>,
    limits: VfsSnapshotLimits,
) -> Result<SnapshotUploadPlan> {
    let root = root.as_ref();
    let metadata = fs::symlink_metadata(root)
        .with_context(|| format!("failed to inspect snapshot root {}", root.display()))?;
    if metadata.file_type().is_symlink() {
        bail!(
            "snapshot root must be a directory, not a symlink: {}",
            root.display()
        );
    }
    if !metadata.is_dir() {
        bail!("snapshot root must be a directory: {}", root.display());
    }
    let root = fs::canonicalize(root)
        .with_context(|| format!("failed to canonicalize snapshot root {}", root.display()))?;

    let mut scanner = DirectoryScanner {
        limits,
        plan: SnapshotUploadPlan {
            root: root.display().to_string(),
            directories: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        },
        file_count: 0,
        total_bytes: 0,
    };
    scanner.scan_dir(&root, Vec::new())?;
    Ok(scanner.plan)
}

pub(crate) async fn upload_snapshot_plan(
    api: &(impl CasVfsApi + Sync),
    mut plan: SnapshotUploadPlan,
    options: SnapshotUploadOptions,
) -> Result<SnapshotUploadSummary> {
    plan.directories.sort();
    plan.directories.dedup();
    plan.files
        .sort_by(|left, right| left.vfs_path.cmp(&right.vfs_path));

    let unique_sizes = unique_blob_sizes(&plan.files);
    let existing_refs = existing_blob_refs(api, unique_sizes.keys().cloned().collect(), &options)
        .await
        .context("failed to check existing CAS blobs")?;
    let missing_refs = unique_sizes
        .keys()
        .filter(|blob_ref| !existing_refs.contains(*blob_ref))
        .cloned()
        .collect::<BTreeSet<_>>();

    let upload_stats = upload_missing_blobs(api, &plan.files, &missing_refs, &options)
        .await
        .context("failed to upload missing CAS blobs")?;
    let manifest = manifest_from_plan(&plan)?;
    let commit = api
        .commit_vfs_snapshot(&manifest)
        .await
        .map_err(crate::api_client::api_error)
        .context("failed to commit VFS snapshot manifest")?;

    if commit.files != manifest.totals.files || commit.bytes != manifest.totals.bytes {
        bail!(
            "gateway committed snapshot totals do not match manifest: gateway files={} bytes={}, local files={} bytes={}",
            commit.files,
            commit.bytes,
            manifest.totals.files,
            manifest.totals.bytes
        );
    }

    let reused = unique_sizes
        .iter()
        .filter(|(blob_ref, _)| existing_refs.contains(*blob_ref))
        .fold((0u64, 0u64), |(count, bytes), (_, byte_len)| {
            (count + 1, bytes + *byte_len)
        });

    Ok(SnapshotUploadSummary {
        root: plan.root,
        snapshot_ref: commit.snapshot_ref,
        files: manifest.totals.files,
        bytes: manifest.totals.bytes,
        uploaded_blobs: upload_stats.blobs,
        uploaded_bytes: upload_stats.bytes,
        reused_blobs: reused.0,
        reused_bytes: reused.1,
        skipped_paths: plan.warnings.len() as u64,
        warnings: plan.warnings,
    })
}

pub(crate) async fn upload_snapshot_directory(
    api: &(impl CasVfsApi + Sync),
    root: impl AsRef<Path>,
    options: SnapshotUploadOptions,
) -> Result<SnapshotUploadSummary> {
    let plan = scan_snapshot_directory(root, options.limits)?;
    upload_snapshot_plan(api, plan, options).await
}

pub(crate) async fn materialize_snapshot(
    api: &(impl CasVfsApi + Sync),
    snapshot_ref: impl Into<String>,
    destination: impl AsRef<Path>,
) -> Result<SnapshotMaterializeSummary> {
    let snapshot_ref = snapshot_ref.into();
    let destination = prepare_materialize_destination(destination.as_ref())?;
    let read = api
        .read_vfs_snapshot(snapshot_ref.clone())
        .await
        .map_err(crate::api_client::api_error)
        .context("failed to read VFS snapshot manifest")?;
    if read.snapshot_ref != snapshot_ref {
        bail!(
            "gateway returned snapshot ref {} for requested {}",
            read.snapshot_ref,
            snapshot_ref
        );
    }
    let manifest: VfsSnapshotManifest = serde_json::from_value(read.manifest)
        .context("gateway returned invalid VFS snapshot manifest JSON")?;
    manifest.validate()?;

    let mut directories = Vec::new();
    let mut files = Vec::new();
    collect_materialize_entries(&manifest.root, Vec::new(), &mut directories, &mut files)?;

    let mut materializer = LocalMaterializer {
        api,
        destination: destination.clone(),
        blob_cache: BTreeMap::new(),
        summary: SnapshotMaterializeSummary {
            destination: destination.display().to_string(),
            snapshot_ref,
            files: manifest.totals.files,
            bytes: manifest.totals.bytes,
            directories: directories.len() as u64,
            created_directories: 0,
            written_files: 0,
            written_bytes: 0,
            reused_files: 0,
            reused_bytes: 0,
            downloaded_blobs: 0,
            downloaded_bytes: 0,
        },
    };

    for directory in directories {
        materializer.ensure_directory(&directory)?;
    }
    for file in files {
        materializer.materialize_file(file).await?;
    }

    Ok(materializer.summary)
}

fn unique_blob_sizes(files: &[SnapshotUploadEntry]) -> BTreeMap<String, u64> {
    let mut sizes = BTreeMap::new();
    for file in files {
        sizes
            .entry(file.blob_ref.as_str().to_owned())
            .or_insert(file.size_bytes);
    }
    sizes
}

async fn existing_blob_refs(
    api: &(impl CasVfsApi + Sync),
    blob_refs: Vec<String>,
    options: &SnapshotUploadOptions,
) -> Result<BTreeSet<String>> {
    let mut existing = BTreeSet::new();
    let chunk_size = options.max_has_many_refs.max(1);
    for chunk in blob_refs.chunks(chunk_size) {
        let requested = chunk.to_vec();
        let response = api
            .has_blobs(requested.clone())
            .await
            .map_err(crate::api_client::api_error)?;
        if response.len() != requested.len() {
            bail!(
                "gateway returned {} blob existence entries for {} requested refs",
                response.len(),
                requested.len()
            );
        }
        for item in response {
            if item.exists {
                existing.insert(item.blob_ref);
            }
        }
    }
    Ok(existing)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct UploadStats {
    blobs: u64,
    bytes: u64,
}

async fn upload_missing_blobs(
    api: &(impl CasVfsApi + Sync),
    files: &[SnapshotUploadEntry],
    missing_refs: &BTreeSet<String>,
    options: &SnapshotUploadOptions,
) -> Result<UploadStats> {
    let mut seen = BTreeSet::new();
    let mut batch = Vec::new();
    let mut expected = Vec::new();
    let mut batch_bytes = 0u64;
    let mut stats = UploadStats::default();

    for file in files {
        let blob_ref = file.blob_ref.as_str().to_owned();
        if !missing_refs.contains(&blob_ref) || !seen.insert(blob_ref.clone()) {
            continue;
        }
        let bytes = read_source_bytes(file)
            .with_context(|| format!("failed to read {}", file.source.display()))?;
        let actual_ref = BlobRef::from_bytes(&bytes);
        if actual_ref != file.blob_ref || bytes.len() as u64 != file.size_bytes {
            bail!(
                "file changed while snapshotting {}: expected {} bytes at {}, got {} bytes at {}",
                file.source.display(),
                file.size_bytes,
                file.blob_ref,
                bytes.len(),
                actual_ref
            );
        }
        if bytes.len() as u64 > options.max_put_many_batch_bytes {
            bail!(
                "blob {} is {} bytes, larger than blobs/put batch limit {}; raise --put-batch-bytes and the gateway request body limit or lower --max-file-bytes",
                file.blob_ref,
                bytes.len(),
                options.max_put_many_batch_bytes
            );
        }
        let would_exceed_bytes =
            batch_bytes > 0 && batch_bytes + bytes.len() as u64 > options.max_put_many_batch_bytes;
        let would_exceed_files = batch.len() >= options.max_put_many_batch_files.max(1);
        if would_exceed_bytes || would_exceed_files {
            stats = add_upload_stats(stats, flush_put_many(api, &mut batch, &mut expected).await?)?;
            batch_bytes = 0;
        }
        batch_bytes += bytes.len() as u64;
        expected.push((blob_ref, bytes.len() as u64));
        batch.push(bytes);
    }

    if !batch.is_empty() {
        stats = add_upload_stats(stats, flush_put_many(api, &mut batch, &mut expected).await?)?;
    }
    Ok(stats)
}

async fn flush_put_many(
    api: &(impl CasVfsApi + Sync),
    batch: &mut Vec<Vec<u8>>,
    expected: &mut Vec<(String, u64)>,
) -> Result<UploadStats> {
    let sent = std::mem::take(batch);
    let expected = std::mem::take(expected);
    let response = api
        .put_blobs(sent)
        .await
        .map_err(crate::api_client::api_error)?;
    if response.len() != expected.len() {
        bail!(
            "gateway returned {} blob put entries for {} uploaded blobs",
            response.len(),
            expected.len()
        );
    }
    let mut stats = UploadStats::default();
    for (item, (expected_ref, expected_bytes)) in response.into_iter().zip(expected) {
        if item.blob_ref != expected_ref || item.bytes != expected_bytes {
            bail!(
                "gateway stored unexpected blob: expected {} {} bytes, got {} {} bytes",
                expected_ref,
                expected_bytes,
                item.blob_ref,
                item.bytes
            );
        }
        stats.blobs += 1;
        stats.bytes += item.bytes;
    }
    Ok(stats)
}

fn add_upload_stats(left: UploadStats, right: UploadStats) -> Result<UploadStats> {
    Ok(UploadStats {
        blobs: left
            .blobs
            .checked_add(right.blobs)
            .context("uploaded blob count overflowed")?,
        bytes: left
            .bytes
            .checked_add(right.bytes)
            .context("uploaded blob byte count overflowed")?,
    })
}

fn manifest_from_plan(plan: &SnapshotUploadPlan) -> Result<VfsSnapshotManifest> {
    let mut manifest = VfsSnapshotManifest::empty();
    for directory in &plan.directories {
        create_manifest_directory(&mut manifest, directory, true)?;
    }
    for file in &plan.files {
        write_manifest_file_ref(
            &mut manifest,
            &file.vfs_path,
            file.blob_ref.clone(),
            file.size_bytes,
            file.media_type.clone(),
            file.executable,
        )?;
    }
    manifest.validate()?;
    Ok(manifest)
}

fn read_source_bytes(file: &SnapshotUploadEntry) -> Result<Vec<u8>> {
    match &file.source {
        SnapshotEntrySource::HostPath(path) => Ok(fs::read(path)?),
        SnapshotEntrySource::Bytes(bytes) => Ok(bytes.clone()),
    }
}

#[derive(Clone, Debug)]
struct MaterializeFileEntry {
    path: VfsPath,
    file: VfsFile,
}

fn collect_materialize_entries(
    directory: &VfsDirectory,
    components: Vec<String>,
    directories: &mut Vec<VfsPath>,
    files: &mut Vec<MaterializeFileEntry>,
) -> Result<()> {
    for (name, entry) in &directory.entries {
        let mut child_components = components.clone();
        child_components.push(name.clone());
        let path = vfs_path_from_components(&child_components)?;
        match entry {
            VfsEntry::Directory(directory) => {
                directories.push(path);
                collect_materialize_entries(directory, child_components, directories, files)?;
            }
            VfsEntry::File(file) => files.push(MaterializeFileEntry {
                path,
                file: file.clone(),
            }),
        }
    }
    Ok(())
}

fn prepare_materialize_destination(destination: &Path) -> Result<PathBuf> {
    if let Ok(metadata) = fs::symlink_metadata(destination) {
        if metadata.file_type().is_symlink() {
            bail!(
                "materialize destination must be a directory, not a symlink: {}",
                destination.display()
            );
        }
        if !metadata.is_dir() {
            bail!(
                "materialize destination must be a directory: {}",
                destination.display()
            );
        }
    } else {
        fs::create_dir_all(destination).with_context(|| {
            format!(
                "failed to create materialize destination {}",
                destination.display()
            )
        })?;
    }

    fs::canonicalize(destination).with_context(|| {
        format!(
            "failed to canonicalize materialize destination {}",
            destination.display()
        )
    })
}

struct LocalMaterializer<'a, A: CasVfsApi + Sync + ?Sized> {
    api: &'a A,
    destination: PathBuf,
    blob_cache: BTreeMap<String, Vec<u8>>,
    summary: SnapshotMaterializeSummary,
}

impl<A: CasVfsApi + Sync + ?Sized> LocalMaterializer<'_, A> {
    fn ensure_directory(&mut self, path: &VfsPath) -> Result<()> {
        let mut current = self.destination.clone();
        for component in path.components() {
            current.push(component);
            match fs::symlink_metadata(&current) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() {
                        bail!(
                            "refusing to materialize through symlink directory {}",
                            current.display()
                        );
                    }
                    if !metadata.is_dir() {
                        bail!(
                            "cannot create materialized directory {}; path already exists and is not a directory",
                            current.display()
                        );
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    fs::create_dir(&current)
                        .with_context(|| format!("failed to create {}", current.display()))?;
                    self.summary.created_directories += 1;
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to inspect {}", current.display()));
                }
            }
        }
        Ok(())
    }

    async fn materialize_file(&mut self, entry: MaterializeFileEntry) -> Result<()> {
        let components = entry.path.components();
        if components.is_empty() {
            bail!("cannot materialize a file at the VFS root");
        }
        let parent = vfs_path_from_components(
            &components[..components.len() - 1]
                .iter()
                .map(|component| (*component).to_owned())
                .collect::<Vec<_>>(),
        )?;
        if !parent.is_root() {
            self.ensure_directory(&parent)?;
        }

        let target = self.path_for_vfs_path(&entry.path);
        if self.local_file_matches(&target, &entry.file)? {
            apply_executable(&target, entry.file.executable)
                .with_context(|| format!("failed to update mode for {}", target.display()))?;
            self.summary.reused_files += 1;
            self.summary.reused_bytes += entry.file.size_bytes;
            return Ok(());
        }

        let bytes = self.download_blob(&entry.file).await?;
        fs::write(&target, &bytes)
            .with_context(|| format!("failed to write {}", target.display()))?;
        apply_executable(&target, entry.file.executable)
            .with_context(|| format!("failed to update mode for {}", target.display()))?;
        self.summary.written_files += 1;
        self.summary.written_bytes += entry.file.size_bytes;
        Ok(())
    }

    fn local_file_matches(&self, target: &Path, file: &VfsFile) -> Result<bool> {
        let metadata = match fs::symlink_metadata(target) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", target.display()));
            }
        };
        if metadata.file_type().is_symlink() {
            bail!("refusing to materialize over symlink {}", target.display());
        }
        if metadata.is_dir() {
            bail!(
                "cannot materialize file {}; path already exists as a directory",
                target.display()
            );
        }
        if !metadata.is_file() || metadata.len() != file.size_bytes {
            return Ok(false);
        }

        let bytes =
            fs::read(target).with_context(|| format!("failed to read {}", target.display()))?;
        Ok(BlobRef::from_bytes(&bytes) == file.blob_ref)
    }

    async fn download_blob(&mut self, file: &VfsFile) -> Result<Vec<u8>> {
        let blob_ref = file.blob_ref.as_str().to_owned();
        if let Some(bytes) = self.blob_cache.get(&blob_ref) {
            return Ok(bytes.clone());
        }

        let response = self
            .api
            .get_blob(blob_ref.clone())
            .await
            .map_err(crate::api_client::api_error)
            .with_context(|| format!("failed to download blob {blob_ref}"))?;
        if response.blob_ref != blob_ref {
            bail!(
                "gateway returned blob {} for requested {}",
                response.blob_ref,
                blob_ref
            );
        }
        let bytes = BASE64
            .decode(&response.bytes_base64)
            .with_context(|| format!("gateway returned invalid base64 for blob {blob_ref}"))?;
        if response.bytes != bytes.len() as u64
            || file.size_bytes != bytes.len() as u64
            || BlobRef::from_bytes(&bytes) != file.blob_ref
        {
            bail!(
                "downloaded blob {} did not match manifest metadata: response bytes={}, decoded bytes={}, manifest bytes={}",
                blob_ref,
                response.bytes,
                bytes.len(),
                file.size_bytes
            );
        }

        self.summary.downloaded_blobs += 1;
        self.summary.downloaded_bytes += bytes.len() as u64;
        self.blob_cache.insert(blob_ref, bytes.clone());
        Ok(bytes)
    }

    fn path_for_vfs_path(&self, path: &VfsPath) -> PathBuf {
        let mut host_path = self.destination.clone();
        for component in path.components() {
            host_path.push(component);
        }
        host_path
    }
}

impl SnapshotEntrySource {
    fn display(&self) -> String {
        match self {
            SnapshotEntrySource::HostPath(path) => path.display().to_string(),
            SnapshotEntrySource::Bytes(_) => "<inline bytes>".to_owned(),
        }
    }
}

#[cfg(unix)]
fn apply_executable(path: &Path, executable: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::metadata(path)?;
    let mut permissions = metadata.permissions();
    let mut mode = permissions.mode();
    if executable {
        mode |= (mode & 0o444) >> 2;
    } else {
        mode &= !0o111;
    }
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn apply_executable(_path: &Path, _executable: bool) -> std::io::Result<()> {
    Ok(())
}

struct DirectoryScanner {
    limits: VfsSnapshotLimits,
    plan: SnapshotUploadPlan,
    file_count: u64,
    total_bytes: u64,
}

impl DirectoryScanner {
    fn scan_dir(&mut self, host_dir: &Path, relative_components: Vec<String>) -> Result<()> {
        let mut entries = fs::read_dir(host_dir)
            .with_context(|| format!("failed to read directory {}", host_dir.display()))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .with_context(|| format!("failed to read entries from {}", host_dir.display()))?;
        entries.sort_by_key(|entry| entry.file_name());

        for entry in entries {
            let host_path = entry.path();
            let file_name = path_component_to_string(entry.file_name(), &host_path)?;
            let mut child_components = relative_components.clone();
            child_components.push(file_name);

            let metadata = fs::symlink_metadata(&host_path)
                .with_context(|| format!("failed to inspect {}", host_path.display()))?;
            if metadata.file_type().is_symlink() {
                self.warn_skip(&host_path, "skipping symlink");
                continue;
            }

            let vfs_path = vfs_path_from_components(&child_components)?;
            if vfs_path.depth() > self.limits.max_depth {
                bail!(
                    "vfs snapshot limit exceeded for depth: value {} is greater than max {} at {}",
                    vfs_path.depth(),
                    self.limits.max_depth,
                    host_path.display()
                );
            }

            if metadata.is_dir() {
                self.plan.directories.push(vfs_path);
                self.scan_dir(&host_path, child_components)?;
            } else if metadata.is_file() {
                self.add_file(host_path, vfs_path, &metadata)?;
            } else {
                self.warn_skip(&host_path, "skipping non-file filesystem entry");
            }
        }
        Ok(())
    }

    fn add_file(
        &mut self,
        host_path: PathBuf,
        vfs_path: VfsPath,
        metadata: &fs::Metadata,
    ) -> Result<()> {
        if self.file_count + 1 > self.limits.max_files {
            bail!(
                "vfs snapshot limit exceeded for files: value {} is greater than max {}",
                self.file_count + 1,
                self.limits.max_files
            );
        }

        let bytes = fs::read(&host_path)
            .with_context(|| format!("failed to read {}", host_path.display()))?;
        let size_bytes = bytes.len() as u64;
        if size_bytes > self.limits.max_single_file_bytes {
            bail!(
                "vfs snapshot limit exceeded for single_file_bytes: value {} is greater than max {} at {}",
                size_bytes,
                self.limits.max_single_file_bytes,
                host_path.display()
            );
        }
        let total_bytes = self
            .total_bytes
            .checked_add(size_bytes)
            .context("snapshot byte total overflowed")?;
        if total_bytes > self.limits.max_total_bytes {
            bail!(
                "vfs snapshot limit exceeded for total_bytes: value {} is greater than max {}",
                total_bytes,
                self.limits.max_total_bytes
            );
        }

        self.file_count += 1;
        self.total_bytes = total_bytes;
        self.plan.files.push(SnapshotUploadEntry {
            vfs_path,
            source: SnapshotEntrySource::HostPath(host_path),
            blob_ref: BlobRef::from_bytes(&bytes),
            size_bytes,
            media_type: None,
            executable: executable(metadata),
        });
        Ok(())
    }

    fn warn_skip(&mut self, path: &Path, message: impl Into<String>) {
        self.plan.warnings.push(SnapshotWarning {
            path: path.display().to_string(),
            message: message.into(),
        });
    }
}

fn vfs_path_from_components(components: &[String]) -> Result<VfsPath> {
    VfsPath::parse(format!("/{}", components.join("/"))).map_err(Into::into)
}

fn path_component_to_string(component: std::ffi::OsString, path: &Path) -> Result<String> {
    component
        .into_string()
        .map_err(|_| anyhow::anyhow!("path is not valid UTF-8: {}", path.display()))
}

#[cfg(unix)]
fn executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use api::{BlobHasItem, BlobReadResponse, VfsSnapshotCommitResponse, VfsSnapshotReadResponse};
    use tempfile::tempdir;
    use vfs::VfsEntry;

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn snapshot_directory_uploads_missing_blobs_and_commits_manifest() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("README.md"), b"hello\n").unwrap();
        fs::create_dir(temp.path().join("src")).unwrap();
        fs::write(temp.path().join("src/lib.rs"), b"pub fn ok() {}\n").unwrap();
        fs::write(temp.path().join("src/copy.rs"), b"pub fn ok() {}\n").unwrap();

        let existing_ref = BlobRef::from_bytes(b"hello\n").as_str().to_owned();
        let api = FakeCasVfsApi::new([existing_ref.clone()]);

        let summary =
            upload_snapshot_directory(&api, temp.path(), SnapshotUploadOptions::default())
                .await
                .unwrap();

        assert_eq!(summary.files, 3);
        assert_eq!(summary.reused_blobs, 1);
        assert_eq!(summary.uploaded_blobs, 1);
        assert_eq!(
            api.put_batches.lock().unwrap().as_slice(),
            &[vec![b"pub fn ok() {}\n".to_vec()]]
        );

        let commits = api.commits.lock().unwrap();
        let manifest = commits.last().unwrap();
        assert_eq!(manifest.totals.files, 3);
        let src = match manifest.root.entry("src").unwrap() {
            VfsEntry::Directory(directory) => directory,
            VfsEntry::File(_) => panic!("src should be a directory"),
        };
        assert!(matches!(src.entry("lib.rs"), Some(VfsEntry::File(_))));
        assert!(matches!(src.entry("copy.rs"), Some(VfsEntry::File(_))));
        let readme = match manifest.root.entry("README.md").unwrap() {
            VfsEntry::File(file) => file,
            VfsEntry::Directory(_) => panic!("README.md should be a file"),
        };
        assert_eq!(readme.blob_ref.as_str(), existing_ref);
    }

    #[test]
    fn scan_snapshot_directory_preserves_empty_directories() {
        let temp = tempdir().unwrap();
        fs::create_dir(temp.path().join("empty")).unwrap();

        let plan =
            scan_snapshot_directory(temp.path(), SnapshotUploadOptions::default().limits).unwrap();

        assert_eq!(plan.files.len(), 0);
        assert_eq!(
            plan.directories
                .iter()
                .map(VfsPath::as_str)
                .collect::<Vec<_>>(),
            vec!["/empty"]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn upload_snapshot_plan_accepts_inline_bytes_entries() {
        let api = FakeCasVfsApi::new([]);
        let bytes = b"generated bundle\n".to_vec();
        let plan = SnapshotUploadPlan {
            root: "<generated>".to_owned(),
            directories: vec![VfsPath::parse("/bundle").unwrap()],
            files: vec![SnapshotUploadEntry {
                vfs_path: VfsPath::parse("/bundle/index.txt").unwrap(),
                source: SnapshotEntrySource::Bytes(bytes.clone()),
                blob_ref: BlobRef::from_bytes(&bytes),
                size_bytes: bytes.len() as u64,
                media_type: Some("text/plain".to_owned()),
                executable: false,
            }],
            warnings: Vec::new(),
        };

        let summary = upload_snapshot_plan(&api, plan, SnapshotUploadOptions::default())
            .await
            .unwrap();

        assert_eq!(summary.files, 1);
        assert_eq!(summary.uploaded_blobs, 1);
        let commits = api.commits.lock().unwrap();
        let bundle = match commits.last().unwrap().root.entry("bundle").unwrap() {
            VfsEntry::Directory(directory) => directory,
            VfsEntry::File(_) => panic!("bundle should be a directory"),
        };
        let file = match bundle.entry("index.txt").unwrap() {
            VfsEntry::File(file) => file,
            VfsEntry::Directory(_) => panic!("bundle/index.txt should be a file"),
        };
        assert_eq!(file.media_type.as_deref(), Some("text/plain"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn materialize_snapshot_writes_files_and_reuses_matching_local_content() {
        let readme = b"hello\n".to_vec();
        let tool = b"#!/bin/sh\necho ok\n".to_vec();
        let readme_ref = BlobRef::from_bytes(&readme);
        let tool_ref = BlobRef::from_bytes(&tool);
        let mut manifest = VfsSnapshotManifest::empty();
        create_manifest_directory(&mut manifest, &VfsPath::parse("/bin").unwrap(), false).unwrap();
        write_manifest_file_ref(
            &mut manifest,
            &VfsPath::parse("/README.md").unwrap(),
            readme_ref.clone(),
            readme.len() as u64,
            None,
            false,
        )
        .unwrap();
        write_manifest_file_ref(
            &mut manifest,
            &VfsPath::parse("/bin/tool").unwrap(),
            tool_ref.clone(),
            tool.len() as u64,
            None,
            true,
        )
        .unwrap();
        let snapshot_ref = snapshot_ref_for_manifest(&manifest);
        let api = FakeCasVfsApi::new([]).with_snapshot(
            snapshot_ref.clone(),
            manifest,
            [
                (readme_ref.clone(), readme.clone()),
                (tool_ref, tool.clone()),
            ],
        );
        let destination = tempdir().unwrap();
        fs::write(destination.path().join("README.md"), &readme).unwrap();

        let summary = materialize_snapshot(&api, snapshot_ref.clone(), destination.path())
            .await
            .unwrap();

        assert_eq!(summary.snapshot_ref, snapshot_ref);
        assert_eq!(summary.files, 2);
        assert_eq!(summary.directories, 1);
        assert_eq!(summary.written_files, 1);
        assert_eq!(summary.reused_files, 1);
        assert_eq!(summary.downloaded_blobs, 1);
        assert_eq!(fs::read(destination.path().join("bin/tool")).unwrap(), tool);
        assert_eq!(
            fs::read(destination.path().join("README.md")).unwrap(),
            readme
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(destination.path().join("bin/tool"))
                .unwrap()
                .permissions()
                .mode();
            assert_ne!(mode & 0o100, 0);
        }
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn materialize_snapshot_refuses_to_write_through_destination_symlinks() {
        use std::os::unix::fs::symlink;

        let bytes = b"outside\n".to_vec();
        let blob_ref = BlobRef::from_bytes(&bytes);
        let mut manifest = VfsSnapshotManifest::empty();
        create_manifest_directory(&mut manifest, &VfsPath::parse("/linked").unwrap(), false)
            .unwrap();
        write_manifest_file_ref(
            &mut manifest,
            &VfsPath::parse("/linked/file.txt").unwrap(),
            blob_ref.clone(),
            bytes.len() as u64,
            None,
            false,
        )
        .unwrap();
        let snapshot_ref = snapshot_ref_for_manifest(&manifest);
        let api = FakeCasVfsApi::new([]).with_snapshot(
            snapshot_ref.clone(),
            manifest,
            [(blob_ref, bytes.clone())],
        );
        let destination = tempdir().unwrap();
        let outside = tempdir().unwrap();
        symlink(outside.path(), destination.path().join("linked")).unwrap();

        let error = materialize_snapshot(&api, snapshot_ref, destination.path())
            .await
            .expect_err("symlink destination must be rejected");

        assert!(error.to_string().contains("symlink directory"));
        assert!(!outside.path().join("file.txt").exists());
    }

    struct FakeCasVfsApi {
        existing: Mutex<BTreeSet<String>>,
        blobs: Mutex<BTreeMap<String, Vec<u8>>>,
        snapshots: Mutex<BTreeMap<String, VfsSnapshotManifest>>,
        put_batches: Mutex<Vec<Vec<Vec<u8>>>>,
        commits: Mutex<Vec<VfsSnapshotManifest>>,
    }

    impl FakeCasVfsApi {
        fn new(existing: impl IntoIterator<Item = String>) -> Self {
            Self {
                existing: Mutex::new(existing.into_iter().collect()),
                blobs: Mutex::new(BTreeMap::new()),
                snapshots: Mutex::new(BTreeMap::new()),
                put_batches: Mutex::new(Vec::new()),
                commits: Mutex::new(Vec::new()),
            }
        }

        fn with_snapshot(
            self,
            snapshot_ref: String,
            manifest: VfsSnapshotManifest,
            blobs: impl IntoIterator<Item = (BlobRef, Vec<u8>)>,
        ) -> Self {
            {
                let mut stored_blobs = self.blobs.lock().unwrap();
                let mut existing = self.existing.lock().unwrap();
                for (blob_ref, bytes) in blobs {
                    existing.insert(blob_ref.as_str().to_owned());
                    stored_blobs.insert(blob_ref.as_str().to_owned(), bytes);
                }
            }
            self.snapshots
                .lock()
                .unwrap()
                .insert(snapshot_ref, manifest);
            self
        }
    }

    #[async_trait]
    impl CasVfsApi for FakeCasVfsApi {
        async fn has_blobs(
            &self,
            blob_refs: Vec<String>,
        ) -> Result<Vec<BlobHasItem>, AgentApiError> {
            let existing = self.existing.lock().unwrap();
            Ok(blob_refs
                .into_iter()
                .map(|blob_ref| BlobHasItem {
                    exists: existing.contains(&blob_ref),
                    blob_ref,
                })
                .collect())
        }

        async fn put_blobs(
            &self,
            blobs: Vec<Vec<u8>>,
        ) -> Result<Vec<BlobPutResult>, AgentApiError> {
            self.put_batches.lock().unwrap().push(blobs.clone());
            let mut existing = self.existing.lock().unwrap();
            let mut stored_blobs = self.blobs.lock().unwrap();
            Ok(blobs
                .into_iter()
                .map(|bytes| {
                    let blob_ref = BlobRef::from_bytes(&bytes).as_str().to_owned();
                    existing.insert(blob_ref.clone());
                    stored_blobs.insert(blob_ref.clone(), bytes.clone());
                    BlobPutResult {
                        blob_ref,
                        bytes: bytes.len() as u64,
                    }
                })
                .collect())
        }

        async fn get_blob(&self, blob_ref: String) -> Result<BlobReadResponse, AgentApiError> {
            let blobs = self.blobs.lock().unwrap();
            let bytes = blobs
                .get(&blob_ref)
                .ok_or_else(|| AgentApiError::not_found(format!("blob not found: {blob_ref}")))?;
            Ok(BlobReadResponse {
                blob_ref,
                bytes_base64: BASE64.encode(bytes),
                bytes: bytes.len() as u64,
            })
        }

        async fn commit_vfs_snapshot(
            &self,
            manifest: &VfsSnapshotManifest,
        ) -> Result<VfsSnapshotCommitResponse, AgentApiError> {
            manifest
                .validate()
                .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
            self.commits.lock().unwrap().push(manifest.clone());
            let bytes = serde_json::to_vec(manifest)
                .map_err(|error| AgentApiError::internal(error.to_string()))?;
            let snapshot_ref = BlobRef::from_bytes(&bytes).as_str().to_owned();
            self.snapshots
                .lock()
                .unwrap()
                .insert(snapshot_ref.clone(), manifest.clone());
            Ok(VfsSnapshotCommitResponse {
                snapshot_ref,
                files: manifest.totals.files,
                bytes: manifest.totals.bytes,
            })
        }

        async fn read_vfs_snapshot(
            &self,
            snapshot_ref: String,
        ) -> Result<VfsSnapshotReadResponse, AgentApiError> {
            let snapshots = self.snapshots.lock().unwrap();
            let manifest = snapshots.get(&snapshot_ref).ok_or_else(|| {
                AgentApiError::not_found(format!("snapshot not found: {snapshot_ref}"))
            })?;
            Ok(VfsSnapshotReadResponse {
                snapshot_ref,
                manifest: serde_json::to_value(manifest)
                    .map_err(|error| AgentApiError::internal(error.to_string()))?,
                files: manifest.totals.files,
                bytes: manifest.totals.bytes,
            })
        }
    }

    fn snapshot_ref_for_manifest(manifest: &VfsSnapshotManifest) -> String {
        let bytes = vfs::encode_snapshot_manifest(manifest).unwrap();
        BlobRef::from_bytes(&bytes).as_str().to_owned()
    }
}
