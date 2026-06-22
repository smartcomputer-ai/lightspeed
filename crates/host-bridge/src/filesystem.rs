use std::{
    io,
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use host_protocol::{
    data::fs::{
        CopyParams, CopyResponse, CreateDirectoryParams, CreateDirectoryResponse,
        GetMetadataParams, GetMetadataResponse, ReadDirectoryEntry, ReadDirectoryParams,
        ReadDirectoryResponse, ReadFileParams, ReadFileResponse, RemoveParams, RemoveResponse,
        WriteFileParams, WriteFileResponse,
    },
    error::{HostError, HostErrorCode},
    shared::{ByteChunk, HostPath},
};
use tokio::fs;

#[derive(Clone)]
pub struct LocalFileSystem {
    root: PathBuf,
    cwd: PathBuf,
    writable: bool,
}

impl LocalFileSystem {
    pub fn new(root: PathBuf, cwd: PathBuf, writable: bool) -> Self {
        Self {
            root: normalize_path(root),
            cwd: normalize_path(cwd),
            writable,
        }
    }

    pub async fn read_file(&self, params: ReadFileParams) -> Result<ReadFileResponse, HostError> {
        let path = self.resolve(&params.path)?;
        let data = fs::read(&path)
            .await
            .map_err(|error| io_error(error, &path))?;
        Ok(ReadFileResponse {
            data: ByteChunk::from(data),
        })
    }

    pub async fn write_file(
        &self,
        params: WriteFileParams,
    ) -> Result<WriteFileResponse, HostError> {
        self.ensure_writable()?;
        let path = self.resolve(&params.path)?;
        fs::write(&path, params.data.into_inner())
            .await
            .map_err(|error| io_error(error, &path))?;
        Ok(WriteFileResponse {})
    }

    pub async fn create_directory(
        &self,
        params: CreateDirectoryParams,
    ) -> Result<CreateDirectoryResponse, HostError> {
        self.ensure_writable()?;
        let path = self.resolve(&params.path)?;
        if params.recursive.unwrap_or(false) {
            fs::create_dir_all(&path)
                .await
                .map_err(|error| io_error(error, &path))?;
        } else {
            fs::create_dir(&path)
                .await
                .map_err(|error| io_error(error, &path))?;
        }
        Ok(CreateDirectoryResponse {})
    }

    pub async fn get_metadata(
        &self,
        params: GetMetadataParams,
    ) -> Result<GetMetadataResponse, HostError> {
        let path = self.resolve(&params.path)?;
        let metadata = fs::symlink_metadata(&path)
            .await
            .map_err(|error| io_error(error, &path))?;
        let file_type = metadata.file_type();
        let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
        let created = metadata.created().unwrap_or(modified);
        Ok(GetMetadataResponse {
            is_directory: file_type.is_dir(),
            is_file: file_type.is_file(),
            is_symlink: file_type.is_symlink(),
            created_at_ms: system_time_ms(created),
            modified_at_ms: system_time_ms(modified),
        })
    }

    pub async fn read_directory(
        &self,
        params: ReadDirectoryParams,
    ) -> Result<ReadDirectoryResponse, HostError> {
        let path = self.resolve(&params.path)?;
        let mut directory = fs::read_dir(&path)
            .await
            .map_err(|error| io_error(error, &path))?;
        let mut entries = Vec::new();
        while let Some(entry) = directory
            .next_entry()
            .await
            .map_err(|error| io_error(error, &path))?
        {
            let metadata = entry
                .metadata()
                .await
                .map_err(|error| io_error(error, &entry.path()))?;
            entries.push(ReadDirectoryEntry {
                file_name: entry.file_name().to_string_lossy().into_owned(),
                is_directory: metadata.is_dir(),
                is_file: metadata.is_file(),
            });
        }
        entries.sort_by(|left, right| left.file_name.cmp(&right.file_name));
        Ok(ReadDirectoryResponse { entries })
    }

    pub async fn remove(&self, params: RemoveParams) -> Result<RemoveResponse, HostError> {
        self.ensure_writable()?;
        let path = self.resolve(&params.path)?;
        let metadata = match fs::symlink_metadata(&path).await {
            Ok(metadata) => metadata,
            Err(error)
                if params.force.unwrap_or(false) && error.kind() == io::ErrorKind::NotFound =>
            {
                return Ok(RemoveResponse {});
            }
            Err(error) => return Err(io_error(error, &path)),
        };
        if metadata.is_dir() {
            if params.recursive.unwrap_or(false) {
                fs::remove_dir_all(&path)
                    .await
                    .map_err(|error| io_error(error, &path))?;
            } else {
                fs::remove_dir(&path)
                    .await
                    .map_err(|error| io_error(error, &path))?;
            }
        } else {
            fs::remove_file(&path)
                .await
                .map_err(|error| io_error(error, &path))?;
        }
        Ok(RemoveResponse {})
    }

    pub async fn copy(&self, params: CopyParams) -> Result<CopyResponse, HostError> {
        self.ensure_writable()?;
        let source = self.resolve(&params.source_path)?;
        let destination = self.resolve(&params.destination_path)?;
        tokio::task::spawn_blocking(move || copy_path(&source, &destination, params.recursive))
            .await
            .map_err(|error| HostError::new(HostErrorCode::Internal, error.to_string()))??;
        Ok(CopyResponse {})
    }

    fn resolve(&self, path: &HostPath) -> Result<PathBuf, HostError> {
        let candidate = if path.is_absolute() {
            PathBuf::from(path.as_str())
        } else if path.as_str() == "." {
            self.cwd.clone()
        } else {
            self.cwd.join(path.as_str())
        };
        let normalized = normalize_path(candidate);
        if !normalized.starts_with(&self.root) {
            return Err(HostError::new(
                HostErrorCode::Forbidden,
                format!(
                    "path is outside bridge fs root: {} (root {})",
                    normalized.display(),
                    self.root.display()
                ),
            ));
        }
        Ok(normalized)
    }

    fn ensure_writable(&self) -> Result<(), HostError> {
        if self.writable {
            Ok(())
        } else {
            Err(HostError::new(
                HostErrorCode::CapabilityUnavailable,
                "bridge filesystem is read-only",
            ))
        }
    }
}

fn copy_path(source: &Path, destination: &Path, recursive: bool) -> Result<(), HostError> {
    let metadata = std::fs::symlink_metadata(source).map_err(|error| io_error(error, source))?;
    if metadata.is_dir() {
        if !recursive {
            return Err(HostError::new(
                HostErrorCode::InvalidRequest,
                "copy requires recursive=true when source is a directory",
            ));
        }
        copy_directory(source, destination)
    } else {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|error| io_error(error, parent))?;
        }
        std::fs::copy(source, destination)
            .map(|_| ())
            .map_err(|error| io_error(error, destination))
    }
}

fn copy_directory(source: &Path, destination: &Path) -> Result<(), HostError> {
    std::fs::create_dir_all(destination).map_err(|error| io_error(error, destination))?;
    for entry in std::fs::read_dir(source).map_err(|error| io_error(error, source))? {
        let entry = entry.map_err(|error| io_error(error, source))?;
        let source_child = entry.path();
        let destination_child = destination.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&source_child)
            .map_err(|error| io_error(error, &source_child))?;
        if metadata.is_dir() {
            copy_directory(&source_child, &destination_child)?;
        } else {
            std::fs::copy(&source_child, &destination_child)
                .map(|_| ())
                .map_err(|error| io_error(error, &destination_child))?;
        }
    }
    Ok(())
}

fn normalize_path(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(std::path::MAIN_SEPARATOR.to_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(segment) => normalized.push(segment),
        }
    }
    normalized
}

fn io_error(error: io::Error, path: &Path) -> HostError {
    let code = match error.kind() {
        io::ErrorKind::NotFound => HostErrorCode::NotFound,
        io::ErrorKind::PermissionDenied => HostErrorCode::Forbidden,
        io::ErrorKind::AlreadyExists => HostErrorCode::Conflict,
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => HostErrorCode::InvalidRequest,
        _ => HostErrorCode::Internal,
    };
    HostError::new(code, format!("{}: {}", path.display(), error))
}

fn system_time_ms(value: SystemTime) -> i64 {
    value
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn filesystem_reads_and_writes_under_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        let fs = LocalFileSystem::new(root.clone(), root.clone(), true);
        let path = HostPath::new(root.join("file.txt").to_string_lossy()).expect("host path");

        fs.write_file(WriteFileParams {
            path: path.clone(),
            data: ByteChunk::from(b"hello".as_slice()),
        })
        .await
        .expect("write");
        let read = fs
            .read_file(ReadFileParams { path })
            .await
            .expect("read")
            .data
            .into_inner();

        assert_eq!(read, b"hello");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn filesystem_rejects_root_escape() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("root");
        std::fs::create_dir(&root).expect("root");
        let root = root.canonicalize().expect("canonical root");
        let fs = LocalFileSystem::new(root, temp.path().to_path_buf(), true);
        let outside = HostPath::new(temp.path().join("outside.txt").to_string_lossy()).unwrap();

        let error = fs
            .read_file(ReadFileParams { path: outside })
            .await
            .expect_err("escape should fail");

        assert_eq!(error.code, HostErrorCode::Forbidden);
    }
}
