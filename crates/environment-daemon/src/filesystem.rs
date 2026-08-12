use std::{
    io,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use grep_searcher::{BinaryDetection, SearcherBuilder, sinks::UTF8};
use host_protocol::{
    data::fs::{
        CopyParams, CopyResponse, CreateDirectoryParams, CreateDirectoryResponse,
        GetMetadataParams, GetMetadataResponse, GlobFilesParams, GlobFilesResponse, GlobFilesStop,
        ReadDirectoryEntry, ReadDirectoryParams, ReadDirectoryResponse, ReadFileParams,
        ReadFileResponse, RemoveParams, RemoveResponse, SearchTextMatch, SearchTextParams,
        SearchTextResponse, SearchTextStop, WriteFileParams, WriteFileResponse,
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
        let metadata = fs::metadata(&path)
            .await
            .map_err(|error| io_error(error, &path))?;
        if !metadata.is_file() {
            // Preserve the exact outcome a plain read produces for non-file
            // paths (directories fail with the authentic io error).
            return match fs::read(&path).await {
                Ok(data) => Ok(ReadFileResponse {
                    file_size: Some(data.len() as u64),
                    data: ByteChunk::from(data),
                    truncated: false,
                }),
                Err(error) => Err(io_error(error, &path)),
            };
        }
        let file_size = metadata.len();
        let offset = params.offset.unwrap_or(0).min(file_size);
        let remaining = file_size - offset;
        let take = params.max_bytes.map_or(remaining, |max| max.min(remaining));

        let data = if offset == 0 && take == file_size {
            fs::read(&path)
                .await
                .map_err(|error| io_error(error, &path))?
        } else {
            use tokio::io::{AsyncReadExt, AsyncSeekExt};
            let mut file = fs::File::open(&path)
                .await
                .map_err(|error| io_error(error, &path))?;
            file.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(|error| io_error(error, &path))?;
            let mut data = Vec::with_capacity(take.min(64 * 1024 * 1024) as usize);
            file.take(take)
                .read_to_end(&mut data)
                .await
                .map_err(|error| io_error(error, &path))?;
            data
        };
        let truncated = offset.saturating_add(data.len() as u64) < file_size;
        Ok(ReadFileResponse {
            data: ByteChunk::from(data),
            file_size: Some(file_size),
            truncated,
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

    /// Bounded recursive text search executed in-process with the ripgrep
    /// engine crates. No external `rg` binary is involved, and the regex
    /// dialect matches the caller-side generic fallback.
    pub async fn search_text(
        &self,
        params: SearchTextParams,
    ) -> Result<SearchTextResponse, HostError> {
        let root = self.resolve(&params.root)?;
        tokio::task::spawn_blocking(move || search_text_blocking(root, params))
            .await
            .map_err(|error| HostError::new(HostErrorCode::Internal, error.to_string()))?
    }

    /// Bounded recursive file enumeration executed locally, so a broad glob
    /// does not become one `fs/readDirectory` round trip per directory.
    pub async fn glob_files(
        &self,
        params: GlobFilesParams,
    ) -> Result<GlobFilesResponse, HostError> {
        let root = self.resolve(&params.root)?;
        tokio::task::spawn_blocking(move || glob_files_blocking(root, params))
            .await
            .map_err(|error| HostError::new(HostErrorCode::Internal, error.to_string()))?
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

fn search_text_blocking(
    root: PathBuf,
    params: SearchTextParams,
) -> Result<SearchTextResponse, HostError> {
    if params.pattern.is_empty() {
        return Err(HostError::new(
            HostErrorCode::InvalidRequest,
            "search pattern must not be empty",
        ));
    }
    let limits = params.limits;
    if limits.max_matches == 0
        || limits.max_files == 0
        || limits.max_bytes == 0
        || limits.max_duration_ms == 0
    {
        return Err(HostError::new(
            HostErrorCode::InvalidRequest,
            "search limits must all be at least 1",
        ));
    }
    let matcher = grep_regex::RegexMatcherBuilder::new()
        .case_insensitive(!params.case_sensitive)
        .build(&params.pattern)
        .map_err(|error| {
            HostError::new(
                HostErrorCode::InvalidRequest,
                format!("invalid search regex: {error}"),
            )
        })?;
    let include = params
        .include
        .as_deref()
        .map(glob::Pattern::new)
        .transpose()
        .map_err(|error| {
            HostError::new(
                HostErrorCode::InvalidRequest,
                format!("invalid search include glob: {error}"),
            )
        })?;

    let started = Instant::now();
    let deadline = Duration::from_millis(limits.max_duration_ms);
    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .binary_detection(BinaryDetection::quit(0))
        .build();
    let mut walker = ignore::WalkBuilder::new(&root);
    // Mirror the generic traversal: no ignore-file or hidden filtering, no
    // symlink following, deterministic order. The generic walker's max_depth
    // counts directory descents below the root, so files at walker depth
    // `max_depth + 1` are still in range.
    walker
        .standard_filters(false)
        .follow_links(false)
        .sort_by_file_path(|left, right| left.cmp(right));
    if let Some(max_depth) = params.max_depth {
        walker.max_depth(Some((max_depth as usize).saturating_add(1)));
    }

    let mut matches = Vec::new();
    let mut files_searched = 0u64;
    let mut bytes_searched = 0u64;
    let mut stopped = None;

    'walk: for entry in walker.build() {
        // Unreadable entries are skipped rather than failing the search.
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        if let Some(include) = &include
            && !host_path_matches_include(include, path, &root)
        {
            continue;
        }
        if started.elapsed() >= deadline {
            stopped = Some(SearchTextStop::TimeLimit);
            break;
        }
        if files_searched >= limits.max_files {
            stopped = Some(SearchTextStop::FileLimit);
            break;
        }
        let file_len = entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        if bytes_searched.saturating_add(file_len) > limits.max_bytes {
            stopped = Some(SearchTextStop::ByteLimit);
            break;
        }
        files_searched += 1;
        bytes_searched = bytes_searched.saturating_add(file_len);

        let match_path = response_path(&params.root, path, &root)?;
        let mut hit_match_limit = false;
        let sink = UTF8(|line_number, line| {
            if matches.len() as u64 >= limits.max_matches {
                hit_match_limit = true;
                return Ok(false);
            }
            matches.push(SearchTextMatch {
                path: match_path.clone(),
                line_number,
                line: line.trim_end_matches(['\r', '\n']).to_string(),
            });
            Ok(true)
        });
        // Per-file search errors (e.g. the file vanished mid-walk) skip the
        // file rather than failing the whole search.
        let _ = searcher.search_path(&matcher, path, sink);
        // Like the generic fallback, the match limit reports truncation only
        // when a match beyond the cap actually arrived.
        if hit_match_limit {
            stopped = Some(SearchTextStop::MatchLimit);
            break 'walk;
        }
    }

    Ok(SearchTextResponse {
        matches,
        files_searched,
        bytes_searched,
        elapsed_ms: started.elapsed().as_millis() as u64,
        stopped,
    })
}

fn glob_files_blocking(
    root: PathBuf,
    params: GlobFilesParams,
) -> Result<GlobFilesResponse, HostError> {
    if params.pattern.is_empty() {
        return Err(HostError::new(
            HostErrorCode::InvalidRequest,
            "glob pattern must not be empty",
        ));
    }
    let limits = params.limits;
    if limits.max_matches == 0 || limits.max_entries == 0 || limits.max_duration_ms == 0 {
        return Err(HostError::new(
            HostErrorCode::InvalidRequest,
            "glob limits must all be at least 1",
        ));
    }
    let pattern = glob::Pattern::new(&params.pattern).map_err(|error| {
        HostError::new(
            HostErrorCode::InvalidRequest,
            format!("invalid glob pattern: {error}"),
        )
    })?;

    let started = Instant::now();
    let deadline = Duration::from_millis(limits.max_duration_ms);
    let mut walker = ignore::WalkBuilder::new(&root);
    walker
        .standard_filters(false)
        .follow_links(false)
        .sort_by_file_path(|left, right| left.cmp(right));
    if let Some(max_depth) = params.max_depth {
        walker.max_depth(Some((max_depth as usize).saturating_add(1)));
    }

    let mut matches = Vec::new();
    let mut entries_visited = 0u64;
    let mut stopped = None;

    for entry in walker.build() {
        let Ok(entry) = entry else { continue };
        if entry.depth() == 0 {
            continue;
        }
        if started.elapsed() >= deadline {
            stopped = Some(GlobFilesStop::TimeLimit);
            break;
        }
        if entries_visited >= limits.max_entries {
            stopped = Some(GlobFilesStop::EntryLimit);
            break;
        }
        entries_visited += 1;
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        let caller_path = response_path(&params.root, path, &root)?;
        if !glob_pattern_matches(&pattern, &params.pattern, &caller_path, path, &root) {
            continue;
        }
        // Like the generic fallback, the match limit reports truncation only
        // when a match beyond the cap actually arrives.
        if matches.len() as u64 >= limits.max_matches {
            stopped = Some(GlobFilesStop::MatchLimit);
            break;
        }
        matches.push(caller_path);
    }

    Ok(GlobFilesResponse {
        matches,
        entries_visited,
        elapsed_ms: started.elapsed().as_millis() as u64,
        stopped,
    })
}

/// Glob pattern semantics mirror the caller-side generic tool: an absolute
/// pattern matches the caller-space path; otherwise the root-relative path
/// matches, and a pattern without `/` also matches the bare file name.
fn glob_pattern_matches(
    pattern: &glob::Pattern,
    pattern_text: &str,
    caller_path: &HostPath,
    path: &Path,
    root: &Path,
) -> bool {
    if pattern_text.starts_with('/') {
        return pattern.matches(caller_path.as_str());
    }
    let relative = path
        .strip_prefix(root)
        .map(|relative| {
            relative
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/")
        })
        .unwrap_or_else(|_| path.to_string_lossy().into_owned());
    pattern.matches(&relative)
        || (!pattern_text.contains('/')
            && path
                .file_name()
                .is_some_and(|file_name| pattern.matches(&file_name.to_string_lossy())))
}

/// Include globs match the root-relative path or the bare file name, exactly
/// like the caller-side generic fallback.
fn host_path_matches_include(pattern: &glob::Pattern, path: &Path, root: &Path) -> bool {
    let relative = path
        .strip_prefix(root)
        .map(|relative| {
            relative
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/")
        })
        .unwrap_or_else(|_| path.to_string_lossy().into_owned());
    pattern.matches(&relative)
        || path
            .file_name()
            .is_some_and(|file_name| pattern.matches(&file_name.to_string_lossy()))
}

/// Map a matched local path back into the caller's path space by joining its
/// root-relative suffix onto the requested root.
fn response_path(
    requested_root: &HostPath,
    path: &Path,
    resolved_root: &Path,
) -> Result<HostPath, HostError> {
    let relative = path.strip_prefix(resolved_root).map_err(|_| {
        HostError::new(
            HostErrorCode::Internal,
            format!("search match escaped its root: {}", path.display()),
        )
    })?;
    let relative = relative
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    if relative.is_empty() {
        return Ok(requested_root.clone());
    }
    let joined = format!(
        "{}/{}",
        requested_root.as_str().trim_end_matches('/'),
        relative
    );
    HostPath::new(&joined).map_err(|error| {
        HostError::new(
            HostErrorCode::Internal,
            format!("search match produced an invalid host path {joined}: {error}"),
        )
    })
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
            .read_file(ReadFileParams {
                path,
                offset: None,
                max_bytes: None,
            })
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
            .read_file(ReadFileParams {
                path: outside,
                offset: None,
                max_bytes: None,
            })
            .await
            .expect_err("escape should fail");

        assert_eq!(error.code, HostErrorCode::Forbidden);
    }

    fn search_fixture() -> (tempfile::TempDir, LocalFileSystem, HostPath) {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().canonicalize().expect("canonical root");
        std::fs::create_dir_all(root.join("src/deep")).expect("dirs");
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn target() {}\nfn other() {}\n",
        )
        .expect("lib");
        std::fs::write(root.join("src/deep/nested.rs"), "target nested\n").expect("nested");
        std::fs::write(root.join("readme.md"), "target readme\n").expect("readme");
        std::fs::write(root.join("binary.bin"), b"tar\x00get\n").expect("binary");
        let fs = LocalFileSystem::new(root.clone(), root.clone(), false);
        let host_root = HostPath::new(root.to_string_lossy()).expect("host root");
        (temp, fs, host_root)
    }

    fn limits() -> host_protocol::data::fs::SearchTextLimits {
        host_protocol::data::fs::SearchTextLimits {
            max_matches: 100,
            max_files: 100,
            max_bytes: 1024 * 1024,
            max_duration_ms: 10_000,
        }
    }

    fn search_params(root: &HostPath) -> SearchTextParams {
        SearchTextParams {
            root: root.clone(),
            pattern: "target".to_owned(),
            include: None,
            case_sensitive: true,
            max_depth: None,
            limits: limits(),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn search_finds_matches_with_include_and_depth_semantics() {
        let (_temp, fs, root) = search_fixture();

        let mut params = search_params(&root);
        params.include = Some("*.rs".to_owned());
        let result = fs.search_text(params).await.expect("search");
        let mut paths = result
            .matches
            .iter()
            .map(|item| item.path.as_str().to_owned())
            .collect::<Vec<_>>();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                format!("{}/src/deep/nested.rs", root.as_str()),
                format!("{}/src/lib.rs", root.as_str()),
            ]
        );
        assert_eq!(result.matches[0].line_number, 1);
        assert!(result.stopped.is_none());
        // NUL-containing files are treated as binary and skipped, so the
        // binary fixture never matches.
        assert!(
            result
                .matches
                .iter()
                .all(|item| !item.path.as_str().ends_with(".bin"))
        );

        // max_depth 0 mirrors the generic traversal: files directly under
        // the root only.
        let mut params = search_params(&root);
        params.max_depth = Some(0);
        let shallow = fs.search_text(params).await.expect("shallow search");
        assert_eq!(shallow.matches.len(), 1);
        assert!(shallow.matches[0].path.as_str().ends_with("readme.md"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn search_stops_at_each_limit_with_a_typed_reason() {
        let (_temp, fs, root) = search_fixture();

        let mut params = search_params(&root);
        params.limits.max_matches = 1;
        let matches = fs.search_text(params).await.expect("match limit");
        assert_eq!(matches.matches.len(), 1);
        assert_eq!(matches.stopped, Some(SearchTextStop::MatchLimit));

        let mut params = search_params(&root);
        params.limits.max_files = 1;
        let files = fs.search_text(params).await.expect("file limit");
        assert_eq!(files.stopped, Some(SearchTextStop::FileLimit));
        assert_eq!(files.files_searched, 1);

        let mut params = search_params(&root);
        params.limits.max_bytes = 1;
        let bytes = fs.search_text(params).await.expect("byte limit");
        assert_eq!(bytes.stopped, Some(SearchTextStop::ByteLimit));
        assert!(bytes.matches.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn search_rejects_invalid_requests_and_root_escape() {
        let (temp, fs, root) = search_fixture();

        let mut params = search_params(&root);
        params.pattern = "(unclosed".to_owned();
        let error = fs.search_text(params).await.expect_err("invalid regex");
        assert_eq!(error.code, HostErrorCode::InvalidRequest);

        let mut params = search_params(&root);
        params.limits.max_matches = 0;
        let error = fs.search_text(params).await.expect_err("zero limit");
        assert_eq!(error.code, HostErrorCode::InvalidRequest);

        let nested_root = temp.path().join("root");
        std::fs::create_dir(&nested_root).expect("nested root");
        let nested_root = nested_root.canonicalize().expect("canonical nested root");
        let confined = LocalFileSystem::new(nested_root, temp.path().to_path_buf(), false);
        let error = confined
            .search_text(search_params(&root))
            .await
            .expect_err("escape should fail");
        assert_eq!(error.code, HostErrorCode::Forbidden);
    }

    fn glob_params(root: &HostPath, pattern: &str) -> GlobFilesParams {
        GlobFilesParams {
            root: root.clone(),
            pattern: pattern.to_owned(),
            max_depth: None,
            limits: host_protocol::data::fs::GlobFilesLimits {
                max_matches: 100,
                max_entries: 100,
                max_duration_ms: 10_000,
            },
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn glob_enumerates_with_generic_pattern_semantics_and_limits() {
        let (_temp, fs, root) = search_fixture();

        let result = fs
            .glob_files(glob_params(&root, "**/*.rs"))
            .await
            .expect("glob");
        let mut paths = result
            .matches
            .iter()
            .map(|path| path.as_str().to_owned())
            .collect::<Vec<_>>();
        paths.sort();
        assert_eq!(
            paths,
            vec![
                format!("{}/src/deep/nested.rs", root.as_str()),
                format!("{}/src/lib.rs", root.as_str()),
            ]
        );
        assert!(result.stopped.is_none());
        assert!(result.entries_visited >= 4);

        // A pattern without '/' also matches bare file names anywhere.
        let by_name = fs
            .glob_files(glob_params(&root, "*.md"))
            .await
            .expect("glob by name");
        assert_eq!(by_name.matches.len(), 1);
        assert!(by_name.matches[0].as_str().ends_with("readme.md"));

        let mut params = glob_params(&root, "**/*");
        params.limits.max_matches = 1;
        let matches = fs.glob_files(params).await.expect("match limit");
        assert_eq!(matches.matches.len(), 1);
        assert_eq!(matches.stopped, Some(GlobFilesStop::MatchLimit));

        let mut params = glob_params(&root, "**/*");
        params.limits.max_entries = 2;
        let entries = fs.glob_files(params).await.expect("entry limit");
        assert_eq!(entries.stopped, Some(GlobFilesStop::EntryLimit));
        assert_eq!(entries.entries_visited, 2);

        let mut params = glob_params(&root, "**/*");
        params.pattern = String::new();
        let error = fs.glob_files(params).await.expect_err("empty pattern");
        assert_eq!(error.code, HostErrorCode::InvalidRequest);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ranged_read_truncates_at_the_source_and_reports_true_size() {
        let (_temp, fs, root) = search_fixture();
        let path = HostPath::new(format!("{}/readme.md", root.as_str())).expect("path");
        let full = std::fs::read(std::path::Path::new(root.as_str()).join("readme.md"))
            .expect("full contents");

        let ranged = fs
            .read_file(ReadFileParams {
                path: path.clone(),
                offset: Some(0),
                max_bytes: Some(6),
            })
            .await
            .expect("ranged read");
        assert_eq!(ranged.data.as_slice(), &full[..6]);
        assert_eq!(ranged.file_size, Some(full.len() as u64));
        assert!(ranged.truncated);

        let tail = fs
            .read_file(ReadFileParams {
                path: path.clone(),
                offset: Some(7),
                max_bytes: None,
            })
            .await
            .expect("tail read");
        assert_eq!(tail.data.as_slice(), &full[7..]);
        assert!(!tail.truncated);

        let whole = fs
            .read_file(ReadFileParams {
                path,
                offset: None,
                max_bytes: Some(u64::MAX),
            })
            .await
            .expect("whole read");
        assert_eq!(whole.data.as_slice(), full.as_slice());
        assert!(!whole.truncated);
        assert_eq!(whole.file_size, Some(full.len() as u64));
    }
}
