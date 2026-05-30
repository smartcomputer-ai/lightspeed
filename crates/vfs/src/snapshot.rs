use engine::{BlobRef, storage::BlobStore};
use std::collections::BTreeSet;

use crate::{
    manifest::{VfsDirectory, VfsEntry, VfsError, VfsFile, VfsSnapshotManifest},
    path::VfsPath,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineFile {
    pub path: VfsPath,
    pub bytes: Vec<u8>,
    pub media_type: Option<String>,
    pub executable: bool,
}

impl InlineFile {
    pub fn new(path: impl AsRef<str>, bytes: impl Into<Vec<u8>>) -> Result<Self, VfsError> {
        Ok(Self {
            path: VfsPath::parse(path)?,
            bytes: bytes.into(),
            media_type: None,
            executable: false,
        })
    }

    pub fn with_media_type(mut self, media_type: impl Into<String>) -> Self {
        self.media_type = Some(media_type.into());
        self
    }

    pub fn executable(mut self, executable: bool) -> Self {
        self.executable = executable;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VfsSnapshotLimits {
    pub max_files: u64,
    pub max_total_bytes: u64,
    pub max_single_file_bytes: u64,
    pub max_depth: usize,
}

impl VfsSnapshotLimits {
    pub const fn new(
        max_files: u64,
        max_total_bytes: u64,
        max_single_file_bytes: u64,
        max_depth: usize,
    ) -> Self {
        Self {
            max_files,
            max_total_bytes,
            max_single_file_bytes,
            max_depth,
        }
    }
}

impl Default for VfsSnapshotLimits {
    fn default() -> Self {
        Self {
            max_files: 10_000,
            max_total_bytes: 512 * 1024 * 1024,
            max_single_file_bytes: 128 * 1024 * 1024,
            max_depth: 64,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateInlineSnapshotRequest {
    pub files: Vec<InlineFile>,
    pub limits: VfsSnapshotLimits,
}

impl CreateInlineSnapshotRequest {
    pub fn new(files: Vec<InlineFile>) -> Self {
        Self {
            files,
            limits: VfsSnapshotLimits::default(),
        }
    }

    pub fn with_limits(mut self, limits: VfsSnapshotLimits) -> Self {
        self.limits = limits;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateVfsSnapshotResult {
    pub snapshot_ref: BlobRef,
    pub manifest: VfsSnapshotManifest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VfsNode<'a> {
    File(&'a VfsFile),
    Directory(&'a VfsDirectory),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VfsPathMetadata {
    pub is_directory: bool,
    pub is_file: bool,
    pub size_bytes: Option<u64>,
    pub media_type: Option<String>,
    pub executable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VfsDirectoryListingEntry {
    pub file_name: String,
    pub is_directory: bool,
    pub is_file: bool,
    pub size_bytes: Option<u64>,
}

pub async fn create_inline_snapshot(
    blobs: &(impl BlobStore + ?Sized),
    request: CreateInlineSnapshotRequest,
) -> Result<CreateVfsSnapshotResult, VfsError> {
    validate_inline_files(&request.files, request.limits)?;

    let mut staged_files = Vec::with_capacity(request.files.len());
    let mut file_bytes = Vec::with_capacity(request.files.len());
    for file in request.files {
        staged_files.push((
            file.path,
            file.bytes.len() as u64,
            file.media_type,
            file.executable,
        ));
        file_bytes.push(file.bytes);
    }

    let blob_refs = blobs.put_many(file_bytes).await?;
    let mut manifest = VfsSnapshotManifest::empty();
    for ((path, size_bytes, media_type, executable), blob_ref) in
        staged_files.into_iter().zip(blob_refs)
    {
        let vfs_file = VfsFile {
            blob_ref,
            size_bytes,
            media_type,
            executable,
        };
        insert_file(&mut manifest.root, &path, vfs_file)?;
        manifest.totals.files += 1;
        manifest.totals.bytes += size_bytes;
    }

    let manifest_bytes = encode_snapshot_manifest(&manifest)?;
    let snapshot_ref = blobs.put_bytes(manifest_bytes).await?;
    Ok(CreateVfsSnapshotResult {
        snapshot_ref,
        manifest,
    })
}

pub async fn read_snapshot_manifest(
    blobs: &(impl BlobStore + ?Sized),
    snapshot_ref: &BlobRef,
) -> Result<VfsSnapshotManifest, VfsError> {
    let bytes = blobs.read_bytes(snapshot_ref).await?;
    decode_snapshot_manifest(&bytes)
}

pub fn encode_snapshot_manifest(manifest: &VfsSnapshotManifest) -> Result<Vec<u8>, VfsError> {
    manifest.validate()?;
    serde_json::to_vec(manifest).map_err(|source| VfsError::EncodeManifest { source })
}

pub fn decode_snapshot_manifest(bytes: &[u8]) -> Result<VfsSnapshotManifest, VfsError> {
    let manifest: VfsSnapshotManifest =
        serde_json::from_slice(bytes).map_err(|source| VfsError::DecodeManifest { source })?;
    manifest.validate()?;
    Ok(manifest)
}

pub fn lookup_snapshot_path<'a>(
    manifest: &'a VfsSnapshotManifest,
    path: &VfsPath,
) -> Result<VfsNode<'a>, VfsError> {
    if path.is_root() {
        return Ok(VfsNode::Directory(&manifest.root));
    }

    let mut current = &manifest.root;
    let components = path.components();
    for (index, component) in components.iter().enumerate() {
        let entry = current
            .entries
            .get(*component)
            .ok_or_else(|| VfsError::NotFound { path: path.clone() })?;
        let is_last = index == components.len() - 1;
        match (entry, is_last) {
            (VfsEntry::File(file), true) => return Ok(VfsNode::File(file)),
            (VfsEntry::Directory(directory), true) => return Ok(VfsNode::Directory(directory)),
            (VfsEntry::Directory(directory), false) => current = directory,
            (VfsEntry::File(_), false) => {
                return Err(VfsError::NotADirectory { path: path.clone() });
            }
        }
    }

    Err(VfsError::NotFound { path: path.clone() })
}

pub async fn read_snapshot_file(
    blobs: &(impl BlobStore + ?Sized),
    manifest: &VfsSnapshotManifest,
    path: &VfsPath,
) -> Result<Vec<u8>, VfsError> {
    let file = match lookup_snapshot_path(manifest, path)? {
        VfsNode::File(file) => file,
        VfsNode::Directory(_) => return Err(VfsError::NotAFile { path: path.clone() }),
    };
    let bytes = blobs.read_bytes(&file.blob_ref).await?;
    if bytes.len() as u64 != file.size_bytes {
        return Err(VfsError::InvalidManifest {
            message: format!(
                "file size mismatch for {path}: manifest has {}, blob has {}",
                file.size_bytes,
                bytes.len()
            ),
        });
    }
    Ok(bytes)
}

pub fn list_snapshot_directory(
    manifest: &VfsSnapshotManifest,
    path: &VfsPath,
) -> Result<Vec<VfsDirectoryListingEntry>, VfsError> {
    let directory = match lookup_snapshot_path(manifest, path)? {
        VfsNode::Directory(directory) => directory,
        VfsNode::File(_) => return Err(VfsError::NotADirectory { path: path.clone() }),
    };

    Ok(directory
        .entries
        .iter()
        .map(|(file_name, entry)| match entry {
            VfsEntry::File(file) => VfsDirectoryListingEntry {
                file_name: file_name.clone(),
                is_directory: false,
                is_file: true,
                size_bytes: Some(file.size_bytes),
            },
            VfsEntry::Directory(_) => VfsDirectoryListingEntry {
                file_name: file_name.clone(),
                is_directory: true,
                is_file: false,
                size_bytes: None,
            },
        })
        .collect())
}

pub fn stat_snapshot_path(
    manifest: &VfsSnapshotManifest,
    path: &VfsPath,
) -> Result<VfsPathMetadata, VfsError> {
    match lookup_snapshot_path(manifest, path)? {
        VfsNode::File(file) => Ok(VfsPathMetadata {
            is_directory: false,
            is_file: true,
            size_bytes: Some(file.size_bytes),
            media_type: file.media_type.clone(),
            executable: file.executable,
        }),
        VfsNode::Directory(_) => Ok(VfsPathMetadata {
            is_directory: true,
            is_file: false,
            size_bytes: None,
            media_type: None,
            executable: false,
        }),
    }
}

fn validate_inline_files(files: &[InlineFile], limits: VfsSnapshotLimits) -> Result<(), VfsError> {
    if files.len() as u64 > limits.max_files {
        return Err(VfsError::LimitExceeded {
            limit: "files",
            value: files.len() as u64,
            max: limits.max_files,
        });
    }

    let mut total_bytes = 0u64;
    let mut seen = BTreeSet::new();
    for file in files {
        if file.path.is_root() {
            return Err(VfsError::RootFile);
        }
        if !seen.insert(file.path.clone()) {
            return Err(VfsError::DuplicatePath {
                path: file.path.clone(),
            });
        }
        if file.path.depth() > limits.max_depth {
            return Err(VfsError::LimitExceeded {
                limit: "depth",
                value: file.path.depth() as u64,
                max: limits.max_depth as u64,
            });
        }
        let file_bytes = file.bytes.len() as u64;
        if file_bytes > limits.max_single_file_bytes {
            return Err(VfsError::LimitExceeded {
                limit: "single_file_bytes",
                value: file_bytes,
                max: limits.max_single_file_bytes,
            });
        }
        total_bytes = total_bytes
            .checked_add(file_bytes)
            .ok_or(VfsError::LimitExceeded {
                limit: "total_bytes",
                value: u64::MAX,
                max: limits.max_total_bytes,
            })?;
        if total_bytes > limits.max_total_bytes {
            return Err(VfsError::LimitExceeded {
                limit: "total_bytes",
                value: total_bytes,
                max: limits.max_total_bytes,
            });
        }
    }

    let mut root = VfsDirectory::default();
    for file in files {
        insert_file(
            &mut root,
            &file.path,
            VfsFile {
                blob_ref: BlobRef::default(),
                size_bytes: file.bytes.len() as u64,
                media_type: file.media_type.clone(),
                executable: file.executable,
            },
        )?;
    }

    Ok(())
}

fn insert_file(
    directory: &mut VfsDirectory,
    path: &VfsPath,
    file: VfsFile,
) -> Result<(), VfsError> {
    let components = path.components();
    if components.is_empty() {
        return Err(VfsError::RootFile);
    }

    let mut current = directory;
    for component in &components[..components.len() - 1] {
        let entry = current
            .entries
            .entry((*component).to_owned())
            .or_insert_with(|| VfsEntry::Directory(VfsDirectory::default()));
        match entry {
            VfsEntry::Directory(directory) => {
                current = directory;
            }
            VfsEntry::File(_) => {
                return Err(VfsError::PathConflict {
                    path: path.clone(),
                    existing: "file",
                });
            }
        }
    }

    let filename = components[components.len() - 1].to_owned();
    match current.entries.insert(filename, VfsEntry::File(file)) {
        None => Ok(()),
        Some(VfsEntry::Directory(_)) => Err(VfsError::PathConflict {
            path: path.clone(),
            existing: "directory",
        }),
        Some(VfsEntry::File(_)) => Err(VfsError::DuplicatePath { path: path.clone() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{VFS_SNAPSHOT_SCHEMA_VERSION, VfsTotals};
    use engine::{
        BlobRef,
        storage::{BlobStore, InMemoryBlobStore},
    };

    #[tokio::test(flavor = "current_thread")]
    async fn inline_snapshot_writes_file_blobs_and_manifest() {
        let blobs = InMemoryBlobStore::new();
        let request = CreateInlineSnapshotRequest::new(vec![
            InlineFile::new("README.md", b"# hello\n".to_vec())
                .unwrap()
                .with_media_type("text/markdown"),
            InlineFile::new("src/lib.rs", b"pub fn ok() {}\n".to_vec()).unwrap(),
            InlineFile::new("scripts/build.sh", b"#!/bin/sh\n".to_vec())
                .unwrap()
                .executable(true),
        ]);

        let result = create_inline_snapshot(&blobs, request).await.unwrap();
        assert_eq!(result.manifest.schema_version, VFS_SNAPSHOT_SCHEMA_VERSION);
        assert_eq!(
            result.manifest.totals,
            VfsTotals {
                files: 3,
                bytes: 33
            }
        );

        let manifest_bytes = blobs.read_bytes(&result.snapshot_ref).await.unwrap();
        let decoded = decode_snapshot_manifest(&manifest_bytes).unwrap();
        assert_eq!(decoded, result.manifest);

        let readme = match decoded.root.entry("README.md").unwrap() {
            VfsEntry::File(file) => file,
            VfsEntry::Directory(_) => panic!("README.md should be a file"),
        };
        assert_eq!(readme.media_type.as_deref(), Some("text/markdown"));
        assert_eq!(readme.size_bytes, 8);
        assert_eq!(
            blobs.read_bytes(&readme.blob_ref).await.unwrap(),
            b"# hello\n"
        );

        let scripts = match decoded.root.entry("scripts").unwrap() {
            VfsEntry::Directory(directory) => directory,
            VfsEntry::File(_) => panic!("scripts should be a directory"),
        };
        let build = match scripts.entry("build.sh").unwrap() {
            VfsEntry::File(file) => file,
            VfsEntry::Directory(_) => panic!("build.sh should be a file"),
        };
        assert!(build.executable);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn snapshot_read_helpers_resolve_paths() {
        let blobs = InMemoryBlobStore::new();
        let result = create_inline_snapshot(
            &blobs,
            CreateInlineSnapshotRequest::new(vec![
                InlineFile::new("README.md", b"hello\n".to_vec()).unwrap(),
                InlineFile::new("src/lib.rs", b"pub fn f() {}\n".to_vec())
                    .unwrap()
                    .with_media_type("text/rust"),
            ]),
        )
        .await
        .unwrap();

        let loaded = read_snapshot_manifest(&blobs, &result.snapshot_ref)
            .await
            .unwrap();
        assert_eq!(loaded, result.manifest);

        assert!(matches!(
            lookup_snapshot_path(&loaded, &VfsPath::root()).unwrap(),
            VfsNode::Directory(_)
        ));
        assert_eq!(
            read_snapshot_file(&blobs, &loaded, &VfsPath::parse("/README.md").unwrap())
                .await
                .unwrap(),
            b"hello\n"
        );

        let root_entries = list_snapshot_directory(&loaded, &VfsPath::root()).unwrap();
        assert_eq!(
            root_entries,
            vec![
                VfsDirectoryListingEntry {
                    file_name: "README.md".to_string(),
                    is_directory: false,
                    is_file: true,
                    size_bytes: Some(6),
                },
                VfsDirectoryListingEntry {
                    file_name: "src".to_string(),
                    is_directory: true,
                    is_file: false,
                    size_bytes: None,
                },
            ]
        );

        let metadata = stat_snapshot_path(&loaded, &VfsPath::parse("src/lib.rs").unwrap()).unwrap();
        assert!(metadata.is_file);
        assert!(!metadata.is_directory);
        assert_eq!(metadata.size_bytes, Some(14));
        assert_eq!(metadata.media_type.as_deref(), Some("text/rust"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn snapshot_read_helpers_report_missing_and_wrong_kind_paths() {
        let blobs = InMemoryBlobStore::new();
        let result = create_inline_snapshot(
            &blobs,
            CreateInlineSnapshotRequest::new(vec![
                InlineFile::new("README.md", b"hello\n".to_vec()).unwrap(),
                InlineFile::new("src/lib.rs", b"pub fn f() {}\n".to_vec()).unwrap(),
            ]),
        )
        .await
        .unwrap();

        assert!(matches!(
            lookup_snapshot_path(&result.manifest, &VfsPath::parse("/missing").unwrap()),
            Err(VfsError::NotFound { .. })
        ));
        assert!(matches!(
            read_snapshot_file(&blobs, &result.manifest, &VfsPath::parse("/src").unwrap()).await,
            Err(VfsError::NotAFile { .. })
        ));
        assert!(matches!(
            list_snapshot_directory(&result.manifest, &VfsPath::parse("/README.md").unwrap()),
            Err(VfsError::NotADirectory { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inline_snapshot_allows_empty_tree() {
        let blobs = InMemoryBlobStore::new();
        let result = create_inline_snapshot(&blobs, CreateInlineSnapshotRequest::new(Vec::new()))
            .await
            .unwrap();

        assert_eq!(result.manifest, VfsSnapshotManifest::empty());
        assert!(blobs.has_blob(&result.snapshot_ref).await.unwrap());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inline_snapshot_rejects_duplicate_paths_before_writing() {
        let blobs = InMemoryBlobStore::new();
        let request = CreateInlineSnapshotRequest::new(vec![
            InlineFile::new("a.txt", b"one".to_vec()).unwrap(),
            InlineFile::new("/a.txt", b"two".to_vec()).unwrap(),
        ]);

        let error = create_inline_snapshot(&blobs, request).await.unwrap_err();
        assert!(matches!(error, VfsError::DuplicatePath { .. }));
        assert!(!blobs.has_blob(&BlobRef::from_bytes(b"one")).await.unwrap());
        assert!(!blobs.has_blob(&BlobRef::from_bytes(b"two")).await.unwrap());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inline_snapshot_rejects_file_directory_conflicts_before_writing() {
        let blobs = InMemoryBlobStore::new();
        let request = CreateInlineSnapshotRequest::new(vec![
            InlineFile::new("src", b"file".to_vec()).unwrap(),
            InlineFile::new("src/lib.rs", b"nested".to_vec()).unwrap(),
        ]);

        let error = create_inline_snapshot(&blobs, request).await.unwrap_err();
        assert!(matches!(
            error,
            VfsError::PathConflict {
                existing: "file",
                ..
            }
        ));
        assert!(!blobs.has_blob(&BlobRef::from_bytes(b"file")).await.unwrap());
        assert!(
            !blobs
                .has_blob(&BlobRef::from_bytes(b"nested"))
                .await
                .unwrap()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inline_snapshot_enforces_limits_before_writing() {
        let blobs = InMemoryBlobStore::new();
        let request = CreateInlineSnapshotRequest::new(vec![
            InlineFile::new("one.txt", b"1234".to_vec()).unwrap(),
            InlineFile::new("two.txt", b"56".to_vec()).unwrap(),
        ])
        .with_limits(VfsSnapshotLimits::new(10, 5, 10, 10));

        let error = create_inline_snapshot(&blobs, request).await.unwrap_err();
        assert!(matches!(
            error,
            VfsError::LimitExceeded {
                limit: "total_bytes",
                value: 6,
                max: 5
            }
        ));
        assert!(!blobs.has_blob(&BlobRef::from_bytes(b"1234")).await.unwrap());
        assert!(!blobs.has_blob(&BlobRef::from_bytes(b"56")).await.unwrap());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn inline_snapshot_rejects_root_file_and_deep_paths() {
        let blobs = InMemoryBlobStore::new();
        let root_request =
            CreateInlineSnapshotRequest::new(vec![InlineFile::new("/", b"root".to_vec()).unwrap()]);
        assert!(matches!(
            create_inline_snapshot(&blobs, root_request)
                .await
                .unwrap_err(),
            VfsError::RootFile
        ));

        let deep_request = CreateInlineSnapshotRequest::new(vec![
            InlineFile::new("a/b/c", b"deep".to_vec()).unwrap(),
        ])
        .with_limits(VfsSnapshotLimits::new(10, 100, 100, 2));
        assert!(matches!(
            create_inline_snapshot(&blobs, deep_request)
                .await
                .unwrap_err(),
            VfsError::LimitExceeded { limit: "depth", .. }
        ));
    }

    #[test]
    fn manifest_encode_decode_validates_schema_and_totals() {
        let mut manifest = VfsSnapshotManifest::empty();
        manifest.schema_version = "wrong".to_owned();
        assert!(matches!(
            encode_snapshot_manifest(&manifest),
            Err(VfsError::InvalidManifest { .. })
        ));

        let mut manifest = VfsSnapshotManifest::empty();
        manifest.root.entries.insert(
            "file.txt".to_owned(),
            VfsEntry::File(VfsFile {
                blob_ref: BlobRef::from_bytes(b"file"),
                size_bytes: 4,
                media_type: None,
                executable: false,
            }),
        );
        assert!(matches!(
            encode_snapshot_manifest(&manifest),
            Err(VfsError::InvalidManifest { .. })
        ));
    }
}
