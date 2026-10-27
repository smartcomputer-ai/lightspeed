//! envd's filesystem: the `fs/*` and `transfer/*` handlers.
//!
//! Every path goes through one [`Confinement`]: resolved beneath the root's
//! directory descriptor, following a symlink only while its target stays
//! inside the root, and never addressing the daemon state directory. Every
//! later operation names the resolved entry by its parent descriptor and
//! name, so nothing follows a link swapped in after resolution. Handlers do
//! their I/O on blocking threads; walks poll the request's cancellation
//! token between entries.

mod confinement;
mod os;
mod scan;
mod search;
mod transfer;
mod walk;

use std::{
    ffi::OsString,
    fmt::Display,
    io::{self, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

pub use confinement::Confinement;
use confinement::Lookup;
use environment_protocol::{
    data::{
        fs::{
            CreateDirectoryParams, CreateDirectoryResponse, DirectoryEntry, FileKind, FileMetadata,
            FileVersion, GetMetadataParams, GetMetadataResponse, GlobFilesParams,
            GlobFilesResponse, ReadDirectoryParams, ReadDirectoryResponse, ReadFileParams,
            ReadFileResponse, RemoveParams, RemoveResponse, RenameParams, RenameResponse,
            SearchTextParams, SearchTextResponse, WriteCondition, WriteFileParams,
            WriteFileResponse,
        },
        handshake::FilesystemLimits,
        scan::{ScanParams, ScanResponse},
        transfer::{
            AppendTransferInventoryParams, BeginTransferParams, ListTransferMissingParams,
            ListTransferMissingResponse, ReadTransferContentParams, ReadTransferContentResponse,
            ReadTransferInventoryParams, ReadTransferInventoryResponse, TransferOperationParams,
            TransferStatus, WriteTransferContentParams, WriteTransferContentResponse,
        },
    },
    error::{Capability, PermissionReason, PreconditionReason, ProtocolError},
    ids::DirectoryCursor,
    paths::{EntryName, EnvironmentPath},
};
use os::{Directory, EntryStat, RenameMode};
use rustix::io::Errno;
use sha2::{Digest, Sha256};

use crate::dispatch::RequestContext;

/// Prefix of `fs/writeFile` staging files next to their target.
const WRITE_STAGE_PREFIX: &str = ".lightspeed-write-";

/// What the filesystem handlers need from the daemon.
pub struct FilesystemConfig {
    /// The root and state directory; transfers journal in
    /// `<state_dir>/transfers`.
    pub confinement: Arc<Confinement>,
    /// Canonical working directory, inside the root.
    pub cwd: PathBuf,
    pub writable: bool,
    pub limits: FilesystemLimits,
}

/// The `fs/*` and `transfer/*` handlers. Cheap to clone.
#[derive(Clone)]
pub struct LocalFileSystem {
    inner: Arc<Inner>,
}

struct Inner {
    confinement: Arc<Confinement>,
    cwd: PathBuf,
    writable: bool,
    limits: FilesystemLimits,
    transfers: transfer::Transfers,
}

impl LocalFileSystem {
    pub fn new(config: FilesystemConfig) -> Result<Self, ProtocolError> {
        let transfers = transfer::Transfers::new(
            config.confinement.clone(),
            config.limits.transfer,
            config.writable,
        )?;
        Ok(Self {
            inner: Arc::new(Inner {
                confinement: config.confinement,
                cwd: config.cwd,
                writable: config.writable,
                limits: config.limits,
                transfers,
            }),
        })
    }

    pub fn confinement(&self) -> &Arc<Confinement> {
        &self.inner.confinement
    }

    pub fn cwd(&self) -> &Path {
        &self.inner.cwd
    }

    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Inner) -> Result<T, ProtocolError> + Send + 'static,
    ) -> Result<T, ProtocolError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || work(&inner))
            .await
            .map_err(|error| ProtocolError::internal(format!("filesystem worker: {error}")))?
    }

    pub async fn read_file(
        &self,
        _: &RequestContext,
        params: ReadFileParams,
    ) -> Result<ReadFileResponse, ProtocolError> {
        self.blocking(move |inner| inner.read_file(params)).await
    }

    pub async fn write_file(
        &self,
        _: &RequestContext,
        params: WriteFileParams,
    ) -> Result<WriteFileResponse, ProtocolError> {
        self.blocking(move |inner| inner.write_file(params)).await
    }

    pub async fn create_directory(
        &self,
        _: &RequestContext,
        params: CreateDirectoryParams,
    ) -> Result<CreateDirectoryResponse, ProtocolError> {
        self.blocking(move |inner| inner.create_directory(params))
            .await
    }

    pub async fn get_metadata(
        &self,
        _: &RequestContext,
        params: GetMetadataParams,
    ) -> Result<GetMetadataResponse, ProtocolError> {
        self.blocking(move |inner| inner.get_metadata(params)).await
    }

    pub async fn read_directory(
        &self,
        _: &RequestContext,
        params: ReadDirectoryParams,
    ) -> Result<ReadDirectoryResponse, ProtocolError> {
        self.blocking(move |inner| inner.read_directory(params))
            .await
    }

    pub async fn remove(
        &self,
        _: &RequestContext,
        params: RemoveParams,
    ) -> Result<RemoveResponse, ProtocolError> {
        self.blocking(move |inner| inner.remove(params)).await
    }

    pub async fn rename(
        &self,
        _: &RequestContext,
        params: RenameParams,
    ) -> Result<RenameResponse, ProtocolError> {
        self.blocking(move |inner| inner.rename(params)).await
    }

    /// Bounded recursive regex search with the ripgrep engine crates.
    pub async fn search_text(
        &self,
        context: &RequestContext,
        params: SearchTextParams,
    ) -> Result<SearchTextResponse, ProtocolError> {
        search::check_search(&params, &self.inner.limits.search)?;
        let cancel = context.cancel.clone();
        self.blocking(move |inner| search::search_text(&inner.confinement, params, &cancel))
            .await
    }

    /// Bounded recursive enumeration of entries matching `filter.include`.
    pub async fn glob_files(
        &self,
        context: &RequestContext,
        params: GlobFilesParams,
    ) -> Result<GlobFilesResponse, ProtocolError> {
        search::check_glob(&params, &self.inner.limits.glob)?;
        let cancel = context.cancel.clone();
        self.blocking(move |inner| search::glob_files(&inner.confinement, params, &cancel))
            .await
    }

    pub async fn scan(
        &self,
        context: &RequestContext,
        params: ScanParams,
    ) -> Result<ScanResponse, ProtocolError> {
        scan::check_scan(&params, &self.inner.limits.scan)?;
        let cancel = context.cancel.clone();
        self.blocking(move |inner| scan::scan(&inner.confinement, params, &cancel))
            .await
    }

    pub async fn transfer_begin(
        &self,
        _: &RequestContext,
        params: BeginTransferParams,
    ) -> Result<TransferStatus, ProtocolError> {
        self.blocking(move |inner| inner.transfers.begin(params))
            .await
    }

    pub async fn transfer_advance(
        &self,
        _: &RequestContext,
        params: TransferOperationParams,
    ) -> Result<TransferStatus, ProtocolError> {
        self.blocking(move |inner| inner.transfers.advance(params))
            .await
    }

    pub async fn transfer_get_status(
        &self,
        _: &RequestContext,
        params: TransferOperationParams,
    ) -> Result<TransferStatus, ProtocolError> {
        self.blocking(move |inner| inner.transfers.get_status(params))
            .await
    }

    pub async fn transfer_read_inventory(
        &self,
        _: &RequestContext,
        params: ReadTransferInventoryParams,
    ) -> Result<ReadTransferInventoryResponse, ProtocolError> {
        self.blocking(move |inner| inner.transfers.read_inventory(params))
            .await
    }

    pub async fn transfer_append(
        &self,
        _: &RequestContext,
        params: AppendTransferInventoryParams,
    ) -> Result<TransferStatus, ProtocolError> {
        self.blocking(move |inner| inner.transfers.append(params))
            .await
    }

    pub async fn transfer_list_missing(
        &self,
        _: &RequestContext,
        params: ListTransferMissingParams,
    ) -> Result<ListTransferMissingResponse, ProtocolError> {
        self.blocking(move |inner| inner.transfers.list_missing(params))
            .await
    }

    pub async fn transfer_read(
        &self,
        _: &RequestContext,
        params: ReadTransferContentParams,
    ) -> Result<ReadTransferContentResponse, ProtocolError> {
        self.blocking(move |inner| inner.transfers.read(params))
            .await
    }

    pub async fn transfer_write(
        &self,
        _: &RequestContext,
        params: WriteTransferContentParams,
    ) -> Result<WriteTransferContentResponse, ProtocolError> {
        self.blocking(move |inner| inner.transfers.write(params))
            .await
    }

    pub async fn transfer_commit(
        &self,
        _: &RequestContext,
        params: TransferOperationParams,
    ) -> Result<TransferStatus, ProtocolError> {
        self.blocking(move |inner| inner.transfers.commit(params))
            .await
    }

    pub async fn transfer_abort(
        &self,
        _: &RequestContext,
        params: TransferOperationParams,
    ) -> Result<TransferStatus, ProtocolError> {
        self.blocking(move |inner| inner.transfers.abort(params))
            .await
    }
}

impl Inner {
    fn ensure_writable(&self) -> Result<(), ProtocolError> {
        if self.writable {
            Ok(())
        } else {
            Err(ProtocolError::capability_unavailable(
                Capability::FilesystemWrite,
                "the filesystem is read-only",
            ))
        }
    }

    /// Open non-blocking, `fstat` the descriptor and refuse anything but a
    /// regular file before reading; read at most `maxBytes` (plus one byte
    /// to learn whether more follow).
    fn read_file(&self, params: ReadFileParams) -> Result<ReadFileResponse, ProtocolError> {
        let ceiling = self.limits.max_read_file_bytes;
        if params.max_bytes > ceiling {
            return Err(ProtocolError::above_ceiling(
                "maxBytes",
                params.max_bytes,
                ceiling,
            ));
        }
        let path = &params.path;
        let resolved = self.confinement.lookup(path, Lookup::FOLLOW)?;
        let stat = resolved.stat.expect("a followed lookup reports its target");
        require_regular(path, &stat)?;
        let mut file = resolved
            .parent
            .open_read(&resolved.name)
            .map_err(|error| io_error(error, path))?;
        let opened = EntryStat::of_file(&file).map_err(|error| io_error(error, path))?;
        require_regular(path, &opened)?;
        if params.offset > 0 {
            file.seek(SeekFrom::Start(params.offset))
                .map_err(|error| io_error(error, path))?;
        }
        let mut data = os::read_up_to(&mut file, params.max_bytes.saturating_add(1))
            .map_err(|error| io_error(error, path))?;
        let read = data.len() as u64;
        let more = read > params.max_bytes;
        data.truncate(params.max_bytes as usize);
        // Files such as procfs entries report size 0; their size is what
        // could be read. Every other file's size is what `fstat` reported,
        // also for an `offset` past its end.
        let file_size = if opened.size == 0 && read > 0 {
            params.offset.saturating_add(read)
        } else {
            opened.size
        };
        let truncated = more || params.offset.saturating_add(data.len() as u64) < file_size;
        Ok(ReadFileResponse {
            data: data.into(),
            file_size,
            truncated,
            version: opened.version(),
        })
    }

    /// Stage next to the final target, fsync, set the mode and rename over
    /// it.
    fn write_file(&self, params: WriteFileParams) -> Result<WriteFileResponse, ProtocolError> {
        self.ensure_writable()?;
        let ceiling = self.limits.max_write_file_bytes;
        let size = params.data.len() as u64;
        if size > ceiling {
            return Err(ProtocolError::above_ceiling("data", size, ceiling));
        }
        let path = &params.path;
        let resolved = self
            .confinement
            .lookup(path, Lookup::FOLLOW.allowing_missing())?;
        write_preconditions(
            path,
            resolved.stat.as_ref(),
            params.create,
            params.if_match.as_ref(),
        )?;
        let base = resolved
            .stat
            .map_or(os::DEFAULT_FILE_MODE, |stat| stat.mode);
        let mode = if params.executable {
            os::with_execute_bits(base)
        } else {
            base
        };
        let rename = if params.create == WriteCondition::IfAbsent {
            RenameMode::NoReplace
        } else {
            RenameMode::Replace
        };
        let stage = OsString::from(format!(
            "{WRITE_STAGE_PREFIX}{:032x}",
            rand::random::<u128>()
        ));
        let parent = &resolved.parent;
        let io = |error| io_error(error, path);
        let mut file = parent
            .create_file(&stage, os::PRIVATE_FILE_MODE)
            .map_err(io)?;
        let staged = file
            .write_all(params.data.as_slice())
            .and_then(|()| os::set_mode(&file, mode))
            .and_then(|()| file.sync_all())
            .and_then(|()| parent.rename(&stage, parent, &resolved.name, rename));
        if let Err(error) = staged {
            let _ = parent.unlink(&stage);
            if rename == RenameMode::NoReplace && error.kind() == io::ErrorKind::AlreadyExists {
                return Err(create_condition(path, true));
            }
            return Err(io(error));
        }
        parent.sync().map_err(io)?;
        let version = EntryStat::of_file(&file).map_err(io)?.version();
        Ok(WriteFileResponse { version })
    }

    fn create_directory(
        &self,
        params: CreateDirectoryParams,
    ) -> Result<CreateDirectoryResponse, ProtocolError> {
        self.ensure_writable()?;
        let path = &params.path;
        let lookup = if params.recursive {
            Lookup::FOLLOW.allowing_missing().creating_parents()
        } else {
            Lookup::FOLLOW.allowing_missing()
        };
        let resolved = self.confinement.lookup(path, lookup)?;
        match resolved.stat {
            Some(stat) if params.recursive && stat.is_dir() => {}
            Some(_) => {
                return Err(ProtocolError::already_exists(format!("{path} exists")));
            }
            None => match resolved
                .parent
                .mkdir(&resolved.name, os::DEFAULT_DIRECTORY_MODE)
            {
                Ok(()) => resolved
                    .parent
                    .sync()
                    .map_err(|error| io_error(error, path))?,
                Err(error)
                    if params.recursive
                        && error.kind() == io::ErrorKind::AlreadyExists
                        && resolved
                            .parent
                            .stat(&resolved.name)
                            .is_ok_and(|stat| stat.is_dir()) => {}
                Err(error) => return Err(io_error(error, path)),
            },
        }
        Ok(CreateDirectoryResponse {})
    }

    fn get_metadata(
        &self,
        params: GetMetadataParams,
    ) -> Result<GetMetadataResponse, ProtocolError> {
        let path = &params.path;
        let lookup = if params.follow_symlinks {
            Lookup::FOLLOW
        } else {
            Lookup::NO_FOLLOW
        };
        let resolved = self.confinement.lookup(path, lookup)?;
        let stat = resolved
            .parent
            .stat_with_birth(&resolved.name)
            .map_err(|error| io_error(error, path))?;
        Ok(GetMetadataResponse {
            metadata: metadata(&resolved.parent, &resolved.name, &stat),
        })
    }

    fn read_directory(
        &self,
        params: ReadDirectoryParams,
    ) -> Result<ReadDirectoryResponse, ProtocolError> {
        let ceiling = self.limits.max_directory_entries;
        if params.limit == 0 {
            return Err(ProtocolError::invalid_field(
                "limit",
                "limit must be at least 1",
            ));
        }
        if params.limit > ceiling {
            return Err(ProtocolError::above_ceiling(
                "limit",
                params.limit.into(),
                ceiling.into(),
            ));
        }
        let position = params.cursor.as_ref().map(cursor::decode).transpose()?;
        let path = &params.path;
        let resolved = self.confinement.lookup(path, Lookup::FOLLOW)?;
        if !resolved.stat.is_some_and(|stat| stat.is_dir()) {
            return Err(ProtocolError::failed_precondition(
                PreconditionReason::NotDirectory,
                format!("{path} is not a directory"),
            ));
        }
        let directory = resolved
            .open_directory()
            .map_err(|error| io_error(error, path))?;
        let mut names = directory.names().map_err(|error| io_error(error, path))?;
        if resolved
            .stat
            .is_some_and(|stat| self.confinement.holds_state(&stat))
        {
            names.retain(|name| {
                directory
                    .stat(name)
                    .map_or(true, |stat| !self.confinement.is_state(&stat))
            });
        }
        names.sort_by(|left, right| left.as_encoded_bytes().cmp(right.as_encoded_bytes()));
        let start = position.map_or(0, |position| position.start(&names));
        let end = start.saturating_add(params.limit as usize).min(names.len());
        let page = &names[start.min(end)..end];
        let mut response = ReadDirectoryResponse {
            entries: Vec::with_capacity(page.len()),
            next_cursor: (end < names.len())
                .then(|| {
                    page.last()
                        .map(|name| cursor::encode(name.as_encoded_bytes()))
                })
                .flatten(),
            skipped_entry_count: 0,
        };
        for name in page {
            let Some(entry_name) = os::utf8_name(name).and_then(|name| EntryName::new(name).ok())
            else {
                response.skipped_entry_count += 1;
                continue;
            };
            let stat = match directory.stat_with_birth(name) {
                Ok(stat) => stat,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(io_error(error, format!("{path}/{entry_name}"))),
            };
            response.entries.push(DirectoryEntry {
                name: entry_name,
                metadata: metadata(&directory, name, &stat),
            });
        }
        Ok(response)
    }

    fn remove(&self, params: RemoveParams) -> Result<RemoveResponse, ProtocolError> {
        self.ensure_writable()?;
        let path = &params.path;
        let resolved = match self
            .confinement
            .lookup(path, Lookup::NO_FOLLOW.allowing_missing())
        {
            Ok(resolved) => resolved,
            Err(error)
                if params.force
                    && matches!(
                        error.data,
                        environment_protocol::error::ErrorData::NotFound { .. }
                    ) =>
            {
                return Ok(RemoveResponse {});
            }
            Err(error) => return Err(error),
        };
        if resolved.is_root() {
            return Err(filesystem_root());
        }
        self.confinement.refuse_containing_state(&resolved, path)?;
        let io = |error| io_error(error, path);
        match resolved.stat {
            None if params.force => return Ok(RemoveResponse {}),
            None => return Err(ProtocolError::not_found(format!("{path} does not exist"))),
            Some(stat) if stat.is_dir() => {
                if params.recursive {
                    resolved.parent.remove_tree(&resolved.name).map_err(io)?;
                } else {
                    resolved.parent.rmdir(&resolved.name).map_err(io)?;
                }
            }
            Some(_) => resolved.parent.unlink(&resolved.name).map_err(io)?,
        }
        resolved.parent.sync().map_err(io)?;
        Ok(RemoveResponse {})
    }

    fn rename(&self, params: RenameParams) -> Result<RenameResponse, ProtocolError> {
        self.ensure_writable()?;
        let source = self.confinement.lookup(&params.source, Lookup::NO_FOLLOW)?;
        let destination = self
            .confinement
            .lookup(&params.destination, Lookup::NO_FOLLOW.allowing_missing())?;
        if source.is_root() || destination.is_root() {
            return Err(filesystem_root());
        }
        self.confinement
            .refuse_containing_state(&source, &params.source)?;
        self.confinement
            .refuse_containing_state(&destination, &params.destination)?;
        let source_stat = source.stat.expect("an existing source has a stat");
        if let Some(expected) = &params.if_match
            && source_stat.version() != *expected
        {
            return Err(ProtocolError::failed_precondition(
                PreconditionReason::IfMatchFailed,
                format!("{} changed since that version", params.source),
            ));
        }
        if destination.segments.starts_with(&source.segments)
            && destination.segments != source.segments
        {
            return Err(ProtocolError::invalid_field(
                "destination",
                "a directory cannot move into itself",
            ));
        }
        if let Some(stat) = &destination.stat {
            if stat.is_dir() {
                return Err(ProtocolError::failed_precondition(
                    PreconditionReason::IsDirectory,
                    format!(
                        "{} is a directory and is never replaced",
                        params.destination
                    ),
                ));
            }
            if !params.overwrite {
                return Err(ProtocolError::already_exists(format!(
                    "{} exists",
                    params.destination
                )));
            }
        }
        let mode = if params.overwrite {
            RenameMode::Replace
        } else {
            RenameMode::NoReplace
        };
        source
            .parent
            .rename(&source.name, &destination.parent, &destination.name, mode)
            .map_err(|error| io_error(error, &params.destination))?;
        source
            .parent
            .sync()
            .and_then(|()| destination.parent.sync())
            .map_err(|error| io_error(error, &params.destination))?;
        Ok(RenameResponse {})
    }
}

fn require_regular(path: &EnvironmentPath, stat: &EntryStat) -> Result<(), ProtocolError> {
    match stat.kind {
        FileKind::File => Ok(()),
        FileKind::Directory => Err(ProtocolError::failed_precondition(
            PreconditionReason::IsDirectory,
            format!("{path} is a directory"),
        )),
        FileKind::Symlink | FileKind::Other => Err(ProtocolError::failed_precondition(
            PreconditionReason::NotRegularFile,
            format!("{path} is not a regular file"),
        )),
    }
}

/// The checks of `fs/writeFile` and the transfer `writeFile` selection on
/// the final target as it is now.
pub(crate) fn write_preconditions(
    path: &EnvironmentPath,
    current: Option<&EntryStat>,
    create: WriteCondition,
    if_match: Option<&FileVersion>,
) -> Result<(), ProtocolError> {
    if let Some(stat) = current {
        require_regular(path, stat)?;
    }
    match (current, create) {
        (Some(_), WriteCondition::IfAbsent) => return Err(create_condition(path, true)),
        (None, WriteCondition::IfPresent) => return Err(create_condition(path, false)),
        _ => {}
    }
    if let Some(expected) = if_match
        && current.is_none_or(|stat| stat.version() != *expected)
    {
        return Err(ProtocolError::failed_precondition(
            PreconditionReason::IfMatchFailed,
            format!("{path} is not at the expected version"),
        ));
    }
    Ok(())
}

fn create_condition(path: &EnvironmentPath, exists: bool) -> ProtocolError {
    ProtocolError::failed_precondition(
        PreconditionReason::CreateCondition,
        if exists {
            format!("{path} exists")
        } else {
            format!("{path} does not exist")
        },
    )
}

fn filesystem_root() -> ProtocolError {
    ProtocolError::failed_precondition(
        PreconditionReason::FilesystemRoot,
        "the filesystem root cannot be removed, renamed or replaced",
    )
}

fn metadata(parent: &Directory, name: &std::ffi::OsStr, stat: &EntryStat) -> FileMetadata {
    let symlink_target = (stat.kind == FileKind::Symlink)
        .then(|| parent.read_link(name).ok())
        .flatten()
        .and_then(|target| target.into_string().ok());
    FileMetadata {
        kind: stat.kind,
        size_bytes: stat.size,
        executable: stat.kind == FileKind::File && stat.executable(),
        modified_at_ms: stat.modified_ms(),
        created_at_ms: stat.birth_ms,
        symlink_target,
        version: stat.version(),
    }
}

/// Bytes `value` takes as a JSON string, quotes included: `"` and `\`
/// and the short control escapes take two bytes, every other control
/// character six (`\u00XX`). What response budgets charge, so an answer
/// within its budget also encodes within it.
pub(crate) fn json_string_len(value: &str) -> u64 {
    let escapes: u64 = value
        .bytes()
        .map(|byte| match byte {
            b'"' | b'\\' | b'\n' | b'\r' | b'\t' | 0x08 | 0x0c => 1,
            0x00..=0x1f => 5,
            _ => 0,
        })
        .sum();
    value.len() as u64 + escapes + 2
}

/// envd's one mapping of OS errors to protocol error kinds.
pub(crate) fn io_error(error: io::Error, subject: impl Display) -> ProtocolError {
    let message = format!("{subject}: {error}");
    let precondition = |reason| ProtocolError::failed_precondition(reason, message.clone());
    match Errno::from_io_error(&error) {
        Some(Errno::LOOP) => return precondition(PreconditionReason::SymlinkLoop),
        Some(Errno::NOTDIR) => return precondition(PreconditionReason::NotDirectory),
        Some(Errno::ISDIR) => return precondition(PreconditionReason::IsDirectory),
        Some(Errno::NOTEMPTY) => return precondition(PreconditionReason::NotEmpty),
        Some(Errno::NOSPC | Errno::DQUOT) => return precondition(PreconditionReason::NoSpace),
        Some(Errno::XDEV) => return precondition(PreconditionReason::CrossDevice),
        Some(Errno::ROFS) => {
            return ProtocolError::capability_unavailable(Capability::FilesystemWrite, message);
        }
        _ => {}
    }
    match error.kind() {
        io::ErrorKind::NotFound => ProtocolError::not_found(message),
        io::ErrorKind::AlreadyExists => ProtocolError::already_exists(message),
        io::ErrorKind::PermissionDenied => {
            ProtocolError::permission_denied(Some(PermissionReason::FilePermission), message)
        }
        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
            ProtocolError::unavailable(message)
        }
        _ => ProtocolError::internal(message),
    }
}

/// `fs/readDirectory` cursors: the last name returned, as hex. Names too
/// long for the 255-byte cursor carry a prefix and the name's SHA-256.
mod cursor {
    use super::*;

    /// Longest name encoded in full (`x` and two hex digits per byte).
    const MAX_EXACT_NAME_BYTES: usize = 127;
    const PREFIX_BYTES: usize = 64;

    pub(super) enum Position {
        After(Vec<u8>),
        AfterHashed { prefix: Vec<u8>, sha256: Vec<u8> },
    }

    pub(super) fn encode(name: &[u8]) -> DirectoryCursor {
        let value = if name.len() <= MAX_EXACT_NAME_BYTES {
            format!("x{}", hex::encode(name))
        } else {
            format!(
                "h{}.{}",
                hex::encode(&name[..PREFIX_BYTES]),
                hex::encode(Sha256::digest(name))
            )
        };
        DirectoryCursor::new(value).expect("hex cursors are short and printable")
    }

    pub(super) fn decode(cursor: &DirectoryCursor) -> Result<Position, ProtocolError> {
        let malformed =
            || ProtocolError::invalid_field("cursor", "not a cursor this daemon minted");
        let value = cursor.as_str();
        if let Some(exact) = value.strip_prefix('x') {
            return hex::decode(exact)
                .map(Position::After)
                .map_err(|_| malformed());
        }
        let (prefix, sha256) = value
            .strip_prefix('h')
            .and_then(|hashed| hashed.split_once('.'))
            .ok_or_else(malformed)?;
        let prefix = hex::decode(prefix).map_err(|_| malformed())?;
        let sha256 = hex::decode(sha256).map_err(|_| malformed())?;
        if prefix.len() != PREFIX_BYTES || sha256.len() != 32 {
            return Err(malformed());
        }
        Ok(Position::AfterHashed { prefix, sha256 })
    }

    impl Position {
        /// Index of the first name after this position in sorted `names`.
        pub(super) fn start(&self, names: &[OsString]) -> usize {
            match self {
                Self::After(last) => {
                    names.partition_point(|name| name.as_encoded_bytes() <= last.as_slice())
                }
                Self::AfterHashed { prefix, sha256 } => names
                    .iter()
                    .position(|name| {
                        let bytes = name.as_encoded_bytes();
                        bytes.starts_with(prefix) && Sha256::digest(bytes).as_slice() == sha256
                    })
                    .map(|index| index + 1)
                    // The last name vanished: continue after its prefix.
                    .unwrap_or_else(|| {
                        names.partition_point(|name| name.as_encoded_bytes() <= prefix.as_slice())
                    }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use environment_protocol::{
        data::handshake::{GlobCeilings, ScanCeilings, SearchCeilings, TransferCeilings},
        error::{ErrorData, ProtocolErrorKind},
    };

    use super::*;

    pub(crate) fn limits() -> FilesystemLimits {
        FilesystemLimits {
            max_read_file_bytes: 8 * 1024 * 1024,
            max_write_file_bytes: 8 * 1024 * 1024,
            max_directory_entries: 1000,
            search: SearchCeilings {
                max_matches: 10_000,
                max_files: 100_000,
                max_bytes: 1 << 30,
                max_duration_ms: 60_000,
                max_line_bytes: 4096,
            },
            glob: GlobCeilings {
                max_matches: 100_000,
                max_files: 1_000_000,
                max_duration_ms: 60_000,
            },
            scan: ScanCeilings {
                max_roots: 32,
                max_patterns: 32,
                max_entries: 10_000,
                max_file_bytes: 256 * 1024,
                max_total_bytes: 4 * 1024 * 1024,
                max_duration_ms: 30_000,
            },
            transfer: TransferCeilings {
                max_active_operations: 8,
                max_content_chunk_bytes: 4 * 1024 * 1024,
                max_inventory_page_entries: 1000,
                receipt_retention_ms: 86_400_000,
                max_entries: 100_000,
                max_depth: 64,
                max_file_bytes: 1 << 40,
                max_total_bytes: 1 << 40,
                max_manifest_bytes: 32 * 1024 * 1024,
                max_duration_ms: 86_400_000,
            },
        }
    }

    pub(crate) fn context() -> RequestContext {
        RequestContext::detached()
    }

    /// A filesystem rooted at `root` with its state directory inside.
    pub(crate) fn filesystem(root: &Path, writable: bool) -> LocalFileSystem {
        let confinement =
            Arc::new(Confinement::new(root.to_path_buf(), root.join(".state")).unwrap());
        LocalFileSystem::new(FilesystemConfig {
            confinement,
            cwd: root.to_path_buf(),
            writable,
            limits: limits(),
        })
        .unwrap()
    }

    fn path(root: &Path, relative: &str) -> EnvironmentPath {
        EnvironmentPath::new(format!("{}/{relative}", root.display())).unwrap()
    }

    fn reason(error: &ProtocolError) -> Option<PreconditionReason> {
        match error.data {
            ErrorData::FailedPrecondition { reason } => Some(reason),
            _ => None,
        }
    }

    fn read(path: EnvironmentPath, max_bytes: u64) -> ReadFileParams {
        ReadFileParams {
            path,
            offset: 0,
            max_bytes,
        }
    }

    fn write(path: EnvironmentPath, data: &[u8]) -> WriteFileParams {
        WriteFileParams {
            path,
            data: data.into(),
            create: WriteCondition::Always,
            if_match: None,
            executable: false,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn special_files_are_refused_before_any_read() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        // The root is `/` so a workspace link to a device stays inside it.
        let confinement = Arc::new(Confinement::new("/".into(), base.join("state")).unwrap());
        let fs = LocalFileSystem::new(FilesystemConfig {
            confinement,
            cwd: base.clone(),
            writable: false,
            limits: limits(),
        })
        .unwrap();
        symlink("/dev/zero", base.join("zero")).unwrap();
        let status = std::process::Command::new("mkfifo")
            .arg(base.join("fifo"))
            .status()
            .unwrap();
        assert!(status.success());
        for subject in [
            EnvironmentPath::new("/dev/zero").unwrap(),
            path(&base, "zero"),
            path(&base, "fifo"),
        ] {
            let error = fs
                .read_file(&context(), read(subject.clone(), 1024))
                .await
                .unwrap_err();
            assert_eq!(
                reason(&error),
                Some(PreconditionReason::NotRegularFile),
                "{subject}"
            );
        }
        let error = fs
            .read_file(
                &context(),
                read(EnvironmentPath::new(base.to_str().unwrap()).unwrap(), 1),
            )
            .await
            .unwrap_err();
        assert_eq!(reason(&error), Some(PreconditionReason::IsDirectory));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "current_thread")]
    async fn zero_size_proc_files_report_the_bytes_read() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let confinement = Arc::new(Confinement::new("/".into(), base.join("state")).unwrap());
        let fs = LocalFileSystem::new(FilesystemConfig {
            confinement,
            cwd: base,
            writable: false,
            limits: limits(),
        })
        .unwrap();
        let response = fs
            .read_file(
                &context(),
                read(EnvironmentPath::new("/proc/self/status").unwrap(), 1 << 20),
            )
            .await
            .unwrap();
        assert!(!response.data.is_empty());
        assert_eq!(response.file_size, response.data.len() as u64);
        assert!(!response.truncated);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ranged_reads_bound_every_read_and_report_truncation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let fs = filesystem(&root, false);
        std::fs::write(root.join("file"), b"0123456789").unwrap();
        let ranged = fs
            .read_file(
                &context(),
                ReadFileParams {
                    path: path(&root, "file"),
                    offset: 2,
                    max_bytes: 3,
                },
            )
            .await
            .unwrap();
        assert_eq!(ranged.data.as_slice(), b"234");
        assert_eq!(ranged.file_size, 10);
        assert!(ranged.truncated);
        let past_end = fs
            .read_file(
                &context(),
                ReadFileParams {
                    path: path(&root, "file"),
                    offset: 100,
                    max_bytes: 3,
                },
            )
            .await
            .unwrap();
        assert!(past_end.data.as_slice().is_empty());
        assert_eq!(
            past_end.file_size, 10,
            "the size of the file, not the offset"
        );
        assert!(!past_end.truncated);
        let whole = fs
            .read_file(&context(), read(path(&root, "file"), 10))
            .await
            .unwrap();
        assert!(!whole.truncated);
        let error = fs
            .read_file(&context(), read(path(&root, "file"), u64::MAX))
            .await
            .unwrap_err();
        assert!(matches!(
            error.data,
            ErrorData::InvalidParams {
                ceiling: Some(_),
                ..
            }
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn writes_are_atomic_conditional_and_keep_links_and_modes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let fs = filesystem(&root, true);
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("src/tool.sh"), b"old").unwrap();
        std::fs::set_permissions(
            root.join("src/tool.sh"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        symlink("src/tool.sh", root.join("link")).unwrap();

        let current = fs
            .read_file(&context(), read(path(&root, "link"), 16))
            .await
            .unwrap()
            .version;
        let written = fs
            .write_file(
                &context(),
                WriteFileParams {
                    if_match: Some(current.clone()),
                    ..write(path(&root, "link"), b"new")
                },
            )
            .await
            .unwrap();
        assert!(
            std::fs::symlink_metadata(root.join("link"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(root.join("src/tool.sh")).unwrap(), b"new");
        let mode = std::fs::metadata(root.join("src/tool.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        assert_ne!(written.version, current);

        let stale = fs
            .write_file(
                &context(),
                WriteFileParams {
                    if_match: Some(current),
                    ..write(path(&root, "src/tool.sh"), b"lost")
                },
            )
            .await
            .unwrap_err();
        assert_eq!(reason(&stale), Some(PreconditionReason::IfMatchFailed));
        let exists = fs
            .write_file(
                &context(),
                WriteFileParams {
                    create: WriteCondition::IfAbsent,
                    ..write(path(&root, "src/tool.sh"), b"x")
                },
            )
            .await
            .unwrap_err();
        assert_eq!(reason(&exists), Some(PreconditionReason::CreateCondition));
        let missing = fs
            .write_file(
                &context(),
                WriteFileParams {
                    create: WriteCondition::IfPresent,
                    ..write(path(&root, "src/new"), b"x")
                },
            )
            .await
            .unwrap_err();
        assert_eq!(reason(&missing), Some(PreconditionReason::CreateCondition));
        fs.write_file(
            &context(),
            WriteFileParams {
                executable: true,
                ..write(path(&root, "src/run"), b"#!/bin/sh\n")
            },
        )
        .await
        .unwrap();
        let mode = std::fs::metadata(root.join("src/run"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
        let error = fs
            .write_file(&context(), write(path(&root, "src"), b"x"))
            .await
            .unwrap_err();
        assert_eq!(reason(&error), Some(PreconditionReason::IsDirectory));
        let error = fs
            .write_file(&context(), write(path(&root, "absent/file"), b"x"))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ProtocolErrorKind::NotFound);
        // No staging file is left behind.
        let leftovers: Vec<_> = std::fs::read_dir(root.join("src"))
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(WRITE_STAGE_PREFIX)
            })
            .collect();
        assert!(leftovers.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rename_never_replaces_directories_or_unasked_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let fs = filesystem(&root, true);
        std::fs::create_dir(root.join("dir")).unwrap();
        std::fs::write(root.join("a"), b"a").unwrap();
        std::fs::write(root.join("b"), b"b").unwrap();
        let rename = |source: &str, destination: &str, overwrite: bool| RenameParams {
            source: path(&root, source),
            destination: path(&root, destination),
            overwrite,
            if_match: None,
        };
        let error = fs
            .rename(&context(), rename("a", "b", false))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ProtocolErrorKind::AlreadyExists);
        let error = fs
            .rename(&context(), rename("a", "dir", true))
            .await
            .unwrap_err();
        assert_eq!(reason(&error), Some(PreconditionReason::IsDirectory));
        fs.rename(&context(), rename("a", "b", true)).await.unwrap();
        assert_eq!(std::fs::read(root.join("b")).unwrap(), b"a");
        fs.rename(&context(), rename("b", "dir/c", false))
            .await
            .unwrap();
        assert!(root.join("dir/c").exists());
        let error = fs
            .rename(&context(), rename(".state", "moved", false))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ProtocolErrorKind::PermissionDenied);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn removal_refuses_the_root_and_the_state_directory() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let fs = filesystem(&root, true);
        let remove = |path: EnvironmentPath| RemoveParams {
            path,
            recursive: true,
            force: false,
        };
        let error = fs
            .remove(
                &context(),
                remove(EnvironmentPath::new(root.to_str().unwrap()).unwrap()),
            )
            .await
            .unwrap_err();
        assert_eq!(reason(&error), Some(PreconditionReason::FilesystemRoot));
        let error = fs
            .remove(&context(), remove(path(&root, ".state")))
            .await
            .unwrap_err();
        assert!(matches!(
            error.data,
            ErrorData::PermissionDenied {
                reason: Some(PermissionReason::DaemonState)
            }
        ));
        fs.remove(
            &context(),
            RemoveParams {
                force: true,
                ..remove(path(&root, "missing/deeper"))
            },
        )
        .await
        .unwrap();
        let read_only = filesystem(&root, false);
        let error = read_only
            .remove(&context(), remove(path(&root, "anything")))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ProtocolErrorKind::CapabilityUnavailable);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn directories_page_by_cursor_and_hide_the_state_directory() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let fs = filesystem(&root, false);
        for name in ["c", "a", "b\\slash", "d"] {
            std::fs::write(root.join(name), name).unwrap();
        }
        symlink("a", root.join("link")).unwrap();
        let long = "n".repeat(200);
        std::fs::write(root.join(&long), "long").unwrap();
        let mut names = Vec::new();
        let mut cursor = None;
        loop {
            let page = fs
                .read_directory(
                    &context(),
                    ReadDirectoryParams {
                        path: EnvironmentPath::new(root.to_str().unwrap()).unwrap(),
                        limit: 2,
                        cursor: cursor.take(),
                    },
                )
                .await
                .unwrap();
            names.extend(
                page.entries
                    .iter()
                    .map(|entry| (entry.name.as_str().to_owned(), entry.metadata.kind)),
            );
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        let expected: Vec<(String, FileKind)> = [
            ("a", FileKind::File),
            ("b\\slash", FileKind::File),
            ("c", FileKind::File),
            ("d", FileKind::File),
            ("link", FileKind::Symlink),
            (long.as_str(), FileKind::File),
        ]
        .into_iter()
        .map(|(name, kind)| (name.to_owned(), kind))
        .collect();
        assert_eq!(names, expected);
        let error = fs
            .read_directory(
                &context(),
                ReadDirectoryParams {
                    path: EnvironmentPath::new(root.to_str().unwrap()).unwrap(),
                    limit: 1,
                    cursor: Some(DirectoryCursor::new("garbage").unwrap()),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ProtocolErrorKind::InvalidParams);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn metadata_describes_links_and_follows_on_request() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let fs = filesystem(&root, false);
        std::fs::create_dir(root.join("dir")).unwrap();
        symlink("dir", root.join("link")).unwrap();
        let link = fs
            .get_metadata(
                &context(),
                GetMetadataParams {
                    path: path(&root, "link"),
                    follow_symlinks: false,
                },
            )
            .await
            .unwrap()
            .metadata;
        assert_eq!(link.kind, FileKind::Symlink);
        assert_eq!(link.symlink_target.as_deref(), Some("dir"));
        let followed = fs
            .get_metadata(
                &context(),
                GetMetadataParams {
                    path: path(&root, "link"),
                    follow_symlinks: true,
                },
            )
            .await
            .unwrap()
            .metadata;
        assert_eq!(followed.kind, FileKind::Directory);
    }

    #[test]
    fn io_errors_map_to_protocol_kinds() {
        let error = |errno: Errno| io_error(io::Error::from(errno), "subject");
        assert_eq!(
            reason(&error(Errno::LOOP)),
            Some(PreconditionReason::SymlinkLoop)
        );
        assert_eq!(
            reason(&error(Errno::NOTEMPTY)),
            Some(PreconditionReason::NotEmpty)
        );
        assert_eq!(
            reason(&error(Errno::XDEV)),
            Some(PreconditionReason::CrossDevice)
        );
        assert_eq!(error(Errno::NOENT).kind(), ProtocolErrorKind::NotFound);
        assert_eq!(error(Errno::EXIST).kind(), ProtocolErrorKind::AlreadyExists);
        assert!(matches!(
            error(Errno::ACCESS).data,
            ErrorData::PermissionDenied {
                reason: Some(PermissionReason::FilePermission)
            }
        ));
        assert_eq!(
            error(Errno::ROFS).kind(),
            ProtocolErrorKind::CapabilityUnavailable
        );
    }

    #[test]
    fn json_string_lengths_count_escapes() {
        for value in [
            "",
            "plain",
            "a\"b\\c",
            "line\n\ttab",
            "\u{1b}[31mred\u{1b}[0m",
            "\0",
            "ünï",
        ] {
            let encoded = serde_json::to_string(value).unwrap();
            assert_eq!(json_string_len(value), encoded.len() as u64, "{value:?}");
        }
    }
}
