//! Direct local host filesystem implementation.

use std::{
    io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;

use crate::fs::{
    CopyOptions, CreateDirectoryOptions, FileAccessPolicy, FileMetadata, FileSystem, FsError,
    FsPath, FsResult, ReadDirectoryEntry, RemoveOptions,
};

#[derive(Clone, Debug)]
pub struct LocalFileSystem {
    cwd: PathBuf,
}

impl LocalFileSystem {
    pub fn full_access() -> FsResult<Self> {
        let cwd = std::env::current_dir().map_err(|error| FsError::Failed {
            message: format!("failed to resolve current directory: {error}"),
        })?;
        Self::with_cwd(cwd)
    }

    pub fn with_cwd(cwd: impl AsRef<Path>) -> FsResult<Self> {
        let cwd = std::fs::canonicalize(cwd.as_ref())
            .map_err(|error| map_root_io_error(cwd.as_ref().display().to_string(), error))?;
        if !cwd.is_dir() {
            return Err(FsError::InvalidInput {
                message: format!("local filesystem cwd is not a directory: {}", cwd.display()),
            });
        }
        Ok(Self { cwd })
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    fn host_path(&self, path: &FsPath) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else if path.is_root() {
            self.cwd.clone()
        } else {
            self.cwd.join(path.to_relative_path_buf())
        }
    }
}

#[async_trait]
impl FileSystem for LocalFileSystem {
    fn access_policy(&self) -> FileAccessPolicy {
        FileAccessPolicy::FullReadWrite
    }

    async fn read_file(&self, path: &FsPath) -> FsResult<Vec<u8>> {
        tokio::fs::read(self.host_path(path))
            .await
            .map_err(|error| map_io_error(path, error))
    }

    async fn write_file(&self, path: &FsPath, contents: Vec<u8>) -> FsResult<()> {
        tokio::fs::write(self.host_path(path), contents)
            .await
            .map_err(|error| map_io_error(path, error))
    }

    async fn create_directory(
        &self,
        path: &FsPath,
        options: CreateDirectoryOptions,
    ) -> FsResult<()> {
        let result = if options.recursive {
            tokio::fs::create_dir_all(self.host_path(path)).await
        } else {
            tokio::fs::create_dir(self.host_path(path)).await
        };
        result.map_err(|error| map_io_error(path, error))
    }

    async fn get_metadata(&self, path: &FsPath) -> FsResult<FileMetadata> {
        let host_path = self.host_path(path);
        let metadata = tokio::fs::metadata(&host_path)
            .await
            .map_err(|error| map_io_error(path, error))?;
        let symlink_metadata = tokio::fs::symlink_metadata(host_path)
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
        let mut entries = Vec::new();
        let mut read_dir = tokio::fs::read_dir(self.host_path(path))
            .await
            .map_err(|error| map_io_error(path, error))?;

        while let Some(entry) = read_dir
            .next_entry()
            .await
            .map_err(|error| map_io_error(path, error))?
        {
            let Ok(metadata) = entry.metadata().await else {
                continue;
            };
            entries.push(ReadDirectoryEntry {
                file_name: entry.file_name().to_string_lossy().into_owned(),
                is_directory: metadata.is_dir(),
                is_file: metadata.is_file(),
            });
        }

        entries.sort_by(|left, right| left.file_name.cmp(&right.file_name));
        Ok(entries)
    }

    async fn remove(&self, path: &FsPath, options: RemoveOptions) -> FsResult<()> {
        let host_path = self.host_path(path);
        let metadata = match tokio::fs::symlink_metadata(&host_path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound && options.force => {
                return Ok(());
            }
            Err(error) => return Err(map_io_error(path, error)),
        };

        let file_type = metadata.file_type();
        if file_type.is_dir() {
            if options.recursive {
                tokio::fs::remove_dir_all(host_path).await
            } else {
                tokio::fs::remove_dir(host_path).await
            }
        } else {
            tokio::fs::remove_file(host_path).await
        }
        .map_err(|error| map_io_error(path, error))
    }

    async fn copy(
        &self,
        source_path: &FsPath,
        destination_path: &FsPath,
        options: CopyOptions,
    ) -> FsResult<()> {
        copy_path(
            source_path,
            destination_path,
            &self.host_path(source_path),
            &self.host_path(destination_path),
            options,
        )
    }
}

fn copy_path(
    source_fs_path: &FsPath,
    destination_fs_path: &FsPath,
    source_host_path: &Path,
    destination_host_path: &Path,
    options: CopyOptions,
) -> FsResult<()> {
    let metadata = std::fs::symlink_metadata(source_host_path)
        .map_err(|error| map_io_error(source_fs_path, error))?;
    let file_type = metadata.file_type();

    if file_type.is_symlink() {
        return Err(FsError::Unsupported {
            message: "copying symlinks is not supported by LocalFileSystem".to_string(),
        });
    }

    if file_type.is_dir() {
        if !options.recursive {
            return Err(FsError::InvalidInput {
                message: "copy requires recursive: true when source is a directory".to_string(),
            });
        }
        return copy_dir_recursive(source_host_path, destination_host_path)
            .map_err(|error| map_io_error(destination_fs_path, error));
    }

    if file_type.is_file() {
        std::fs::copy(source_host_path, destination_host_path)
            .map(|_| ())
            .map_err(|error| map_io_error(destination_fs_path, error))?;
        return Ok(());
    }

    Err(FsError::Unsupported {
        message: format!("unsupported source file type: {}", source_fs_path),
    })
}

fn copy_dir_recursive(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::create_dir(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&source_path)?;
        if metadata.is_dir() {
            copy_dir_recursive(&source_path, &destination_path)?;
        } else if metadata.is_file() {
            std::fs::copy(&source_path, &destination_path)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("unsupported file type: {}", source_path.display()),
            ));
        }
    }
    Ok(())
}

fn system_time_to_unix_ms(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn map_root_io_error(path: String, error: io::Error) -> FsError {
    match error.kind() {
        io::ErrorKind::NotFound => FsError::InvalidInput {
            message: format!("local filesystem cwd not found: {path}"),
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
    async fn local_file_system_relative_paths_resolve_against_cwd() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let fs = LocalFileSystem::with_cwd(temp_dir.path()).expect("local fs");
        let path = FsPath::new("nested/file.txt").expect("fs path");

        fs.create_directory(
            &FsPath::new("nested").expect("dir path"),
            CreateDirectoryOptions::single(),
        )
        .await
        .expect("create dir");
        fs.write_file(&path, b"hello".to_vec())
            .await
            .expect("write file");

        assert_eq!(fs.read_file_text(&path).await.expect("read file"), "hello");
    }
}
