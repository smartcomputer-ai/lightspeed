//! Materialize and writeFile: append the incoming inventory, scan the
//! destination (hashing only files whose size matches an incoming file),
//! collect missing content, stage the tree next to the destination and
//! publish it with one rename at commit.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ffi::{OsStr, OsString},
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use environment_protocol::{
    data::{
        fs::{FileKind, FileVersion, TreeContent, WriteCondition},
        transfer::{InventoryEntry, TransferLimits, TransferOnExisting},
    },
    error::{PreconditionReason, ProtocolError},
    paths::{ContentDigest, EnvironmentPath, TreePath},
};
use sha2::{Digest, Sha256};

use crate::filesystem::{
    confinement::{Confinement, Lookup, Resolved},
    io_error,
    os::{self, DEFAULT_DIRECTORY_MODE, DEFAULT_FILE_MODE, Directory, EntryStat, RenameMode},
    walk::{CompiledFilter, Event, LinkMode, Walker},
    write_preconditions,
};

use super::{
    STEP_BYTES, STEP_ENTRIES,
    capture::{Source, source_changed},
    check_entry_quota, entry_bytes,
    journal::StageRecord,
};

/// Prefix of staging directories next to a destination.
pub(super) const STAGE_PREFIX: &str = ".lightspeed-transfer-";
/// The staged tree inside a staging directory.
const STAGED_TREE: &str = "tree";

/// What a publish writes.
#[derive(Clone, Debug)]
pub(super) enum Kind {
    Materialize {
        on_existing: TransferOnExisting,
    },
    WriteFile {
        create: WriteCondition,
        if_match: Option<FileVersion>,
        executable: bool,
    },
}

struct Stage {
    parent: Arc<Directory>,
    name: OsString,
    directory: Directory,
}

/// Content uploaded for one digest, spooled under the journal directory.
pub(super) struct Upload {
    file: File,
    path: PathBuf,
    hash: Sha256,
    length: u64,
    size: u64,
    complete: bool,
}

/// Why one uploaded chunk was refused.
pub(super) enum ChunkFailure {
    /// The offset is not the digest's next offset and the chunk is not an
    /// identical repeat; nothing changed.
    Offset(ProtocolError),
    /// The completed content does not hash to its digest.
    Mismatch(ProtocolError),
    Io(ProtocolError),
}

impl Upload {
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// Bytes written so far: the next expected offset.
    pub fn length(&self) -> u64 {
        self.length
    }

    /// Append a chunk at `offset`; returns the next expected offset.
    pub fn write(
        &mut self,
        digest: &ContentDigest,
        offset: u64,
        data: &[u8],
    ) -> Result<u64, ChunkFailure> {
        let io = |error| ChunkFailure::Io(io_error(error, digest));
        let end = offset.saturating_add(data.len() as u64);
        if offset < self.length {
            // An identical repeat of bytes already written is accepted.
            if end > self.length {
                return Err(ChunkFailure::Offset(offset_error(digest, self.length)));
            }
            let mut existing = vec![0; data.len()];
            self.file.seek(SeekFrom::Start(offset)).map_err(io)?;
            self.file.read_exact(&mut existing).map_err(io)?;
            if existing != data {
                return Err(ChunkFailure::Offset(offset_error(digest, self.length)));
            }
            return Ok(self.length);
        }
        if offset != self.length || self.complete || end > self.size {
            return Err(ChunkFailure::Offset(offset_error(digest, self.length)));
        }
        self.file.seek(SeekFrom::Start(offset)).map_err(io)?;
        self.file.write_all(data).map_err(io)?;
        self.hash.update(data);
        self.length = end;
        if self.length == self.size {
            let actual = ContentDigest::from_sha256(self.hash.clone().finalize().into());
            if actual != *digest {
                return Err(ChunkFailure::Mismatch(ProtocolError::failed_precondition(
                    PreconditionReason::DigestMismatch,
                    format!("uploaded content hashes to {actual}, not {digest}"),
                )));
            }
            self.file.sync_all().map_err(io)?;
            self.complete = true;
        }
        Ok(self.length)
    }
}

fn offset_error(digest: &ContentDigest, next: u64) -> ProtocolError {
    ProtocolError::failed_precondition(
        PreconditionReason::TransferOffset,
        format!("the next offset of {digest} is {next}"),
    )
}

struct Copying {
    source: File,
    destination: File,
    hash: Sha256,
    expected: ContentDigest,
    copied: u64,
    size: u64,
    mode: u32,
    changed: Option<Source>,
}

struct DestinationScan {
    walker: Option<Walker>,
    single: Option<Source>,
    hashing: Option<(Source, File, Sha256, u64)>,
}

pub(super) struct Publish {
    confinement: Arc<Confinement>,
    pub kind: Kind,
    destination: EnvironmentPath,
    /// The destination's parent directory and its name there.
    parent: Arc<Directory>,
    name: OsString,
    parent_segments: Vec<OsString>,
    pub destination_segments: Vec<OsString>,
    /// Directories `begin` created for the destination; removed again
    /// unless a commit published into them.
    created_parents: Vec<Vec<OsString>>,
    /// Name of the staging directory, chosen at begin and created by
    /// [`Publish::create_stage`] once it is journaled.
    stage_name: OsString,
    stage: Option<Stage>,
    limits: TransferLimits,
    pub entries: Vec<InventoryEntry>,
    pages: BTreeMap<u32, (Vec<InventoryEntry>, bool)>,
    paths: HashMap<TreePath, bool>,
    sizes: BTreeMap<ContentDigest, u64>,
    pub total_bytes: u64,
    manifest_bytes: u64,
    scan: Option<DestinationScan>,
    reusable: BTreeMap<ContentDigest, Source>,
    pub uploads: BTreeMap<ContentDigest, Arc<Mutex<Upload>>>,
    spool: Option<tempfile::TempDir>,
    staging_index: usize,
    copying: Option<Copying>,
    pub reused_bytes: u64,
}

impl Publish {
    pub fn begin(
        confinement: Arc<Confinement>,
        destination: &EnvironmentPath,
        kind: Kind,
        limits: TransferLimits,
        spool: tempfile::TempDir,
    ) -> Result<Self, ProtocolError> {
        let resolved = match &kind {
            Kind::Materialize { on_existing } => {
                let resolved = confinement.lookup(
                    destination,
                    Lookup::NO_FOLLOW.allowing_missing().creating_parents(),
                )?;
                if let Some(stat) = &resolved.stat {
                    if stat.kind == FileKind::Symlink {
                        return Err(ProtocolError::failed_precondition(
                            PreconditionReason::SymlinkRejected,
                            format!("{destination} is a symlink"),
                        ));
                    }
                    if stat.kind == FileKind::Other {
                        return Err(ProtocolError::failed_precondition(
                            PreconditionReason::NotRegularFile,
                            format!("{destination} is a special file"),
                        ));
                    }
                    if *on_existing == TransferOnExisting::Error {
                        return Err(ProtocolError::already_exists(format!(
                            "{destination} exists"
                        )));
                    }
                }
                resolved
            }
            Kind::WriteFile {
                create, if_match, ..
            } => {
                let resolved =
                    confinement.lookup(destination, Lookup::FOLLOW.allowing_missing())?;
                write_preconditions(
                    destination,
                    resolved.stat.as_ref(),
                    *create,
                    if_match.as_ref(),
                )?;
                resolved
            }
        };
        if resolved.is_root() {
            return Err(ProtocolError::failed_precondition(
                PreconditionReason::FilesystemRoot,
                "the filesystem root cannot be replaced",
            ));
        }
        if let Err(error) = confinement.refuse_containing_state(&resolved, destination) {
            confinement.remove_created(&resolved.created);
            return Err(error);
        }
        let Resolved {
            parent,
            name,
            segments,
            created,
            ..
        } = resolved;
        Ok(Self {
            confinement,
            kind,
            destination: destination.clone(),
            parent_segments: segments[..segments.len() - 1].to_vec(),
            destination_segments: segments,
            created_parents: created,
            parent: Arc::new(parent),
            name,
            stage_name: OsString::from(format!("{STAGE_PREFIX}{:032x}", rand::random::<u128>())),
            stage: None,
            limits,
            entries: Vec::new(),
            pages: BTreeMap::new(),
            paths: HashMap::new(),
            sizes: BTreeMap::new(),
            total_bytes: 0,
            manifest_bytes: 0,
            scan: None,
            reusable: BTreeMap::new(),
            uploads: BTreeMap::new(),
            spool: Some(spool),
            staging_index: 0,
            copying: None,
            reused_bytes: 0,
        })
    }

    /// Create the staging directory next to the destination. Called once
    /// its record is journaled, so a restart always finds and removes it.
    pub fn create_stage(&mut self) -> Result<(), ProtocolError> {
        let io = |error| io_error(error, &self.destination);
        self.parent
            .mkdir(&self.stage_name, os::PRIVATE_DIRECTORY_MODE)
            .map_err(io)?;
        let directory = self.parent.child(&self.stage_name).map_err(io)?;
        self.stage = Some(Stage {
            parent: self.parent.clone(),
            name: self.stage_name.clone(),
            directory,
        });
        Ok(())
    }

    /// Remove everything `begin` created, at once: a begin refused before
    /// the operation was recorded leaves nothing behind.
    pub fn discard(mut self) {
        if let Some(stage) = self.stage.take() {
            remove_stage(stage);
        }
        self.confinement
            .remove_created(&std::mem::take(&mut self.created_parents));
        drop(self.spool.take());
    }

    /// Where the staging directory is (or is about to be) created.
    pub fn stage_record(&self) -> StageRecord {
        StageRecord::new(
            &self.parent_segments,
            self.stage_name.to_str().expect("stage names are ASCII"),
        )
    }

    /// [`Self::stage_record`] once the staging directory exists.
    pub fn created_stage_record(&self) -> Option<StageRecord> {
        self.stage.as_ref().map(|_| self.stage_record())
    }

    /// The page at `index` was already appended with these entries.
    pub fn is_repeat(&self, index: u32, entries: &[InventoryEntry], last: bool) -> bool {
        self.pages
            .get(&index)
            .is_some_and(|(previous, sealed)| previous == entries && *sealed == last)
    }

    pub fn spool_path(&self) -> Option<PathBuf> {
        self.spool.as_ref().map(|spool| spool.path().to_owned())
    }

    /// Validate and add one inventory page; `Ok(true)` when it sealed the
    /// inventory. A repeat of an identical page changes nothing.
    pub fn append(
        &mut self,
        index: u32,
        entries: Vec<InventoryEntry>,
        last: bool,
    ) -> Result<bool, ProtocolError> {
        if let Some((previous, sealed)) = self.pages.get(&index) {
            if *previous == entries && *sealed == last {
                return Ok(false);
            }
            return Err(ProtocolError::failed_precondition(
                PreconditionReason::TransferOffset,
                format!("page {index} was appended with different entries"),
            ));
        }
        if index as usize != self.entries.len() {
            return Err(ProtocolError::failed_precondition(
                PreconditionReason::TransferOffset,
                format!("the next inventory index is {}", self.entries.len()),
            ));
        }
        if entries.is_empty() && !(last && !self.entries.is_empty()) {
            return Err(ProtocolError::invalid_field(
                "entries",
                "an inventory page needs at least one entry",
            ));
        }
        let single_file = matches!(self.kind, Kind::WriteFile { .. });
        let mut paths = HashMap::new();
        let mut sizes = BTreeMap::new();
        let mut total = self.total_bytes;
        let mut manifest = self.manifest_bytes;
        for (offset, entry) in entries.iter().enumerate() {
            let position = self.entries.len() + offset;
            let field = |member: &str| format!("entries[{offset}]{member}");
            if entry.path.depth() > self.limits.max_depth {
                return Err(ProtocolError::invalid_field(
                    field(".path"),
                    "deeper than limits.maxDepth",
                ));
            }
            if position == 0 {
                if !entry.path.is_root() {
                    return Err(ProtocolError::invalid_field(
                        field(".path"),
                        "the inventory starts with the root entry `.`",
                    ));
                }
            } else {
                if single_file {
                    return Err(ProtocolError::invalid_field(
                        "entries",
                        "a writeFile inventory holds exactly one file",
                    ));
                }
                if self.paths.contains_key(&entry.path) || paths.contains_key(&entry.path) {
                    return Err(ProtocolError::invalid_field(
                        field(".path"),
                        "duplicate path",
                    ));
                }
                let parent = parent_of(&entry.path);
                let parent_is_directory = self
                    .paths
                    .get(&parent)
                    .or_else(|| paths.get(&parent))
                    .copied()
                    .unwrap_or(false);
                if !parent_is_directory {
                    return Err(ProtocolError::invalid_field(
                        field(".path"),
                        "a parent directory must precede its children",
                    ));
                }
            }
            let is_directory = matches!(entry.content, TreeContent::Directory {});
            if single_file && is_directory {
                return Err(ProtocolError::invalid_field(
                    field(".content"),
                    "a writeFile inventory holds one file",
                ));
            }
            paths.insert(entry.path.clone(), is_directory);
            if let TreeContent::File {
                size_bytes, digest, ..
            } = &entry.content
            {
                let Some(digest) = digest else {
                    return Err(ProtocolError::invalid_field(
                        field(".content.digest"),
                        "inventory files carry their digest",
                    ));
                };
                if *size_bytes > self.limits.max_file_bytes {
                    return Err(ProtocolError::invalid_field(
                        field(".content.sizeBytes"),
                        "larger than limits.maxFileBytes",
                    ));
                }
                let known = self.sizes.get(digest).or_else(|| sizes.get(digest));
                if known.is_some_and(|known| known != size_bytes) {
                    return Err(ProtocolError::invalid_field(
                        field(".content.sizeBytes"),
                        "one digest with two sizes",
                    ));
                }
                sizes.insert(*digest, *size_bytes);
                total = total.saturating_add(*size_bytes);
            }
            manifest += entry_bytes(entry);
        }
        check_entry_quota(self.entries.len() + entries.len(), &self.limits)?;
        if total > self.limits.max_total_bytes {
            return Err(ProtocolError::invalid_field(
                "entries",
                "the inventory exceeds limits.maxTotalBytes",
            ));
        }
        if manifest > self.limits.max_manifest_bytes {
            return Err(ProtocolError::invalid_field(
                "entries",
                "the inventory exceeds limits.maxManifestBytes",
            ));
        }
        if single_file && !last {
            return Err(ProtocolError::invalid_field(
                "last",
                "a writeFile inventory is one page",
            ));
        }
        self.pages.insert(index, (entries.clone(), last));
        self.paths.extend(paths);
        self.sizes.extend(sizes);
        self.entries.extend(entries);
        self.total_bytes = total;
        self.manifest_bytes = manifest;
        Ok(last)
    }

    /// Start the destination scan after the inventory was sealed.
    pub fn start_scan(&mut self) -> Result<(), ProtocolError> {
        let current = self.parent.stat(&self.name);
        let stat = match current {
            Ok(stat) => Some(stat),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(io_error(error, &self.destination)),
        };
        let mut scan = DestinationScan {
            walker: None,
            single: None,
            hashing: None,
        };
        match stat {
            Some(stat) if stat.is_dir() && matches!(self.kind, Kind::Materialize { .. }) => {
                let resolved = self
                    .confinement
                    .reopen(&self.destination_segments, &self.destination)?;
                scan.walker = Some(Walker::new(
                    self.confinement.clone(),
                    &resolved,
                    CompiledFilter::everything(),
                    LinkMode::Report,
                    None,
                    &self.destination,
                )?);
            }
            Some(stat) if stat.is_file() => {
                scan.single = Some(Source {
                    canonical: self.destination_segments.clone(),
                    stat,
                });
            }
            _ => {}
        }
        self.scan = Some(scan);
        Ok(())
    }

    /// One bounded step of the destination scan; `true` when it is done.
    /// Reuse is an optimisation: unreadable destination entries are left
    /// out, never fatal.
    pub fn scan_step(&mut self) -> Result<bool, ProtocolError> {
        let wanted: HashSet<u64> = self
            .missing_digests()
            .iter()
            .map(|digest| self.sizes[digest])
            .collect();
        let mut allowance = STEP_BYTES;
        let Some(scan) = self.scan.as_mut() else {
            return Ok(true);
        };
        for _ in 0..STEP_ENTRIES {
            if let Some((_, file, hash, read)) = scan.hashing.as_mut() {
                let mut buffer = vec![0u8; 64 * 1024];
                let mut finished = false;
                while allowance > 0 {
                    let limit = (buffer.len() as u64).min(allowance) as usize;
                    match file.read(&mut buffer[..limit]) {
                        Ok(0) => {
                            finished = true;
                            break;
                        }
                        Ok(count) => {
                            hash.update(&buffer[..count]);
                            *read += count as u64;
                            allowance -= count as u64;
                        }
                        Err(_) => {
                            scan.hashing = None;
                            break;
                        }
                    }
                }
                if !finished {
                    if scan.hashing.is_some() {
                        return Ok(false);
                    }
                    continue;
                }
                let (source, file, hash, read) = scan.hashing.take().expect("hashing above");
                let digest = ContentDigest::from_sha256(hash.finalize().into());
                let unchanged = EntryStat::of_file(&file)
                    .is_ok_and(|stat| read == source.stat.size && stat.unchanged(&source.stat));
                if unchanged && self.sizes.contains_key(&digest) {
                    self.reusable.entry(digest).or_insert(source);
                }
                continue;
            }
            let candidate = if let Some(single) = scan.single.take() {
                Some((
                    single.clone(),
                    self.confinement
                        .reopen(&single.canonical, &self.destination),
                ))
            } else if let Some(walker) = scan.walker.as_mut() {
                match walker.next_event() {
                    Some(Event::Entry(entry))
                        if entry.stat.is_file() && wanted.contains(&entry.stat.size) =>
                    {
                        let source = Source {
                            canonical: entry.canonical.clone(),
                            stat: entry.stat,
                        };
                        let file = entry.parent.open_read(&entry.name);
                        if let Ok(file) = file {
                            scan.hashing = Some((source, file, Sha256::new(), 0));
                        }
                        continue;
                    }
                    Some(_) => continue,
                    None => {
                        scan.walker = None;
                        None
                    }
                }
            } else {
                None
            };
            match candidate {
                Some((source, Ok(resolved))) if wanted.contains(&source.stat.size) => {
                    if let Ok(file) = resolved.parent.open_read(&resolved.name) {
                        scan.hashing = Some((source, file, Sha256::new(), 0));
                    }
                }
                Some(_) => {}
                None => {
                    self.scan = None;
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    /// Digests whose content is neither empty, reusable nor uploaded.
    pub fn missing_digests(&self) -> Vec<ContentDigest> {
        self.sizes
            .iter()
            .filter(|(digest, size)| {
                **size > 0
                    && !self.reusable.contains_key(*digest)
                    && !self.uploads.get(*digest).is_some_and(|upload| {
                        upload.lock().is_ok_and(|upload| upload.is_complete())
                    })
            })
            .map(|(digest, _)| *digest)
            .collect()
    }

    /// The upload of `digest`, created on first use. `None` when the digest
    /// is not in the inventory.
    pub fn upload(
        &mut self,
        digest: &ContentDigest,
    ) -> Result<Option<Arc<Mutex<Upload>>>, ProtocolError> {
        let Some(size) = self.sizes.get(digest).copied() else {
            return Ok(None);
        };
        if let Some(upload) = self.uploads.get(digest) {
            return Ok(Some(upload.clone()));
        }
        let spool = self
            .spool
            .as_ref()
            .ok_or_else(|| ProtocolError::internal("the upload spool is gone"))?;
        let path = spool.path().join(digest.hex());
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| io_error(error, "the upload spool"))?;
        let upload = Arc::new(Mutex::new(Upload {
            file,
            path,
            hash: Sha256::new(),
            length: 0,
            size,
            complete: false,
        }));
        self.uploads.insert(*digest, upload.clone());
        Ok(Some(upload))
    }

    /// One bounded staging step; `true` when the tree is staged.
    pub fn stage_step(&mut self) -> Result<bool, ProtocolError> {
        let mut allowance = STEP_BYTES;
        for _ in 0..STEP_ENTRIES {
            if self.copying.is_some() {
                if !self.copy_step(&mut allowance)? {
                    return Ok(false);
                }
                continue;
            }
            let Some(entry) = self.entries.get(self.staging_index).cloned() else {
                return Ok(true);
            };
            let stage = self.stage.as_ref().expect("staging before commit");
            let (directory, name) = staged_parent(&stage.directory, &entry.path)
                .map_err(|error| io_error(error, &entry.path))?;
            match entry.content {
                TreeContent::Directory {} => {
                    directory
                        .mkdir(&name, DEFAULT_DIRECTORY_MODE)
                        .map_err(|error| io_error(error, &entry.path))?;
                    directory
                        .sync()
                        .map_err(|error| io_error(error, &entry.path))?;
                    self.staging_index += 1;
                }
                TreeContent::File {
                    size_bytes,
                    executable,
                    digest,
                } => {
                    let digest = digest.expect("appended files carry digests");
                    let mode = if executable {
                        os::with_execute_bits(DEFAULT_FILE_MODE)
                    } else {
                        DEFAULT_FILE_MODE
                    };
                    let destination = directory
                        .create_file(&name, os::PRIVATE_FILE_MODE)
                        .map_err(|error| io_error(error, &entry.path))?;
                    let (source, changed) = self.content_source(&digest, size_bytes)?;
                    self.copying = Some(Copying {
                        source,
                        destination,
                        hash: Sha256::new(),
                        expected: digest,
                        copied: 0,
                        size: size_bytes,
                        mode,
                        changed,
                    });
                }
            }
        }
        Ok(false)
    }

    /// The bytes of `digest`: an upload, a reusable destination file or
    /// nothing for empty content (an empty spool file stands in).
    fn content_source(
        &mut self,
        digest: &ContentDigest,
        size: u64,
    ) -> Result<(File, Option<Source>), ProtocolError> {
        if let Some(upload) = self.uploads.get(digest) {
            let upload = upload
                .lock()
                .map_err(|_| ProtocolError::internal("upload lock poisoned"))?;
            if upload.complete {
                let file = File::open(&upload.path)
                    .map_err(|error| io_error(error, "the upload spool"))?;
                return Ok((file, None));
            }
        }
        if let Some(source) = self.reusable.get(digest).cloned() {
            let resolved = self
                .confinement
                .reopen(&source.canonical, &self.destination)
                .map_err(|_| source_changed(&self.destination))?;
            let file = resolved
                .parent
                .open_read(&resolved.name)
                .map_err(|error| io_error(error, &self.destination))?;
            if !EntryStat::of_file(&file).is_ok_and(|stat| stat.unchanged(&source.stat)) {
                return Err(source_changed(&self.destination));
            }
            self.reused_bytes += size;
            return Ok((file, Some(source)));
        }
        if size == 0 {
            let spool = self
                .spool
                .as_ref()
                .ok_or_else(|| ProtocolError::internal("the upload spool is gone"))?;
            let path = spool.path().join("empty");
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
                .map_err(|error| io_error(error, "the upload spool"))?;
            return Ok((file, None));
        }
        Err(ProtocolError::internal(format!(
            "content of {digest} is missing at staging"
        )))
    }

    fn copy_step(&mut self, allowance: &mut u64) -> Result<bool, ProtocolError> {
        let copy = self.copying.as_mut().expect("caller checked");
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            if *allowance == 0 {
                return Ok(false);
            }
            let limit = (buffer.len() as u64).min(*allowance) as usize;
            let read = copy
                .source
                .read(&mut buffer[..limit])
                .map_err(|error| io_error(error, &self.destination))?;
            if read == 0 {
                break;
            }
            copy.copied += read as u64;
            if copy.copied > copy.size {
                return Err(copy_mismatch(copy, &self.destination));
            }
            copy.hash.update(&buffer[..read]);
            copy.destination
                .write_all(&buffer[..read])
                .map_err(|error| io_error(error, &self.destination))?;
            *allowance -= read as u64;
        }
        let copy = self.copying.take().expect("copying above");
        let digest = ContentDigest::from_sha256(copy.hash.clone().finalize().into());
        if copy.copied != copy.size || digest != copy.expected {
            return Err(copy_mismatch(&copy, &self.destination));
        }
        os::set_mode(&copy.destination, copy.mode)
            .and_then(|()| copy.destination.sync_all())
            .map_err(|error| io_error(error, &self.destination))?;
        self.staging_index += 1;
        Ok(true)
    }

    /// Publish the staged tree with one rename.
    pub fn commit(&mut self) -> Result<(), ProtocolError> {
        let stage = self.stage.as_ref().expect("commit before release");
        let staged = OsStr::new(STAGED_TREE);
        let io = |error| io_error(error, &self.destination);
        match &self.kind {
            Kind::Materialize { on_existing } => {
                let published =
                    stage
                        .directory
                        .rename(staged, &self.parent, &self.name, RenameMode::NoReplace);
                match published {
                    Ok(()) => {}
                    Err(error)
                        if error.kind() == std::io::ErrorKind::AlreadyExists
                            && *on_existing == TransferOnExisting::Replace =>
                    {
                        // The previous tree lands in the staging directory,
                        // which is removed with the operation.
                        stage
                            .directory
                            .rename(staged, &self.parent, &self.name, RenameMode::Exchange)
                            .map_err(io)?;
                    }
                    Err(error) => return Err(io(error)),
                }
            }
            Kind::WriteFile {
                create,
                if_match,
                executable,
            } => {
                let current = match self.parent.stat(&self.name) {
                    Ok(stat) => Some(stat),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => return Err(io(error)),
                };
                write_preconditions(
                    &self.destination,
                    current.as_ref(),
                    *create,
                    if_match.as_ref(),
                )?;
                let base = current.map_or(DEFAULT_FILE_MODE, |stat| stat.mode);
                let mode = if *executable {
                    os::with_execute_bits(base)
                } else {
                    base
                };
                let file = stage.directory.open_read(staged).map_err(io)?;
                os::set_mode(&file, mode)
                    .and_then(|()| file.sync_all())
                    .map_err(io)?;
                let rename = if *create == WriteCondition::IfAbsent {
                    RenameMode::NoReplace
                } else {
                    RenameMode::Replace
                };
                stage
                    .directory
                    .rename(staged, &self.parent, &self.name, rename)
                    .map_err(io)?;
            }
        }
        self.parent.sync().map_err(io)?;
        // The published tree lives in the directories begin created.
        self.created_parents.clear();
        Ok(())
    }
}

impl Drop for Publish {
    fn drop(&mut self) {
        let stage = self.stage.take();
        let created = std::mem::take(&mut self.created_parents);
        if stage.is_some() || !created.is_empty() {
            // A replaced tree may be large; removal never delays a receipt.
            // The receipt keeps the stage's record until a sweep confirms
            // it is gone, so an exit before this thread finishes leaks
            // nothing.
            let confinement = self.confinement.clone();
            let _ = std::thread::Builder::new()
                .name("transfer-stage-cleanup".into())
                .spawn(move || {
                    if let Some(stage) = stage {
                        remove_stage(stage);
                    }
                    confinement.remove_created(&created);
                });
        }
        if let Some(spool) = self.spool.take() {
            let _ = std::thread::Builder::new()
                .name("transfer-spool-cleanup".into())
                .spawn(move || drop(spool));
        }
    }
}

fn remove_stage(stage: Stage) {
    drop(stage.directory);
    if let Err(error) = stage.parent.remove_tree(&stage.name)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!(
            "lightspeed-envd: removing transfer staging {}: {error}",
            stage.name.to_string_lossy()
        );
    }
}

fn copy_mismatch(copy: &Copying, destination: &EnvironmentPath) -> ProtocolError {
    match &copy.changed {
        Some(_) => source_changed(destination),
        None => ProtocolError::failed_precondition(
            PreconditionReason::DigestMismatch,
            format!(
                "staged content of {} does not match its digest",
                copy.expected
            ),
        ),
    }
}

fn parent_of(path: &TreePath) -> TreePath {
    match path.as_str().rsplit_once('/') {
        Some((parent, _)) => TreePath::new(parent).expect("a parent of a tree path is one"),
        None => TreePath::root(),
    }
}

/// The directory that holds `path` in the staged tree, and its name there.
fn staged_parent(stage: &Directory, path: &TreePath) -> std::io::Result<(Directory, OsString)> {
    if path.is_root() {
        return Ok((stage.try_clone()?, OsString::from(STAGED_TREE)));
    }
    let mut directory = stage.child(OsStr::new(STAGED_TREE))?;
    let segments: Vec<&str> = path.segments().collect();
    let (name, parents) = segments
        .split_last()
        .expect("a non-root path has a segment");
    for segment in parents {
        directory = directory.child(OsStr::new(segment))?;
    }
    Ok((directory, OsString::from(name)))
}

/// Remove a staging directory recorded by an operation a restart
/// interrupted.
pub(super) fn discard_stage(confinement: &Confinement, record: &StageRecord) {
    if !record.name.starts_with(STAGE_PREFIX) || record.name.contains('/') {
        return;
    }
    let segments = record.parent_segments();
    let Ok(resolved) = confinement.reopen(&segments, &"transfer staging") else {
        return;
    };
    if let Ok(parent) = resolved.open_directory() {
        let _ = parent.remove_tree(OsStr::new(&record.name));
    }
}
