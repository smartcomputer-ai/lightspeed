//! Deterministic in-memory filesystem for tests and abstract substrates.

use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;

use crate::fs::{
    CopyOptions, CreateDirectoryOptions, FileAccessPolicy, FileMetadata, FileSystem, FsError,
    FsPath, FsResult, ReadDirectoryEntry, RemoveOptions,
};

#[derive(Clone, Debug)]
pub struct InMemoryFileSystem {
    inner: Arc<RwLock<InMemoryFileSystemInner>>,
    policy: FileAccessPolicy,
}

#[derive(Debug)]
struct InMemoryFileSystemInner {
    nodes: BTreeMap<FsPath, Node>,
    next_timestamp_ms: i64,
}

#[derive(Clone, Debug)]
enum Node {
    Directory {
        metadata: FileMetadata,
    },
    File {
        contents: Vec<u8>,
        metadata: FileMetadata,
    },
}

impl InMemoryFileSystem {
    pub fn new(policy: FileAccessPolicy) -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(FsPath::root(), Node::directory(0));
        Self {
            inner: Arc::new(RwLock::new(InMemoryFileSystemInner {
                nodes,
                next_timestamp_ms: 1,
            })),
            policy,
        }
    }

    pub fn full_access() -> Self {
        Self::new(FileAccessPolicy::FullReadWrite)
    }

    fn read_inner(&self) -> FsResult<std::sync::RwLockReadGuard<'_, InMemoryFileSystemInner>> {
        self.inner.read().map_err(|error| FsError::Failed {
            message: format!("in-memory filesystem lock poisoned: {error}"),
        })
    }

    fn write_inner(&self) -> FsResult<std::sync::RwLockWriteGuard<'_, InMemoryFileSystemInner>> {
        self.inner.write().map_err(|error| FsError::Failed {
            message: format!("in-memory filesystem lock poisoned: {error}"),
        })
    }

    fn key(&self, path: &FsPath) -> FsResult<FsPath> {
        if path.is_absolute() {
            Ok(path.clone())
        } else if path.is_root() {
            Ok(FsPath::root())
        } else {
            FsPath::new(format!("/{}", path.as_str())).map_err(Into::into)
        }
    }

    fn ensure_can_read(&self, path: &FsPath) -> FsResult<()> {
        if self.policy.can_read_path(path) {
            Ok(())
        } else {
            Err(FsError::PermissionDenied { path: path.clone() })
        }
    }

    fn ensure_can_write(&self, path: &FsPath) -> FsResult<()> {
        if self.policy.can_write_path(path) {
            Ok(())
        } else {
            Err(FsError::PermissionDenied { path: path.clone() })
        }
    }
}

impl Default for InMemoryFileSystem {
    fn default() -> Self {
        Self::full_access()
    }
}

#[async_trait]
impl FileSystem for InMemoryFileSystem {
    fn access_policy(&self) -> FileAccessPolicy {
        self.policy.clone()
    }

    async fn read_file(&self, path: &FsPath) -> FsResult<Vec<u8>> {
        let path = self.key(path)?;
        self.ensure_can_read(&path)?;
        let inner = self.read_inner()?;
        match inner.nodes.get(&path) {
            Some(Node::File { contents, .. }) => Ok(contents.clone()),
            Some(Node::Directory { .. }) => Err(FsError::InvalidInput {
                message: format!("path is a directory: {path}"),
            }),
            None => Err(FsError::NotFound { path }),
        }
    }

    async fn write_file(&self, path: &FsPath, contents: Vec<u8>) -> FsResult<()> {
        let path = self.key(path)?;
        self.ensure_can_write(&path)?;
        let mut inner = self.write_inner()?;
        inner.write_file(&path, contents)
    }

    async fn create_directory(
        &self,
        path: &FsPath,
        options: CreateDirectoryOptions,
    ) -> FsResult<()> {
        let path = self.key(path)?;
        self.ensure_can_write(&path)?;
        let mut inner = self.write_inner()?;
        inner.create_directory(&path, options)
    }

    async fn get_metadata(&self, path: &FsPath) -> FsResult<FileMetadata> {
        let path = self.key(path)?;
        self.ensure_can_read(&path)?;
        let inner = self.read_inner()?;
        inner
            .nodes
            .get(&path)
            .map(Node::metadata)
            .ok_or(FsError::NotFound { path })
    }

    async fn read_directory(&self, path: &FsPath) -> FsResult<Vec<ReadDirectoryEntry>> {
        let path = self.key(path)?;
        self.ensure_can_read(&path)?;
        let inner = self.read_inner()?;
        match inner.nodes.get(&path) {
            Some(Node::Directory { .. }) => {}
            Some(Node::File { .. }) => {
                return Err(FsError::InvalidInput {
                    message: format!("path is not a directory: {path}"),
                });
            }
            None => return Err(FsError::NotFound { path }),
        }

        let mut entries = Vec::new();
        for (candidate_path, node) in &inner.nodes {
            if candidate_path == &path {
                continue;
            }
            if candidate_path.parent().as_ref() == Some(&path) {
                let Some(file_name) = candidate_path.file_name() else {
                    continue;
                };
                entries.push(ReadDirectoryEntry {
                    file_name: file_name.to_string(),
                    is_directory: node.is_directory(),
                    is_file: node.is_file(),
                });
            }
        }
        Ok(entries)
    }

    async fn remove(&self, path: &FsPath, options: RemoveOptions) -> FsResult<()> {
        let path = self.key(path)?;
        self.ensure_can_write(&path)?;
        let mut inner = self.write_inner()?;
        inner.remove(&path, options)
    }

    async fn copy(
        &self,
        source_path: &FsPath,
        destination_path: &FsPath,
        options: CopyOptions,
    ) -> FsResult<()> {
        let source_path = self.key(source_path)?;
        let destination_path = self.key(destination_path)?;
        self.ensure_can_read(&source_path)?;
        self.ensure_can_write(&destination_path)?;
        let mut inner = self.write_inner()?;
        inner.copy(&source_path, &destination_path, options)
    }
}

impl InMemoryFileSystemInner {
    fn write_file(&mut self, path: &FsPath, contents: Vec<u8>) -> FsResult<()> {
        self.ensure_parent_directory(path)?;
        if matches!(self.nodes.get(path), Some(Node::Directory { .. })) {
            return Err(FsError::InvalidInput {
                message: format!("path is a directory: {path}"),
            });
        }

        let timestamp = self.next_timestamp();
        self.nodes
            .insert(path.clone(), Node::file(contents, timestamp));
        self.touch_parent(path);
        Ok(())
    }

    fn create_directory(&mut self, path: &FsPath, options: CreateDirectoryOptions) -> FsResult<()> {
        if path == &FsPath::root() {
            return if options.recursive || self.nodes.contains_key(path) {
                Ok(())
            } else {
                Err(FsError::AlreadyExists { path: path.clone() })
            };
        }

        if options.recursive {
            let mut current = FsPath::root();
            for segment in path.segments() {
                current = current.join(segment)?;
                match self.nodes.get(&current) {
                    Some(Node::Directory { .. }) => {}
                    Some(Node::File { .. }) => {
                        return Err(FsError::InvalidInput {
                            message: format!("path component is a file: {current}"),
                        });
                    }
                    None => {
                        let timestamp = self.next_timestamp();
                        self.nodes
                            .insert(current.clone(), Node::directory(timestamp));
                    }
                }
            }
            self.touch_parent(path);
            return Ok(());
        }

        self.ensure_parent_directory(path)?;
        if self.nodes.contains_key(path) {
            return Err(FsError::AlreadyExists { path: path.clone() });
        }

        let timestamp = self.next_timestamp();
        self.nodes.insert(path.clone(), Node::directory(timestamp));
        self.touch_parent(path);
        Ok(())
    }

    fn remove(&mut self, path: &FsPath, options: RemoveOptions) -> FsResult<()> {
        if path == &FsPath::root() {
            return Err(FsError::InvalidInput {
                message: "cannot remove filesystem root".to_string(),
            });
        }
        if !self.nodes.contains_key(path) {
            return if options.force {
                Ok(())
            } else {
                Err(FsError::NotFound { path: path.clone() })
            };
        }

        if matches!(self.nodes.get(path), Some(Node::Directory { .. }))
            && !options.recursive
            && self.has_children(path)
        {
            return Err(FsError::InvalidInput {
                message: format!("directory is not empty: {path}"),
            });
        }

        let descendants = self
            .nodes
            .keys()
            .filter(|candidate| candidate.starts_with(path))
            .cloned()
            .collect::<Vec<_>>();
        for descendant in descendants {
            self.nodes.remove(&descendant);
        }
        self.touch_parent(path);
        Ok(())
    }

    fn copy(
        &mut self,
        source_path: &FsPath,
        destination_path: &FsPath,
        options: CopyOptions,
    ) -> FsResult<()> {
        let source = self
            .nodes
            .get(source_path)
            .cloned()
            .ok_or_else(|| FsError::NotFound {
                path: source_path.clone(),
            })?;
        self.ensure_parent_directory(destination_path)?;

        match source {
            Node::File { contents, .. } => self.write_file(destination_path, contents),
            Node::Directory { .. } => {
                if !options.recursive {
                    return Err(FsError::InvalidInput {
                        message: "copy requires recursive: true when source is a directory"
                            .to_string(),
                    });
                }
                if destination_path.starts_with(source_path) {
                    return Err(FsError::InvalidInput {
                        message: "cannot copy a directory to itself or one of its descendants"
                            .to_string(),
                    });
                }
                let entries = self
                    .nodes
                    .iter()
                    .filter(|(path, _)| path.starts_with(source_path))
                    .map(|(path, node)| (path.clone(), node.clone()))
                    .collect::<Vec<_>>();
                for (path, node) in entries {
                    let relative_segments = path
                        .segments()
                        .skip(source_path.segments().count())
                        .collect::<Vec<_>>();
                    let destination = destination_path.join_segments(relative_segments)?;
                    let timestamp = self.next_timestamp();
                    self.nodes
                        .insert(destination, node.with_timestamp(timestamp));
                }
                self.touch_parent(destination_path);
                Ok(())
            }
        }
    }

    fn ensure_parent_directory(&self, path: &FsPath) -> FsResult<()> {
        let Some(parent) = path.parent() else {
            return Ok(());
        };
        match self.nodes.get(&parent) {
            Some(Node::Directory { .. }) => Ok(()),
            Some(Node::File { .. }) => Err(FsError::InvalidInput {
                message: format!("parent path is a file: {parent}"),
            }),
            None => Err(FsError::NotFound { path: parent }),
        }
    }

    fn has_children(&self, path: &FsPath) -> bool {
        self.nodes
            .keys()
            .any(|candidate| candidate != path && candidate.starts_with(path))
    }

    fn touch_parent(&mut self, path: &FsPath) {
        let Some(parent) = path.parent() else {
            return;
        };
        let timestamp = self.next_timestamp();
        if let Some(Node::Directory { metadata }) = self.nodes.get_mut(&parent) {
            metadata.modified_at_ms = timestamp;
        }
    }

    fn next_timestamp(&mut self) -> i64 {
        let timestamp = self.next_timestamp_ms;
        self.next_timestamp_ms += 1;
        timestamp
    }
}

impl Node {
    fn directory(timestamp: i64) -> Self {
        Self::Directory {
            metadata: FileMetadata {
                is_directory: true,
                is_file: false,
                is_symlink: false,
                created_at_ms: timestamp,
                modified_at_ms: timestamp,
            },
        }
    }

    fn file(contents: Vec<u8>, timestamp: i64) -> Self {
        Self::File {
            contents,
            metadata: FileMetadata {
                is_directory: false,
                is_file: true,
                is_symlink: false,
                created_at_ms: timestamp,
                modified_at_ms: timestamp,
            },
        }
    }

    fn metadata(&self) -> FileMetadata {
        match self {
            Self::Directory { metadata } | Self::File { metadata, .. } => metadata.clone(),
        }
    }

    fn is_directory(&self) -> bool {
        matches!(self, Self::Directory { .. })
    }

    fn is_file(&self) -> bool {
        matches!(self, Self::File { .. })
    }

    fn with_timestamp(self, timestamp: i64) -> Self {
        match self {
            Self::Directory { .. } => Self::directory(timestamp),
            Self::File { contents, .. } => Self::file(contents, timestamp),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn in_memory_file_system_reads_and_writes_files() {
        let fs = InMemoryFileSystem::full_access();
        let path = FsPath::new("/src/lib.rs").expect("path");

        fs.create_directory(
            &FsPath::new("/src").expect("dir"),
            CreateDirectoryOptions::single(),
        )
        .await
        .expect("create dir");
        fs.write_file(&path, b"pub fn f() {}".to_vec())
            .await
            .expect("write");

        assert_eq!(
            fs.read_file_text(&path).await.expect("read"),
            "pub fn f() {}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn in_memory_file_system_enforces_read_only_policy() {
        let fs = InMemoryFileSystem::new(FileAccessPolicy::FullReadOnly);

        assert!(matches!(
            fs.write_file(&FsPath::new("/file.txt").unwrap(), b"hello".to_vec())
                .await,
            Err(FsError::PermissionDenied { .. })
        ));
    }
}
