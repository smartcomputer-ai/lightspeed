//! Session filesystem router over generic filesystem backends.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::fs::{
    CopyOptions, CreateDirectoryOptions, FileAccessPolicy, FileMetadata, FileSystem, FsError,
    FsPath, FsResult, ReadDirectoryEntry, RemoveOptions,
};

#[derive(Clone)]
pub struct SessionFileSystem {
    routes: Arc<Vec<SessionFileSystemRoute>>,
}

#[derive(Clone)]
pub struct SessionFileSystemRoute {
    mount_path: FsPath,
    fs: Arc<dyn FileSystem>,
    metadata: SessionFileSystemRouteMetadata,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionFileSystemRouteMetadata {
    pub mount_path: FsPath,
    pub access: FileAccessPolicy,
    pub source: SessionFileSystemRouteSource,
    pub same_state_as_active_env: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionFileSystemRouteSource {
    VfsSnapshot,
    VfsWorkspace,
    EnvironmentFilesystem { environment_id: String },
    Other { label: String },
}

#[derive(Clone)]
struct ResolvedSessionRoute {
    route: SessionFileSystemRoute,
    inner_path: FsPath,
}

impl SessionFileSystem {
    pub fn new(mut routes: Vec<SessionFileSystemRoute>) -> FsResult<Self> {
        validate_routes(&routes)?;
        routes.sort_by(|left, right| {
            right
                .mount_path
                .segments()
                .count()
                .cmp(&left.mount_path.segments().count())
                .then_with(|| left.mount_path.cmp(&right.mount_path))
        });
        Ok(Self {
            routes: Arc::new(routes),
        })
    }

    pub fn routes(&self) -> &[SessionFileSystemRoute] {
        self.routes.as_slice()
    }

    pub fn route_metadata_for_path(
        &self,
        path: &FsPath,
    ) -> FsResult<Option<SessionFileSystemRouteMetadata>> {
        Ok(self
            .resolve_route(path)?
            .map(|resolved| resolved.route.metadata))
    }

    fn resolve_route(&self, path: &FsPath) -> FsResult<Option<ResolvedSessionRoute>> {
        let path = normalize_route_path(path)?;
        for route in self.routes.iter() {
            if path.starts_with(&route.mount_path) {
                return Ok(Some(ResolvedSessionRoute {
                    route: route.clone(),
                    inner_path: strip_route_path(&path, &route.mount_path)?,
                }));
            }
        }
        Ok(None)
    }

    fn synthetic_directory_entries(&self, path: &FsPath) -> FsResult<Vec<ReadDirectoryEntry>> {
        let path = normalize_route_path(path)?;
        let mut entries = BTreeMap::new();
        for route in self.routes.iter() {
            if let Some(file_name) = immediate_route_child(&path, &route.mount_path) {
                entries.insert(
                    file_name.to_owned(),
                    ReadDirectoryEntry {
                        file_name: file_name.to_owned(),
                        is_directory: true,
                        is_file: false,
                    },
                );
            }
        }
        Ok(entries.into_values().collect())
    }

    fn synthetic_metadata(&self, path: &FsPath) -> FsResult<Option<FileMetadata>> {
        if self.synthetic_directory_entries(path)?.is_empty() {
            return Ok(None);
        }
        Ok(Some(directory_metadata()))
    }

    async fn copy_generic(
        &self,
        source_path: &FsPath,
        destination_path: &FsPath,
        options: CopyOptions,
    ) -> FsResult<()> {
        let source_path = normalize_route_path(source_path)?;
        let destination_path = normalize_route_path(destination_path)?;
        let source_metadata = self.get_metadata(&source_path).await?;
        if source_metadata.is_file {
            let bytes = self.read_file(&source_path).await?;
            return self.write_file(&destination_path, bytes).await;
        }
        if !source_metadata.is_directory {
            return Err(FsError::InvalidInput {
                message: format!("path is neither a file nor a directory: {source_path}"),
            });
        }
        if !options.recursive {
            return Err(FsError::InvalidInput {
                message: "copy requires recursive: true when source is a directory".to_owned(),
            });
        }
        if destination_path.starts_with(&source_path) {
            return Err(FsError::InvalidInput {
                message: "cannot copy a directory to itself or one of its descendants".to_owned(),
            });
        }
        self.copy_directory_generic(&source_path, &destination_path)
            .await
    }

    async fn copy_directory_generic(
        &self,
        source_path: &FsPath,
        destination_path: &FsPath,
    ) -> FsResult<()> {
        let mut stack = vec![(source_path.clone(), destination_path.clone(), false)];
        while let Some((source, destination, visited)) = stack.pop() {
            if visited {
                let bytes = self.read_file(&source).await?;
                self.write_file(&destination, bytes).await?;
                continue;
            }

            let metadata = self.get_metadata(&source).await?;
            if metadata.is_file {
                stack.push((source, destination, true));
                continue;
            }

            self.remove(&destination, RemoveOptions::recursive().force())
                .await?;
            self.create_directory(&destination, CreateDirectoryOptions::single())
                .await?;

            let mut entries = self.read_directory(&source).await?;
            entries.sort_by(|left, right| right.file_name.cmp(&left.file_name));
            for entry in entries {
                let source_child = source.join(&entry.file_name)?;
                let destination_child = destination.join(&entry.file_name)?;
                stack.push((source_child, destination_child, entry.is_file));
            }
        }
        Ok(())
    }
}

impl SessionFileSystemRoute {
    pub fn new(
        mount_path: FsPath,
        fs: Arc<dyn FileSystem>,
        source: SessionFileSystemRouteSource,
        same_state_as_active_env: bool,
    ) -> FsResult<Self> {
        let mount_path = normalize_route_path(&mount_path)?;
        Ok(Self {
            metadata: SessionFileSystemRouteMetadata {
                mount_path: mount_path.clone(),
                access: fs.access_policy(),
                source,
                same_state_as_active_env,
            },
            mount_path,
            fs,
        })
    }

    pub fn mount_path(&self) -> &FsPath {
        &self.mount_path
    }

    pub fn file_system(&self) -> Arc<dyn FileSystem> {
        self.fs.clone()
    }

    pub fn metadata(&self) -> &SessionFileSystemRouteMetadata {
        &self.metadata
    }
}

#[async_trait]
impl FileSystem for SessionFileSystem {
    fn access_policy(&self) -> FileAccessPolicy {
        if self
            .routes
            .iter()
            .any(|route| !route.fs.access_policy().is_read_only())
        {
            FileAccessPolicy::FullReadWrite
        } else {
            FileAccessPolicy::FullReadOnly
        }
    }

    async fn read_file(&self, path: &FsPath) -> FsResult<Vec<u8>> {
        if let Some(resolved) = self.resolve_route(path)? {
            return resolved.route.fs.read_file(&resolved.inner_path).await;
        }
        if self.synthetic_metadata(path)?.is_some() {
            return Err(FsError::InvalidInput {
                message: format!("path is not a file: {path}"),
            });
        }
        Err(FsError::NotFound { path: path.clone() })
    }

    async fn write_file(&self, path: &FsPath, contents: Vec<u8>) -> FsResult<()> {
        let Some(resolved) = self.resolve_route(path)? else {
            return Err(FsError::PermissionDenied { path: path.clone() });
        };
        resolved
            .route
            .fs
            .write_file(&resolved.inner_path, contents)
            .await
    }

    async fn create_directory(
        &self,
        path: &FsPath,
        options: CreateDirectoryOptions,
    ) -> FsResult<()> {
        if let Some(resolved) = self.resolve_route(path)? {
            return resolved
                .route
                .fs
                .create_directory(&resolved.inner_path, options)
                .await;
        }
        if self.synthetic_metadata(path)?.is_some() {
            return if options.recursive {
                Ok(())
            } else {
                Err(FsError::AlreadyExists { path: path.clone() })
            };
        }
        Err(FsError::PermissionDenied { path: path.clone() })
    }

    async fn get_metadata(&self, path: &FsPath) -> FsResult<FileMetadata> {
        if let Some(resolved) = self.resolve_route(path)? {
            return resolved.route.fs.get_metadata(&resolved.inner_path).await;
        }
        if let Some(metadata) = self.synthetic_metadata(path)? {
            return Ok(metadata);
        }
        Err(FsError::NotFound { path: path.clone() })
    }

    async fn read_directory(&self, path: &FsPath) -> FsResult<Vec<ReadDirectoryEntry>> {
        if let Some(resolved) = self.resolve_route(path)? {
            return resolved.route.fs.read_directory(&resolved.inner_path).await;
        }
        let entries = self.synthetic_directory_entries(path)?;
        if !entries.is_empty() {
            return Ok(entries);
        }
        Err(FsError::NotFound { path: path.clone() })
    }

    async fn remove(&self, path: &FsPath, options: RemoveOptions) -> FsResult<()> {
        let Some(resolved) = self.resolve_route(path)? else {
            return Err(FsError::PermissionDenied { path: path.clone() });
        };
        resolved
            .route
            .fs
            .remove(&resolved.inner_path, options)
            .await
    }

    async fn copy(
        &self,
        source_path: &FsPath,
        destination_path: &FsPath,
        options: CopyOptions,
    ) -> FsResult<()> {
        if let (Some(source), Some(destination)) = (
            self.resolve_route(source_path)?,
            self.resolve_route(destination_path)?,
        ) && source.route.mount_path == destination.route.mount_path
        {
            return destination
                .route
                .fs
                .copy(&source.inner_path, &destination.inner_path, options)
                .await;
        }
        self.copy_generic(source_path, destination_path, options)
            .await
    }

    fn drain_tool_effects(&self) -> Vec<engine::ToolEffect> {
        self.routes
            .iter()
            .flat_map(|route| route.fs.drain_tool_effects())
            .collect()
    }
}

fn validate_routes(routes: &[SessionFileSystemRoute]) -> FsResult<()> {
    let mut seen = BTreeSet::new();
    for route in routes {
        if !seen.insert(route.mount_path.clone()) {
            return Err(FsError::InvalidInput {
                message: format!("duplicate session filesystem route: {}", route.mount_path),
            });
        }
        if route.mount_path.is_relative() {
            return Err(FsError::InvalidInput {
                message: format!(
                    "session filesystem route must be absolute: {}",
                    route.mount_path
                ),
            });
        }
    }
    Ok(())
}

fn normalize_route_path(path: &FsPath) -> FsResult<FsPath> {
    if path.is_absolute() {
        Ok(path.clone())
    } else if path.is_root() {
        Ok(FsPath::root())
    } else {
        FsPath::new(format!("/{}", path.as_str())).map_err(Into::into)
    }
}

fn strip_route_path(path: &FsPath, mount_path: &FsPath) -> FsResult<FsPath> {
    if mount_path.is_root() {
        return Ok(path.clone());
    }
    if path == mount_path {
        return Ok(FsPath::root());
    }
    let suffix = path
        .as_str()
        .strip_prefix(mount_path.as_str())
        .ok_or_else(|| FsError::InvalidInput {
            message: format!("path {path} is not under route {mount_path}"),
        })?;
    FsPath::new(suffix).map_err(Into::into)
}

fn immediate_route_child<'a>(parent: &FsPath, mount_path: &'a FsPath) -> Option<&'a str> {
    let parent_segments = parent.segments().collect::<Vec<_>>();
    let mount_segments = mount_path.segments().collect::<Vec<_>>();
    if parent_segments.len() >= mount_segments.len() {
        return None;
    }
    if parent_segments
        .iter()
        .zip(mount_segments.iter())
        .all(|(left, right)| left == right)
    {
        Some(mount_segments[parent_segments.len()])
    } else {
        None
    }
}

fn directory_metadata() -> FileMetadata {
    FileMetadata {
        is_directory: true,
        is_file: false,
        is_symlink: false,
        created_at_ms: 0,
        modified_at_ms: 0,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::fs::{InMemoryFileSystem, ReadOnlyFileSystem};

    fn route(mount_path: &str, fs: Arc<dyn FileSystem>, label: &str) -> SessionFileSystemRoute {
        SessionFileSystemRoute::new(
            FsPath::new(mount_path).expect("mount path"),
            fs,
            SessionFileSystemRouteSource::Other {
                label: label.to_owned(),
            },
            false,
        )
        .expect("route")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_file_system_routes_by_deepest_prefix() {
        let root = InMemoryFileSystem::full_access();
        root.write_file(&FsPath::new("/file.txt").unwrap(), b"root".to_vec())
            .await
            .unwrap();
        let nested = InMemoryFileSystem::full_access();
        nested
            .write_file(&FsPath::new("/file.txt").unwrap(), b"nested".to_vec())
            .await
            .unwrap();

        let fs = SessionFileSystem::new(vec![
            route("/workspace", Arc::new(root), "root"),
            route("/workspace/project", Arc::new(nested), "nested"),
        ])
        .unwrap();

        assert_eq!(
            fs.read_file_text(&FsPath::new("/workspace/file.txt").unwrap())
                .await
                .unwrap(),
            "root"
        );
        assert_eq!(
            fs.read_file_text(&FsPath::new("/workspace/project/file.txt").unwrap())
                .await
                .unwrap(),
            "nested"
        );
        let metadata = fs
            .route_metadata_for_path(&FsPath::new("/workspace/project/file.txt").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            metadata.mount_path,
            FsPath::new("/workspace/project").unwrap()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_file_system_lists_synthetic_route_directories() {
        let fs = SessionFileSystem::new(vec![
            route(
                "/skills",
                Arc::new(ReadOnlyFileSystem::new(InMemoryFileSystem::full_access())),
                "skills",
            ),
            route(
                "/workspace/project",
                Arc::new(InMemoryFileSystem::full_access()),
                "workspace",
            ),
        ])
        .unwrap();

        let root = fs.read_directory(&FsPath::root()).await.unwrap();
        assert_eq!(
            root.into_iter()
                .map(|entry| entry.file_name)
                .collect::<Vec<_>>(),
            ["skills", "workspace"]
        );
        let workspace = fs
            .read_directory(&FsPath::new("/workspace").unwrap())
            .await
            .unwrap();
        assert_eq!(workspace[0].file_name, "project");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_file_system_copies_across_routes() {
        let source = InMemoryFileSystem::full_access();
        source
            .write_file(&FsPath::new("/README.md").unwrap(), b"hello".to_vec())
            .await
            .unwrap();
        let destination = InMemoryFileSystem::full_access();

        let fs = SessionFileSystem::new(vec![
            route("/source", Arc::new(source), "source"),
            route("/destination", Arc::new(destination.clone()), "destination"),
        ])
        .unwrap();
        fs.copy(
            &FsPath::new("/source/README.md").unwrap(),
            &FsPath::new("/destination/README.md").unwrap(),
            CopyOptions::file(),
        )
        .await
        .unwrap();

        assert_eq!(
            destination
                .read_file_text(&FsPath::new("/README.md").unwrap())
                .await
                .unwrap(),
            "hello"
        );
    }
}
