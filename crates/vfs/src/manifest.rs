use engine::{BlobRef, storage::BlobStoreError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

use crate::path::{VfsPath, VfsPathError};

pub const VFS_SNAPSHOT_SCHEMA_VERSION: &str = "forge.vfs.snapshot.v1";

#[derive(Debug, Error)]
pub enum VfsError {
    #[error(transparent)]
    InvalidPath(#[from] VfsPathError),

    #[error("vfs file path cannot be the root path")]
    RootFile,

    #[error("duplicate vfs path: {path}")]
    DuplicatePath { path: VfsPath },

    #[error("vfs path already exists: {path}")]
    AlreadyExists { path: VfsPath },

    #[error("vfs path conflicts with an existing {existing}: {path}")]
    PathConflict {
        path: VfsPath,
        existing: &'static str,
    },

    #[error("vfs path not found: {path}")]
    NotFound { path: VfsPath },

    #[error("vfs path is not a file: {path}")]
    NotAFile { path: VfsPath },

    #[error("vfs path is not a directory: {path}")]
    NotADirectory { path: VfsPath },

    #[error("vfs directory is not empty: {path}")]
    DirectoryNotEmpty { path: VfsPath },

    #[error("invalid vfs operation: {message}")]
    InvalidOperation { message: String },

    #[error("vfs snapshot limit exceeded for {limit}: value {value} is greater than max {max}")]
    LimitExceeded {
        limit: &'static str,
        value: u64,
        max: u64,
    },

    #[error("invalid vfs snapshot manifest: {message}")]
    InvalidManifest { message: String },

    #[error("failed to encode vfs snapshot manifest: {source}")]
    EncodeManifest {
        #[source]
        source: serde_json::Error,
    },

    #[error("failed to decode vfs snapshot manifest: {source}")]
    DecodeManifest {
        #[source]
        source: serde_json::Error,
    },

    #[error(transparent)]
    BlobStore(#[from] BlobStoreError),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VfsSnapshotManifest {
    pub schema_version: String,
    pub root: VfsDirectory,
    pub totals: VfsTotals,
}

impl VfsSnapshotManifest {
    pub fn empty() -> Self {
        Self {
            schema_version: VFS_SNAPSHOT_SCHEMA_VERSION.to_owned(),
            root: VfsDirectory::default(),
            totals: VfsTotals::default(),
        }
    }

    pub fn validate(&self) -> Result<(), VfsError> {
        if self.schema_version != VFS_SNAPSHOT_SCHEMA_VERSION {
            return Err(VfsError::InvalidManifest {
                message: format!("unsupported schema version '{}'", self.schema_version),
            });
        }
        let mut totals = VfsTotals::default();
        validate_directory(&self.root, &mut totals)?;
        if totals != self.totals {
            return Err(VfsError::InvalidManifest {
                message: format!(
                    "manifest totals do not match tree contents: expected {:?}, computed {:?}",
                    self.totals, totals
                ),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VfsDirectory {
    #[serde(default)]
    pub entries: BTreeMap<String, VfsEntry>,
}

impl VfsDirectory {
    pub fn entry(&self, name: &str) -> Option<&VfsEntry> {
        self.entries.get(name)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VfsEntry {
    File(VfsFile),
    Directory(VfsDirectory),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VfsFile {
    pub blob_ref: BlobRef,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    pub executable: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VfsTotals {
    pub files: u64,
    pub bytes: u64,
}

fn validate_directory(directory: &VfsDirectory, totals: &mut VfsTotals) -> Result<(), VfsError> {
    for (name, entry) in &directory.entries {
        validate_name(name)?;
        match entry {
            VfsEntry::File(file) => {
                totals.files += 1;
                totals.bytes =
                    totals
                        .bytes
                        .checked_add(file.size_bytes)
                        .ok_or(VfsError::InvalidManifest {
                            message: "file byte total overflowed u64".to_owned(),
                        })?;
            }
            VfsEntry::Directory(directory) => validate_directory(directory, totals)?,
        }
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), VfsError> {
    if name.is_empty() || name == "." || name == ".." || name.as_bytes().contains(&0) {
        return Err(VfsError::InvalidManifest {
            message: format!("invalid directory entry name '{name}'"),
        });
    }
    if name.contains('/') {
        return Err(VfsError::InvalidManifest {
            message: format!("directory entry name contains '/': '{name}'"),
        });
    }
    Ok(())
}
