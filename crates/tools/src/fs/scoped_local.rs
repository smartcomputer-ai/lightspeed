//! Secure scoped local host filesystem implementation.

use std::{
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;

use crate::fs::{
    CreateDirectoryOptions, FileAccessPolicy, FileMetadata, FileSystem, FsError, FsPath, FsResult,
    ReadDirectoryEntry, RemoveOptions, RenameOptions,
};

#[derive(Clone, Debug)]
pub struct ScopedLocalFileSystem {
    root: PathBuf,
    writable: bool,
}

impl ScopedLocalFileSystem {
    pub fn read_write(root: impl AsRef<Path>) -> FsResult<Self> {
        Self::new(root, true)
    }

    pub fn read_only(root: impl AsRef<Path>) -> FsResult<Self> {
        Self::new(root, false)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn new(root: impl AsRef<Path>, writable: bool) -> FsResult<Self> {
        let root = std::fs::canonicalize(root.as_ref())
            .map_err(|error| map_root_io_error(root.as_ref().display().to_string(), error))?;
        if !root.is_dir() {
            return Err(FsError::InvalidInput {
                message: format!(
                    "scoped local filesystem root is not a directory: {}",
                    root.display()
                ),
            });
        }
        Ok(Self { root, writable })
    }

    fn unresolved_host_path(&self, path: &FsPath) -> FsResult<PathBuf> {
        if path.is_relative() && path.has_unresolved_parent() {
            return Err(FsError::PermissionDenied { path: path.clone() });
        }
        if path.is_root() {
            return Ok(self.root.clone());
        }
        Ok(self.root.join(path.to_relative_path_buf()))
    }

    fn resolve_existing(&self, path: &FsPath) -> FsResult<PathBuf> {
        let host_path = self.unresolved_host_path(path)?;
        let resolved =
            std::fs::canonicalize(&host_path).map_err(|error| map_io_error(path, error))?;
        self.ensure_inside_root(path, resolved)
    }

    fn resolve_mutation_target(&self, path: &FsPath) -> FsResult<PathBuf> {
        if !self.writable {
            return Err(FsError::PermissionDenied { path: path.clone() });
        }
        if path.is_root() {
            return Err(FsError::InvalidInput {
                message: "cannot mutate scoped local filesystem root".to_string(),
            });
        }

        let host_path = self.unresolved_host_path(path)?;
        match std::fs::symlink_metadata(&host_path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    let resolved = std::fs::canonicalize(&host_path)
                        .map_err(|error| map_io_error(path, error))?;
                    self.ensure_inside_root(path, resolved)?;
                    return Ok(host_path);
                }
                let resolved =
                    std::fs::canonicalize(&host_path).map_err(|error| map_io_error(path, error))?;
                self.ensure_inside_root(path, resolved)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.ensure_parent_inside_root(path, &host_path)?;
                Ok(host_path)
            }
            Err(error) => Err(map_io_error(path, error)),
        }
    }

    fn ensure_parent_inside_root(&self, path: &FsPath, host_path: &Path) -> FsResult<()> {
        let parent = host_path.parent().ok_or_else(|| FsError::InvalidInput {
            message: format!("path has no parent: {path}"),
        })?;
        let resolved_parent =
            std::fs::canonicalize(parent).map_err(|error| map_io_error(path, error))?;
        self.ensure_inside_root(path, resolved_parent)?;
        Ok(())
    }

    fn ensure_existing_ancestor_inside_root(
        &self,
        path: &FsPath,
        host_path: &Path,
    ) -> FsResult<()> {
        let mut candidate = host_path.parent().ok_or_else(|| FsError::InvalidInput {
            message: format!("path has no parent: {path}"),
        })?;
        loop {
            match std::fs::symlink_metadata(candidate) {
                Ok(_) => {
                    let resolved = std::fs::canonicalize(candidate)
                        .map_err(|error| map_io_error(path, error))?;
                    self.ensure_inside_root(path, resolved)?;
                    return Ok(());
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    candidate = candidate
                        .parent()
                        .ok_or_else(|| FsError::NotFound { path: path.clone() })?;
                }
                Err(error) => return Err(map_io_error(path, error)),
            }
        }
    }

    fn ensure_inside_root(&self, path: &FsPath, host_path: PathBuf) -> FsResult<PathBuf> {
        if host_path.starts_with(&self.root) {
            Ok(host_path)
        } else {
            Err(FsError::PermissionDenied { path: path.clone() })
        }
    }
}

#[async_trait]
impl FileSystem for ScopedLocalFileSystem {
    fn access_policy(&self) -> FileAccessPolicy {
        if self.writable {
            FileAccessPolicy::ScopedReadWrite {
                root: FsPath::root(),
            }
        } else {
            FileAccessPolicy::ScopedReadOnly {
                root: FsPath::root(),
            }
        }
    }

    async fn read_file(&self, path: &FsPath) -> FsResult<Vec<u8>> {
        let host_path = self.resolve_existing(path)?;
        tokio::fs::read(host_path)
            .await
            .map_err(|error| map_io_error(path, error))
    }

    async fn write_file(&self, path: &FsPath, contents: Vec<u8>) -> FsResult<()> {
        let host_path = self.resolve_mutation_target(path)?;
        tokio::fs::write(host_path, contents)
            .await
            .map_err(|error| map_io_error(path, error))
    }

    async fn create_directory(
        &self,
        path: &FsPath,
        options: CreateDirectoryOptions,
    ) -> FsResult<()> {
        let host_path = self.unresolved_host_path(path)?;
        if !self.writable {
            return Err(FsError::PermissionDenied { path: path.clone() });
        }
        if path.is_root() {
            return if options.recursive {
                Ok(())
            } else {
                Err(FsError::AlreadyExists { path: path.clone() })
            };
        }

        if std::fs::symlink_metadata(&host_path).is_ok() {
            self.resolve_mutation_target(path)?;
        } else if options.recursive {
            self.ensure_existing_ancestor_inside_root(path, &host_path)?;
        } else {
            self.ensure_parent_inside_root(path, &host_path)?;
        }

        let result = if options.recursive {
            tokio::fs::create_dir_all(host_path).await
        } else {
            tokio::fs::create_dir(host_path).await
        };
        result.map_err(|error| map_io_error(path, error))
    }

    async fn get_metadata(&self, path: &FsPath) -> FsResult<FileMetadata> {
        let unresolved_host_path = self.unresolved_host_path(path)?;
        let host_path = self.resolve_existing(path)?;
        let metadata = tokio::fs::metadata(&host_path)
            .await
            .map_err(|error| map_io_error(path, error))?;
        let symlink_metadata = tokio::fs::symlink_metadata(unresolved_host_path)
            .await
            .map_err(|error| map_io_error(path, error))?;

        Ok(FileMetadata {
            is_directory: metadata.is_dir(),
            is_file: metadata.is_file(),
            is_symlink: symlink_metadata.file_type().is_symlink(),
            created_at_ms: metadata.created().ok().map_or(0, system_time_to_unix_ms),
            modified_at_ms: metadata.modified().ok().map_or(0, system_time_to_unix_ms),
        })
    }

    async fn read_directory(&self, path: &FsPath) -> FsResult<Vec<ReadDirectoryEntry>> {
        let host_path = self.resolve_existing(path)?;
        let mut entries = Vec::new();
        let mut read_dir = tokio::fs::read_dir(host_path)
            .await
            .map_err(|error| map_io_error(path, error))?;

        while let Some(entry) = read_dir
            .next_entry()
            .await
            .map_err(|error| map_io_error(path, error))?
        {
            let Ok(metadata) = tokio::fs::symlink_metadata(entry.path()).await else {
                continue;
            };
            let file_type = metadata.file_type();
            entries.push(ReadDirectoryEntry {
                file_name: entry.file_name().to_string_lossy().into_owned(),
                is_directory: file_type.is_dir(),
                is_file: file_type.is_file(),
            });
        }

        entries.sort_by(|left, right| left.file_name.cmp(&right.file_name));
        Ok(entries)
    }

    async fn remove(&self, path: &FsPath, options: RemoveOptions) -> FsResult<()> {
        if !self.writable {
            return Err(FsError::PermissionDenied { path: path.clone() });
        }
        if path.is_root() {
            return Err(FsError::InvalidInput {
                message: "cannot remove scoped local filesystem root".to_string(),
            });
        }

        let host_path = self.unresolved_host_path(path)?;
        self.ensure_parent_inside_root(path, &host_path)?;
        let metadata = match tokio::fs::symlink_metadata(&host_path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound && options.force => {
                return Ok(());
            }
            Err(error) => return Err(map_io_error(path, error)),
        };

        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            return tokio::fs::remove_file(host_path)
                .await
                .map_err(|error| map_io_error(path, error));
        }

        let resolved = self.resolve_existing(path)?;
        if file_type.is_dir() {
            if options.recursive {
                tokio::fs::remove_dir_all(resolved).await
            } else {
                tokio::fs::remove_dir(resolved).await
            }
        } else {
            tokio::fs::remove_file(resolved).await
        }
        .map_err(|error| map_io_error(path, error))
    }

    async fn rename(
        &self,
        source_path: &FsPath,
        destination_path: &FsPath,
        options: RenameOptions,
    ) -> FsResult<()> {
        if !self.writable {
            return Err(FsError::PermissionDenied {
                path: source_path.clone(),
            });
        }
        if source_path.is_root() {
            return Err(FsError::InvalidInput {
                message: "cannot move scoped local filesystem root".to_string(),
            });
        }
        let source_host_path = self.unresolved_host_path(source_path)?;
        self.ensure_parent_inside_root(source_path, &source_host_path)?;
        let destination_host_path = self.resolve_mutation_target(destination_path)?;
        crate::fs::local::rename_path(
            source_path,
            destination_path,
            &source_host_path,
            &destination_host_path,
            options,
        )
        .await
    }
}

fn system_time_to_unix_ms(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn map_root_io_error(path: String, error: io::Error) -> FsError {
    match error.kind() {
        io::ErrorKind::NotFound => FsError::InvalidInput {
            message: format!("scoped local filesystem root not found: {path}"),
        },
        io::ErrorKind::PermissionDenied => FsError::PermissionDenied {
            path: FsPath::new(path).unwrap_or_else(|_| FsPath::current_dir()),
        },
        _ => FsError::Failed {
            message: error.to_string(),
        },
    }
}

fn map_io_error(path: &FsPath, error: io::Error) -> FsError {
    match error.kind() {
        io::ErrorKind::NotFound => FsError::NotFound { path: path.clone() },
        io::ErrorKind::AlreadyExists => FsError::AlreadyExists { path: path.clone() },
        io::ErrorKind::PermissionDenied => FsError::PermissionDenied { path: path.clone() },
        io::ErrorKind::InvalidInput => FsError::InvalidInput {
            message: error.to_string(),
        },
        io::ErrorKind::InvalidData => FsError::InvalidData {
            message: error.to_string(),
        },
        io::ErrorKind::Unsupported => FsError::Unsupported {
            message: error.to_string(),
        },
        _ => FsError::Failed {
            message: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_local_file_system_reads_and_writes_under_root() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let fs = ScopedLocalFileSystem::read_write(temp_dir.path()).expect("scoped local fs");

        fs.create_directory(
            &FsPath::new("nested").expect("dir path"),
            CreateDirectoryOptions::single(),
        )
        .await
        .expect("create dir");
        fs.write_file(
            &FsPath::new("/nested/file.txt").expect("file path"),
            b"hello".to_vec(),
        )
        .await
        .expect("write file");

        assert_eq!(
            fs.read_file_text(&FsPath::new("nested/file.txt").unwrap())
                .await
                .expect("read file"),
            "hello"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_local_file_system_rejects_parent_escape() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let fs = ScopedLocalFileSystem::read_write(temp_dir.path()).expect("scoped local fs");

        assert!(matches!(
            fs.read_file(&FsPath::new("../outside.txt").unwrap()).await,
            Err(FsError::PermissionDenied { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_local_file_system_creates_recursive_directories_under_root() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let fs = ScopedLocalFileSystem::read_write(temp_dir.path()).expect("scoped local fs");

        fs.create_directory(
            &FsPath::new("a/b/c").unwrap(),
            CreateDirectoryOptions::recursive(),
        )
        .await
        .expect("create dirs");

        assert!(temp_dir.path().join("a/b/c").is_dir());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_local_file_system_read_only_rejects_writes() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let fs = ScopedLocalFileSystem::read_only(temp_dir.path()).expect("scoped local fs");

        assert!(matches!(
            fs.write_file(&FsPath::new("file.txt").unwrap(), b"hello".to_vec())
                .await,
            Err(FsError::PermissionDenied { .. })
        ));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn scoped_local_file_system_rejects_read_through_symlink_escape() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let root = temp_dir.path().join("root");
        let outside = temp_dir.path().join("outside");
        std::fs::create_dir(&root).expect("create root");
        std::fs::create_dir(&outside).expect("create outside");
        std::fs::write(outside.join("secret.txt"), "secret").expect("write secret");
        std::os::unix::fs::symlink(&outside, root.join("outside_link")).expect("symlink");
        let fs = ScopedLocalFileSystem::read_write(&root).expect("scoped local fs");

        assert!(matches!(
            fs.read_file(&FsPath::new("outside_link/secret.txt").unwrap())
                .await,
            Err(FsError::PermissionDenied { .. })
        ));
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn scoped_local_file_system_rejects_write_through_symlink_escape() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let root = temp_dir.path().join("root");
        let outside = temp_dir.path().join("outside");
        std::fs::create_dir(&root).expect("create root");
        std::fs::create_dir(&outside).expect("create outside");
        std::os::unix::fs::symlink(&outside, root.join("outside_link")).expect("symlink");
        let fs = ScopedLocalFileSystem::read_write(&root).expect("scoped local fs");

        assert!(matches!(
            fs.write_file(
                &FsPath::new("outside_link/created.txt").unwrap(),
                b"created".to_vec()
            )
            .await,
            Err(FsError::PermissionDenied { .. })
        ));
        assert!(!outside.join("created.txt").exists());
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn scoped_local_file_system_can_remove_symlink_itself() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let root = temp_dir.path().join("root");
        let outside = temp_dir.path().join("outside");
        std::fs::create_dir(&root).expect("create root");
        std::fs::create_dir(&outside).expect("create outside");
        std::fs::write(outside.join("secret.txt"), "secret").expect("write secret");
        std::os::unix::fs::symlink(&outside, root.join("outside_link")).expect("symlink");
        let fs = ScopedLocalFileSystem::read_write(&root).expect("scoped local fs");

        fs.remove(&FsPath::new("outside_link").unwrap(), RemoveOptions::file())
            .await
            .expect("remove symlink");

        assert!(!root.join("outside_link").exists());
        assert!(outside.join("secret.txt").exists());
    }
}
