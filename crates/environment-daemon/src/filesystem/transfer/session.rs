//! Bounded transfer sessions retain inventories and publish through the filesystem backend.
use crate::filesystem::backend::{
    self, Result, conflict, digest, error, invalid, io, relative, valid_digest, valid_path,
};
use environment_protocol::{
    data::{inventory::*, transfer::TransferOnExisting, transfer_session::*},
    error::EnvironmentProtocolErrorCode as Code,
    shared::ByteChunk,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    time::{Duration, Instant},
};
#[path = "journal.rs"]
mod journal;
use backend::{Directory, Observation};
#[derive(Clone)]
struct Source {
    path: String,
    observed: Observation,
}
struct Hashing {
    source: Source,
    file: File,
    hash: Sha256,
    read: u64,
}
struct Scanner {
    anchor: Option<Directory>,
    pending: Vec<String>,
    pending_bytes: usize,
    hashing: Option<Hashing>,
    entries: Vec<InventoryEntry>,
    files: BTreeMap<String, Source>,
    observed: Vec<Source>,
    limits: InventoryLimits,
    bytes: u64,
    manifest_bytes: u64,
    selected: String,
    ignore_errors: bool,
    patterns: Vec<glob::Pattern>,
    visited: u32,
    metadata_only: bool,
    skipped: bool,
}
impl Scanner {
    fn new(
        anchor: Directory,
        selected: String,
        limits: InventoryLimits,
        ignore_errors: bool,
    ) -> Self {
        Self {
            anchor: Some(anchor),
            pending_bytes: selected.len(),
            pending: vec![selected.clone()],
            hashing: None,
            entries: vec![],
            files: BTreeMap::new(),
            observed: vec![],
            limits,
            bytes: 0,
            manifest_bytes: 0,
            selected,
            ignore_errors,
            patterns: vec![],
            visited: 0,
            metadata_only: false,
            skipped: false,
        }
    }
    fn record(&mut self, source: Source, content: InventoryContent) -> Result<()> {
        let relative = source
            .path
            .strip_prefix(&self.selected)
            .unwrap()
            .trim_start_matches('/')
            .to_owned();
        if !valid_path(&relative) {
            return Err(invalid("invalid inventory path"));
        }
        let entry = InventoryEntry {
            path: relative,
            content,
        };
        self.manifest_bytes += serde_json::to_vec(&entry)
            .map_err(|e| invalid(&e.to_string()))?
            .len() as u64;
        if self.manifest_bytes > self.limits.max_manifest_bytes {
            return Err(invalid("manifest memory quota exceeded"));
        }
        self.entries.push(entry);
        self.observed.push(source);
        Ok(())
    }
    /// At most 4 MiB of hashing and 128 visited nodes per exchange. Directory
    /// enumeration is additionally bounded by the inventory entry/manifest quotas.
    fn advance(&mut self) -> Result<bool> {
        let mut allowance = 4 * 1024 * 1024;
        for _ in 0..MAX_INVENTORY_PAGE {
            if let Some(mut task) = self.hashing.take() {
                let mut buffer = [0u8; 64 * 1024];
                while allowance > 0 {
                    let limit = buffer.len().min(allowance);
                    let read = task.file.read(&mut buffer[..limit]).map_err(io)?;
                    if read == 0 {
                        if task.read != task.source.observed.size()
                            || !task
                                .source
                                .observed
                                .matches(&backend::observe(&task.file).map_err(io)?)
                        {
                            return Err(conflict());
                        }
                        let hash = digest(task.hash);
                        self.files
                            .entry(hash.clone())
                            .or_insert_with(|| task.source.clone());
                        let content = InventoryContent::File {
                            size_bytes: task.read,
                            executable: task.source.observed.executable(),
                            digest: hash,
                        };
                        self.record(task.source, content)?;
                        break;
                    }
                    task.read += read as u64;
                    if task.read > task.source.observed.size() {
                        return Err(conflict());
                    }
                    task.hash.update(&buffer[..read]);
                    allowance -= read;
                    if allowance == 0 {
                        self.hashing = Some(task);
                        return Ok(false);
                    }
                }
                continue;
            }
            let Some(path) = self.pending.pop() else {
                return Ok(true);
            };
            self.pending_bytes -= path.len();
            let anchor = self.anchor.as_ref().unwrap();
            let file = match if self.metadata_only {
                anchor.metadata(&path)
            } else {
                anchor.open(&path)
            } {
                Ok(file) => file,
                Err(_) if self.ignore_errors => {
                    self.skipped = true;
                    continue;
                }
                Err(e) => return Err(io(e)),
            };
            let observed = backend::observe(&file).map_err(io)?;
            self.visited += 1;
            if self.visited > self.limits.max_entries {
                return Err(invalid("entry quota exceeded"));
            }
            let rel = path
                .strip_prefix(&self.selected)
                .unwrap()
                .trim_start_matches('/');
            if !valid_path(rel)
                || (!rel.is_empty() && rel.split('/').count() > self.limits.max_depth as usize)
            {
                return Err(invalid("path/depth quota exceeded"));
            }
            let source = Source {
                path: path.clone(),
                observed,
            };
            if source.observed.is_dir() {
                let names =
                    Directory::names(&file, self.limits.max_entries as usize).map_err(io)?;
                if self.pending.len() + self.entries.len() + names.len()
                    > self.limits.max_entries as usize
                {
                    return Err(invalid("entry quota exceeded"));
                }
                // Bound the pending traversal too, before any names enter the queue.
                let pending_bytes = self.pending_bytes;
                let added_bytes: usize = names
                    .iter()
                    .map(|n| path.len() + usize::from(!path.is_empty()) + n.len())
                    .sum();
                if (pending_bytes + added_bytes) as u64 > self.limits.max_manifest_bytes {
                    return Err(invalid("traversal memory quota exceeded"));
                }
                self.pending_bytes += added_bytes;
                for name in names.into_iter().rev() {
                    if !valid_path(&name) {
                        return Err(invalid("unsupported filename"));
                    }
                    self.pending.push(if path.is_empty() {
                        name
                    } else {
                        format!("{path}/{name}")
                    });
                }
                self.record(source, InventoryContent::Directory)?;
            } else {
                if !self.patterns.is_empty() && !self.patterns.iter().any(|p| p.matches(rel)) {
                    continue;
                }
                self.bytes = self
                    .bytes
                    .checked_add(source.observed.size())
                    .ok_or_else(|| invalid("size overflow"))?;
                if source.observed.size() > self.limits.max_file_bytes
                    || self.bytes > self.limits.max_total_bytes
                {
                    return Err(invalid("content quota exceeded"));
                }
                if self.metadata_only {
                    let content = InventoryContent::File {
                        size_bytes: source.observed.size(),
                        executable: source.observed.executable(),
                        digest: String::new(),
                    };
                    self.record(source, content)?;
                    continue;
                }
                self.hashing = Some(Hashing {
                    source,
                    file,
                    hash: Sha256::new(),
                    read: 0,
                });
            }
        }
        Ok(false)
    }
    fn verify(&self) -> Result<()> {
        for source in &self.observed {
            let file = self
                .anchor
                .as_ref()
                .unwrap()
                .open(&source.path)
                .map_err(io)?;
            if !source
                .observed
                .matches(&backend::observe(&file).map_err(io)?)
            {
                return Err(conflict());
            }
        }
        Ok(())
    }
}
struct Stage {
    parent: Directory,
    directory: Directory,
    name: String,
    target: String,
}
impl Drop for Stage {
    fn drop(&mut self) {
        if let Ok(parent) = self.parent.clone_dir() {
            let name = self.name.clone();
            // Retired trees may be large. Cleanup cannot delay the publication receipt.
            let _ = std::thread::Builder::new()
                .name("transfer-cleanup".into())
                .spawn(move || {
                    if let Err(error) = parent.remove_tree(&name)
                        && error.kind() != std::io::ErrorKind::NotFound
                    {
                        eprintln!("lightspeed-envd retirement cleanup {name}: {error}");
                    }
                });
        }
    }
}
struct Upload {
    path: std::path::PathBuf,
    hash: Sha256,
    length: u64,
    complete: bool,
}
struct Copying {
    source: File,
    destination: File,
    hash: Sha256,
    expected: String,
    bytes: u64,
    size: u64,
    executable: bool,
}
struct Operation {
    request: TransferRequest,
    status: TransferStatus,
    started: Instant,
    limits: InventoryLimits,
    scanner: Scanner,
    stage: Option<Stage>,
    entries: Vec<InventoryEntry>,
    manifest_bytes: u64,
    paths: BTreeMap<String, bool>,
    sizes: BTreeMap<String, u64>,
    pages: BTreeMap<u32, (Vec<InventoryEntry>, bool)>,
    uploads: BTreeMap<String, Upload>,
    spool: Option<tempfile::TempDir>,
    retained_inventory: Option<tempfile::TempPath>,
    staging_index: usize,
    copying: Option<Copying>,
    unchanged: bool,
    staging_name: Option<String>,
}
#[derive(Default)]
pub struct TransferManager {
    operations: BTreeMap<String, Operation>,
    journal: Option<std::path::PathBuf>,
}
impl TransferManager {
    pub fn with_journal(directory: std::path::PathBuf) -> Result<Self> {
        backend::create_private_directory(&directory).map_err(io)?;
        Ok(Self {
            operations: BTreeMap::new(),
            journal: Some(directory),
        })
    }
    pub fn expire(&mut self) -> Option<std::path::PathBuf> {
        self.operations.retain(|_, op| {
            op.started.elapsed() < Duration::from_millis(op.limits.max_duration_ms)
        });
        self.journal.clone()
    }
    pub fn cleanup_expired(directory: &Path) {
        if let Ok(entries) = std::fs::read_dir(directory) {
            for entry in entries.flatten().take(8192) {
                if entry.path().extension().is_some_and(|s| s == "json") {
                    let id = entry
                        .file_name()
                        .to_string_lossy()
                        .trim_end_matches(".json")
                        .to_owned();
                    if let Ok(Some(record)) = journal::read(directory, &id)
                        && record.expires_at_ms < journal::now_ms()
                    {
                        if let (
                            Some(name),
                            TransferRequest::Begin {
                                selection: TransferSelection::Materialize { destination, .. },
                                ..
                            },
                        ) = (&record.stage_name, &record.request)
                            && name.starts_with(".env-transfer-")
                            && valid_path(name)
                            && !name.contains('/')
                            && let Ok(anchor) = Directory::anchor(&record.root)
                            && let Ok(relative) = relative(&record.root, destination)
                            && let Ok((parent, _)) = anchor.parent(&relative)
                            && let Err(error) = parent.remove_tree(name)
                            && error.kind() != std::io::ErrorKind::NotFound
                        {
                            continue;
                        }
                        if let Some(spool) = &record.spool_directory
                            && spool.parent() == Some(directory)
                            && spool.file_name().is_some_and(|name| {
                                name.to_string_lossy()
                                    .starts_with(&format!("{id}.content-"))
                            })
                        {
                            let _ = std::fs::remove_dir_all(spool);
                        }
                        let _ = std::fs::remove_file(directory.join(format!("{id}.inventory")));
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
    }
    fn save(&self, root: &Path, id: &str) -> Result<()> {
        let Some(directory) = &self.journal else {
            return Ok(());
        };
        let op = &self.operations[id];
        if let Some(path) = &op.retained_inventory {
            let mut copy = tempfile::NamedTempFile::new_in(directory).map_err(io)?;
            std::io::copy(&mut File::open(path).map_err(io)?, &mut copy).map_err(io)?;
            copy.as_file().sync_all().map_err(io)?;
            copy.persist(directory.join(format!("{id}.inventory")))
                .map_err(|e| io(e.error))?;
        }
        journal::write(
            directory,
            id,
            &journal::Record {
                root: root.to_path_buf(),
                request: op.request.clone(),
                status: op.status.clone(),
                expires_at_ms: journal::now_ms()
                    + op.limits
                        .max_duration_ms
                        .saturating_sub(op.started.elapsed().as_millis() as u64),
                stage_name: op.staging_name.clone(),
                spool_directory: op.spool.as_ref().map(|s| s.path().to_path_buf()),
            },
        )
    }
    pub fn execute(&mut self, root: &Path, request: TransferRequest) -> Result<TransferResponse> {
        let id = request.operation_id().to_owned();
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            return Err(invalid("invalid operation id"));
        }
        if !self.operations.contains_key(&id)
            && let Some(directory) = &self.journal
            && let Some(record) = journal::read(directory, &id)?
        {
            if record.root != root {
                return Err(error(
                    Code::Forbidden,
                    "receipt belongs to a different filesystem scope",
                ));
            }
            if record.expires_at_ms < journal::now_ms() {
                return Err(error(Code::Timeout, "transfer receipt expired"));
            }
            if matches!(&request, TransferRequest::Begin { .. }) && request != record.request {
                return Err(error(
                    Code::Conflict,
                    "operation id already used for different input",
                ));
            }
            if matches!(
                request,
                TransferRequest::Begin { .. }
                    | TransferRequest::Status { .. }
                    | TransferRequest::Commit { .. }
                    | TransferRequest::Abort { .. }
            ) && record.status.phase == TransferPhase::Complete
            {
                return Ok(TransferResponse::Status(record.status));
            }
            if let TransferRequest::Inventory { offset, .. } = &request
                && record.status.phase == TransferPhase::Complete
            {
                return retained_page(
                    &directory.join(format!("{id}.inventory")),
                    record.status.entries,
                    *offset,
                );
            }
            if matches!(request, TransferRequest::Abort { .. }) {
                if let (
                    Some(name),
                    TransferRequest::Begin {
                        selection: TransferSelection::Materialize { destination, .. },
                        ..
                    },
                ) = (&record.stage_name, &record.request)
                    && name.starts_with(".env-transfer-")
                    && valid_path(name)
                    && !name.contains('/')
                {
                    let anchor = Directory::anchor(root).map_err(io)?;
                    let (parent, _) = anchor.parent(&relative(root, destination)?).map_err(io)?;
                    let _ = parent.remove_tree(name);
                }
                let mut record = record;
                record.status.phase = TransferPhase::Aborted;
                record.stage_name = None;
                if let Some(spool) = record.spool_directory.take()
                    && spool.parent() == Some(directory.as_path())
                    && spool.file_name().is_some_and(|name| {
                        name.to_string_lossy()
                            .starts_with(&format!("{id}.content-"))
                    })
                {
                    let _ = std::fs::remove_dir_all(spool);
                }
                journal::write(directory, &id, &record)?;
                return Ok(TransferResponse::Status(record.status));
            }
            return Err(error(
                Code::Conflict,
                "daemon restarted during transfer; inspect the destination and abort the old operation before starting a new one",
            ));
        }
        if let Some(op) = self.operations.get(&id)
            && op.started.elapsed() >= Duration::from_millis(op.limits.max_duration_ms)
        {
            return Err(error(Code::Timeout, "transfer expired"));
        }
        if let TransferRequest::Begin {
            selection, limits, ..
        } = &request
        {
            if let Some(op) = self.operations.get(&id) {
                return if op.request == request {
                    Ok(TransferResponse::Status(op.status.clone()))
                } else {
                    Err(error(
                        Code::Conflict,
                        "operation id already used for different input",
                    ))
                };
            }
            self.operations.retain(|_, op| {
                op.started.elapsed() < Duration::from_millis(op.limits.max_duration_ms)
            });
            if self.operations.len() >= 4096
                || self
                    .operations
                    .values()
                    .filter(|op| {
                        !matches!(
                            op.status.phase,
                            TransferPhase::Complete | TransferPhase::Aborted
                        )
                    })
                    .count()
                    >= 8
            {
                return Err(error(Code::Conflict, "transfer capacity reached"));
            }
            let ceiling = InventoryLimits::default();
            if limits.max_entries == 0
                || limits.max_entries > ceiling.max_entries
                || limits.max_depth > ceiling.max_depth
                || limits.max_file_bytes > ceiling.max_file_bytes
                || limits.max_total_bytes > ceiling.max_total_bytes
                || limits.max_manifest_bytes > ceiling.max_manifest_bytes
                || limits.max_duration_ms == 0
                || limits.max_duration_ms > ceiling.max_duration_ms
            {
                return Err(invalid("operation quotas exceed daemon ceilings"));
            }
            let anchor = Directory::anchor(root).map_err(io)?;
            let (selected, stage, ignore_errors) = match selection {
                TransferSelection::Capture { source } => (relative(root, source)?, None, false),
                TransferSelection::Materialize {
                    destination,
                    on_existing,
                } => {
                    let path = relative(root, destination)?;
                    if path.is_empty() {
                        return Err(invalid("cannot replace the filesystem root"));
                    }
                    for operation in self.operations.values().filter(|op| op.stage.is_some()) {
                        let other = &operation.scanner.selected;
                        if path == *other
                            || path
                                .strip_prefix(other)
                                .is_some_and(|tail| tail.starts_with('/'))
                            || other
                                .strip_prefix(&path)
                                .is_some_and(|tail| tail.starts_with('/'))
                        {
                            return Err(error(
                                Code::Conflict,
                                "destination overlaps an active materialization",
                            ));
                        }
                    }
                    let (parent, target) = anchor.ensure_parent(&path).map_err(io)?;
                    if parent.target_exists(&target).map_err(io)?
                        && *on_existing == TransferOnExisting::Error
                    {
                        return Err(error(Code::Conflict, "destination exists"));
                    }
                    let name = format!(".env-transfer-{:032x}", rand::random::<u128>());
                    let directory = parent.mkdir(&name, true).map_err(io)?;
                    (
                        path,
                        Some(Stage {
                            parent,
                            directory,
                            name,
                            target,
                        }),
                        true,
                    )
                }
            };
            let op = Operation {
                request: request.clone(),
                status: TransferStatus {
                    operation_id: id.clone(),
                    phase: TransferPhase::Scanning,
                    entries: 0,
                    bytes: 0,
                    transferred_bytes: 0,
                    reused_bytes: 0,
                },
                started: Instant::now(),
                limits: *limits,
                scanner: Scanner::new(anchor, selected, *limits, ignore_errors),
                staging_name: stage.as_ref().map(|stage| stage.name.clone()),
                stage,
                entries: vec![],
                manifest_bytes: 0,
                paths: BTreeMap::new(),
                sizes: BTreeMap::new(),
                pages: BTreeMap::new(),
                uploads: BTreeMap::new(),
                spool: Some(if let Some(directory) = &self.journal {
                    tempfile::Builder::new()
                        .prefix(&format!("{id}.content-"))
                        .tempdir_in(directory)
                        .map_err(io)?
                } else {
                    tempfile::tempdir().map_err(io)?
                }),
                retained_inventory: None,
                staging_index: 0,
                copying: None,
                unchanged: false,
            };
            let response = TransferResponse::Status(op.status.clone());
            self.operations.insert(id.clone(), op);
            if let Err(error) = self.save(root, &id) {
                self.operations.remove(&id);
                return Err(error);
            }
            return Ok(response);
        }
        let op = self.operations.get_mut(&id).ok_or_else(|| {
            error(
                Code::NotFound,
                "unknown transfer; operation receipts may have expired",
            )
        })?;
        if matches!(request, TransferRequest::Status { .. }) {
            return Ok(TransferResponse::Status(op.status.clone()));
        }
        if matches!(request, TransferRequest::Abort { .. }) {
            if op.status.phase != TransferPhase::Complete {
                op.status.phase = TransferPhase::Aborted;
                op.release();
            }
            let response = TransferResponse::Status(op.status.clone());
            self.save(root, &id)?;
            return Ok(response);
        }
        if op.started.elapsed() >= Duration::from_millis(op.limits.max_duration_ms) {
            return Err(error(Code::Timeout, "transfer expired"));
        }
        if op.status.phase == TransferPhase::Aborted {
            return Err(error(Code::Conflict, "transfer aborted"));
        }
        let terminal = matches!(request, TransferRequest::Commit { .. });
        let response = op.execute(request)?;
        if terminal {
            self.save(root, &id)?;
        }
        Ok(response)
    }
}
impl Drop for Operation {
    fn drop(&mut self) {
        self.release();
    }
}
impl Operation {
    fn retain_inventory(&mut self) -> Result<()> {
        let mut spool = tempfile::NamedTempFile::new().map_err(io)?;
        let count = self.entries.len();
        spool
            .seek(SeekFrom::Start((8 * (count + 1)) as u64))
            .map_err(io)?;
        let mut positions = Vec::with_capacity(count + 1);
        for entry in &self.entries {
            positions.push(spool.stream_position().map_err(io)?);
            serde_json::to_writer(&mut spool, entry).map_err(|e| invalid(&e.to_string()))?;
        }
        positions.push(spool.stream_position().map_err(io)?);
        spool.seek(SeekFrom::Start(0)).map_err(io)?;
        for position in positions {
            spool.write_all(&position.to_le_bytes()).map_err(io)?;
        }
        self.retained_inventory = Some(spool.into_temp_path());
        Ok(())
    }
    fn retained_page(&self, offset: u32) -> Result<TransferResponse> {
        let path = self.retained_inventory.as_ref().ok_or_else(|| {
            error(
                Code::Conflict,
                "completed materialization retains only its receipt",
            )
        })?;
        retained_page(path, self.status.entries, offset)
    }
    fn release(&mut self) {
        self.stage.take();
        self.uploads.clear();
        self.copying.take();
        if let Some(spool) = self.spool.take() {
            let _ = std::thread::Builder::new()
                .name("transfer-spool-cleanup".into())
                .spawn(move || drop(spool));
        }
        self.scanner.anchor.take();
        self.scanner.pending = Vec::new();
        self.scanner.hashing.take();
        self.scanner.files.clear();
        self.scanner.observed = Vec::new();
        self.scanner.entries = Vec::new();
        self.paths.clear();
        self.sizes.clear();
        self.pages.clear();
        self.entries = Vec::new();
    }
    fn status(&self) -> TransferResponse {
        TransferResponse::Status(self.status.clone())
    }
    fn execute(&mut self, request: TransferRequest) -> Result<TransferResponse> {
        match request {
            TransferRequest::Advance { .. } => {
                if self.status.phase == TransferPhase::Scanning
                    && match self.scanner.advance() {
                        Ok(done) => done,
                        Err(_) if self.stage.is_some() => {
                            self.scanner.skipped = true;
                            self.scanner.pending.clear();
                            self.scanner.hashing.take();
                            true
                        }
                        Err(error) => return Err(error),
                    }
                {
                    if self.stage.is_some() {
                        self.status.phase = TransferPhase::Inventory;
                    } else {
                        self.scanner.verify()?;
                        self.entries = self.scanner.entries.clone();
                        self.status.entries = self.entries.len() as u32;
                        self.status.bytes = self.scanner.bytes;
                        self.status.phase = TransferPhase::Ready;
                    }
                }
                if self.status.phase == TransferPhase::Content && self.missing().is_empty() {
                    if !self.scanner.skipped && self.entries == self.scanner.entries {
                        self.scanner.verify()?;
                        self.unchanged = true;
                        self.status.reused_bytes = self.status.bytes;
                        self.status.phase = TransferPhase::Ready;
                    } else {
                        self.status.phase = TransferPhase::Staging;
                    }
                }
                if self.status.phase == TransferPhase::Staging {
                    self.advance_staging()?;
                }
                Ok(self.status())
            }
            TransferRequest::Inventory { offset, .. } => {
                if self.status.phase == TransferPhase::Complete {
                    return self.retained_page(offset);
                }
                if matches!(
                    self.status.phase,
                    TransferPhase::Scanning | TransferPhase::Inventory
                ) {
                    return Err(error(Code::Conflict, "inventory not complete"));
                }
                let offset = offset as usize;
                if offset > self.entries.len() {
                    return Err(invalid("inventory offset"));
                }
                let end = (offset + MAX_INVENTORY_PAGE).min(self.entries.len());
                Ok(TransferResponse::Inventory {
                    entries: self.entries[offset..end].to_vec(),
                    next_offset: (end < self.entries.len()).then_some(end as u32),
                })
            }
            TransferRequest::Append {
                offset,
                entries,
                last,
                ..
            } => {
                if self.stage.is_none() || entries.len() > MAX_INVENTORY_PAGE {
                    return Err(invalid("invalid inventory page"));
                }
                if let Some((previous, sealed)) = self.pages.get(&offset) {
                    if previous != &entries || *sealed != last {
                        return Err(error(Code::Conflict, "inventory retry differs"));
                    }
                    return Ok(self.status());
                }
                let offset = offset as usize;
                if self.status.phase != TransferPhase::Inventory || offset != self.entries.len() {
                    return Err(error(Code::Conflict, "inventory offset/phase mismatch"));
                }
                if entries.is_empty() {
                    return Err(invalid("empty inventory page"));
                }
                // Validate the complete page before mutating the operation.
                let mut total = self.status.bytes;
                let mut meta = self.manifest_bytes;
                let mut paths = BTreeMap::new();
                let mut sizes = BTreeMap::new();
                for (index, entry) in entries.iter().enumerate() {
                    if !valid_path(&entry.path)
                        || (!entry.path.is_empty()
                            && entry.path.split('/').count() > self.limits.max_depth as usize)
                    {
                        return Err(invalid("invalid inventory path"));
                    }
                    if offset + index == 0 {
                        if !entry.path.is_empty() {
                            return Err(invalid("inventory must start with root"));
                        }
                    } else {
                        let parent = entry.path.rsplit_once('/').map_or("", |(p, _)| p);
                        if self.paths.contains_key(&entry.path) || paths.contains_key(&entry.path) {
                            return Err(invalid("duplicate inventory path"));
                        }
                        if !self
                            .paths
                            .get(parent)
                            .or_else(|| paths.get(parent))
                            .copied()
                            .unwrap_or(false)
                        {
                            return Err(invalid("parent must precede child"));
                        }
                    }
                    paths.insert(
                        entry.path.clone(),
                        entry.content == InventoryContent::Directory,
                    );
                    if let InventoryContent::File {
                        size_bytes, digest, ..
                    } = &entry.content
                    {
                        if !valid_digest(digest) || *size_bytes > self.limits.max_file_bytes {
                            return Err(invalid("invalid file identity/size"));
                        }
                        if self
                            .sizes
                            .get(digest)
                            .or_else(|| sizes.get(digest))
                            .is_some_and(|s| s != size_bytes)
                            || self
                                .scanner
                                .files
                                .get(digest)
                                .is_some_and(|f| f.observed.size() != *size_bytes)
                        {
                            return Err(invalid("same digest has different sizes"));
                        }
                        sizes.insert(digest.clone(), *size_bytes);
                        total = total
                            .checked_add(*size_bytes)
                            .ok_or_else(|| invalid("size overflow"))?;
                    }
                    meta += serde_json::to_vec(entry)
                        .map_err(|e| invalid(&e.to_string()))?
                        .len() as u64;
                }
                if self.entries.len() + entries.len() > self.limits.max_entries as usize
                    || total > self.limits.max_total_bytes
                    || meta > self.limits.max_manifest_bytes
                {
                    return Err(invalid("inventory quota exceeded"));
                }
                self.pages.insert(offset as u32, (entries.clone(), last));
                self.paths.extend(paths);
                self.sizes.extend(sizes);
                self.entries.extend(entries);
                self.status.entries = self.entries.len() as u32;
                self.status.bytes = total;
                self.manifest_bytes = meta;
                if last {
                    if self.entries.is_empty() {
                        return Err(invalid("empty inventory"));
                    }
                    self.status.phase = TransferPhase::Content;
                }
                Ok(self.status())
            }
            TransferRequest::Missing { offset, .. } => {
                if !matches!(
                    self.status.phase,
                    TransferPhase::Content
                        | TransferPhase::Staging
                        | TransferPhase::Ready
                        | TransferPhase::Complete
                ) {
                    return Err(error(Code::Conflict, "inventory not sealed"));
                }
                let missing = self.missing();
                let offset = offset as usize;
                if offset > missing.len() {
                    return Err(invalid("missing offset"));
                }
                let end = (offset + MAX_INVENTORY_PAGE).min(missing.len());
                Ok(TransferResponse::Missing {
                    digests: missing[offset..end].to_vec(),
                    next_offset: (end < missing.len()).then_some(end as u32),
                })
            }
            TransferRequest::Read { digest, offset, .. } => {
                if self.stage.is_some() || self.status.phase != TransferPhase::Ready {
                    return Err(error(Code::Conflict, "capture is not ready"));
                }
                let source = self
                    .scanner
                    .files
                    .get(&digest)
                    .ok_or_else(|| invalid("digest outside capture"))?;
                let mut file = self
                    .scanner
                    .anchor
                    .as_ref()
                    .unwrap()
                    .open(&source.path)
                    .map_err(io)?;
                if !source
                    .observed
                    .matches(&backend::observe(&file).map_err(io)?)
                    || offset > source.observed.size()
                {
                    return Err(conflict());
                }
                file.seek(SeekFrom::Start(offset)).map_err(io)?;
                let mut data = vec![
                    0;
                    (source.observed.size() - offset).min(MAX_CONTENT_CHUNK as u64)
                        as usize
                ];
                file.read_exact(&mut data).map_err(io)?;
                if !source
                    .observed
                    .matches(&backend::observe(&file).map_err(io)?)
                {
                    return Err(conflict());
                }
                self.status.transferred_bytes += data.len() as u64;
                Ok(TransferResponse::Chunk {
                    eof: offset + data.len() as u64 == source.observed.size(),
                    data: ByteChunk::from(data),
                })
            }
            TransferRequest::Write {
                digest: expected,
                offset,
                data,
                ..
            } => {
                if self.status.phase != TransferPhase::Content
                    || data.as_slice().len() > MAX_CONTENT_CHUNK
                {
                    return Err(invalid("invalid upload chunk/phase"));
                }
                let size = self
                    .file_size(&expected)
                    .ok_or_else(|| invalid("digest outside inventory"))?;
                if !self.uploads.contains_key(&expected) {
                    if offset != 0 {
                        return Err(error(Code::Conflict, "upload starts at offset zero"));
                    }
                    let path = self.spool.as_ref().unwrap().path().join(&expected[7..]);
                    let file = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create_new(true)
                        .open(&path)
                        .map_err(io)?;
                    drop(file);
                    self.uploads.insert(
                        expected.clone(),
                        Upload {
                            path,
                            hash: Sha256::new(),
                            length: 0,
                            complete: false,
                        },
                    );
                }
                let upload = self.uploads.get_mut(&expected).unwrap();
                let mut file = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&upload.path)
                    .map_err(io)?;
                let bytes = data.as_slice();
                let end = offset
                    .checked_add(bytes.len() as u64)
                    .ok_or_else(|| invalid("chunk offset overflow"))?;
                if offset < upload.length || upload.complete {
                    if end > upload.length {
                        return Err(error(Code::Conflict, "overlapping chunk"));
                    }
                    file.seek(SeekFrom::Start(offset)).map_err(io)?;
                    let mut existing = vec![0; bytes.len()];
                    file.read_exact(&mut existing).map_err(io)?;
                    if existing != bytes {
                        return Err(error(Code::Conflict, "chunk retry differs"));
                    }
                    return Ok(TransferResponse::Written {
                        next_offset: upload.length,
                    });
                }
                if offset != upload.length || end > size || (bytes.is_empty() && size != 0) {
                    return Err(invalid("chunk offset/size mismatch"));
                }
                file.seek(SeekFrom::Start(offset)).map_err(io)?;
                file.write_all(bytes).map_err(io)?;
                upload.hash.update(bytes);
                upload.length += bytes.len() as u64;
                self.status.transferred_bytes += bytes.len() as u64;
                if upload.length == size {
                    if digest(upload.hash.clone()) != expected {
                        self.uploads.remove(&expected);
                        let _ = std::fs::remove_file(
                            self.spool.as_ref().unwrap().path().join(&expected[7..]),
                        );
                        return Err(error(Code::Conflict, "file digest mismatch"));
                    }
                    file.sync_all().map_err(io)?;
                    upload.complete = true;
                }
                Ok(TransferResponse::Written { next_offset: end })
            }
            TransferRequest::Commit { .. } => {
                if self.status.phase == TransferPhase::Complete {
                    return Ok(self.status());
                }
                if self.status.phase != TransferPhase::Ready {
                    return Err(error(Code::Conflict, "transfer is not ready to commit"));
                }
                if self.stage.is_none() {
                    self.retain_inventory()?;
                }
                if self.unchanged {
                    self.scanner.verify()?;
                } else if let Some(stage) = &self.stage {
                    let replace = matches!(
                        &self.request,
                        TransferRequest::Begin {
                            selection: TransferSelection::Materialize {
                                on_existing: TransferOnExisting::Replace,
                                ..
                            },
                            ..
                        }
                    );
                    stage.parent.target_exists(&stage.target).map_err(io)?;
                    stage
                        .parent
                        .publish(&stage.directory, &stage.target, replace)
                        .map_err(io)?;
                    // Rename has succeeded. A later durability error must never cause
                    // a second swap on retry.
                    self.status.phase = TransferPhase::Complete;
                    stage.parent.sync().map_err(io)?;
                    stage.directory.sync().map_err(io)?;
                } else {
                    self.scanner.verify()?;
                }
                self.status.phase = TransferPhase::Complete;
                self.release();
                Ok(self.status())
            }
            _ => Err(invalid("unexpected transfer action")),
        }
    }
    fn file_size(&self, digest: &str) -> Option<u64> {
        self.sizes.get(digest).copied()
    }
    fn missing(&self) -> Vec<String> {
        let mut missing = std::collections::BTreeSet::new();
        for digest in self.sizes.keys() {
            if !self.scanner.files.contains_key(digest)
                && !self.uploads.get(digest).is_some_and(|u| u.complete)
            {
                missing.insert(digest.clone());
            }
        }
        missing.into_iter().collect()
    }
    fn advance_staging(&mut self) -> Result<()> {
        let mut allowance = 4 * 1024 * 1024;
        for _ in 0..MAX_INVENTORY_PAGE {
            if let Some(mut copy) = self.copying.take() {
                let mut buffer = [0u8; 64 * 1024];
                while allowance > 0 {
                    let limit = buffer.len().min(allowance);
                    let read = copy.source.read(&mut buffer[..limit]).map_err(io)?;
                    if read == 0 {
                        if copy.bytes != copy.size || digest(copy.hash) != copy.expected {
                            return Err(conflict());
                        }
                        backend::set_executable(&copy.destination, copy.executable).map_err(io)?;
                        copy.destination.sync_all().map_err(io)?;
                        let entry = &self.entries[self.staging_index];
                        let path = if entry.path.is_empty() {
                            "tree".into()
                        } else {
                            format!("tree/{}", entry.path)
                        };
                        self.stage
                            .as_ref()
                            .unwrap()
                            .directory
                            .parent(&path)
                            .map_err(io)?
                            .0
                            .sync()
                            .map_err(io)?;
                        self.staging_index += 1;
                        break;
                    }
                    copy.bytes += read as u64;
                    if copy.bytes > copy.size {
                        return Err(conflict());
                    }
                    copy.hash.update(&buffer[..read]);
                    copy.destination.write_all(&buffer[..read]).map_err(io)?;
                    allowance -= read;
                    if allowance == 0 {
                        self.copying = Some(copy);
                        return Ok(());
                    }
                }
                continue;
            }
            let Some(entry) = self.entries.get(self.staging_index) else {
                self.status.phase = TransferPhase::Ready;
                return Ok(());
            };
            let stage = &self.stage.as_ref().unwrap().directory;
            let path = if entry.path.is_empty() {
                "tree".into()
            } else {
                format!("tree/{}", entry.path)
            };
            match &entry.content {
                InventoryContent::Directory => {
                    let (parent, name) = stage.parent(&path).map_err(io)?;
                    parent.mkdir(&name, false).map_err(io)?.sync().map_err(io)?;
                    parent.sync().map_err(io)?;
                    self.staging_index += 1;
                }
                InventoryContent::File {
                    digest,
                    size_bytes,
                    executable,
                } => {
                    let source =
                        if let Some(upload) = self.uploads.get(digest).filter(|u| u.complete) {
                            let mut f = File::open(&upload.path).map_err(io)?;
                            f.seek(SeekFrom::Start(0)).map_err(io)?;
                            f
                        } else {
                            let source = self
                                .scanner
                                .files
                                .get(digest)
                                .ok_or_else(|| invalid("missing content"))?;
                            let file = self
                                .scanner
                                .anchor
                                .as_ref()
                                .unwrap()
                                .open(&source.path)
                                .map_err(io)?;
                            if !source
                                .observed
                                .matches(&backend::observe(&file).map_err(io)?)
                            {
                                return Err(conflict());
                            }
                            self.status.reused_bytes += *size_bytes;
                            file
                        };
                    self.copying = Some(Copying {
                        source,
                        destination: stage.create(&path).map_err(io)?,
                        hash: Sha256::new(),
                        expected: digest.clone(),
                        bytes: 0,
                        size: *size_bytes,
                        executable: *executable,
                    });
                }
            }
        }
        Ok(())
    }
}

fn retained_page(path: &Path, count: u32, offset: u32) -> Result<TransferResponse> {
    if offset > count {
        return Err(invalid("inventory offset"));
    }
    let end = (offset + MAX_INVENTORY_PAGE as u32).min(count);
    let mut file = File::open(path).map_err(io)?;
    file.seek(SeekFrom::Start(offset as u64 * 8)).map_err(io)?;
    let mut positions = Vec::new();
    for _ in offset..=end {
        let mut bytes = [0; 8];
        file.read_exact(&mut bytes).map_err(io)?;
        positions.push(u64::from_le_bytes(bytes));
    }
    let mut entries = Vec::new();
    for pair in positions.windows(2) {
        let size = pair[1] - pair[0];
        if size > MAX_INVENTORY_PATH_BYTES as u64 + 1024 {
            return Err(invalid("corrupt retained inventory"));
        }
        file.seek(SeekFrom::Start(pair[0])).map_err(io)?;
        let mut bytes = vec![0; size as usize];
        file.read_exact(&mut bytes).map_err(io)?;
        entries.push(serde_json::from_slice(&bytes).map_err(|e| invalid(&e.to_string()))?);
    }
    Ok(TransferResponse::Inventory {
        entries,
        next_offset: (end < count).then_some(end),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use environment_protocol::shared::EnvironmentPath;

    #[test]
    fn expired_transfer_rejects_progress_and_publication() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::write(root.join("destination"), b"keep").unwrap();
        let mut manager = TransferManager::default();
        manager
            .execute(
                &root,
                TransferRequest::Begin {
                    operation_id: "expired".into(),
                    selection: TransferSelection::Materialize {
                        destination: EnvironmentPath::new(
                            root.join("destination").to_str().unwrap(),
                        )
                        .unwrap(),
                        on_existing: TransferOnExisting::Replace,
                    },
                    limits: InventoryLimits {
                        max_duration_ms: 1000,
                        ..Default::default()
                    },
                },
            )
            .unwrap();
        // Exercise expiry without waiting for wall-clock time or depending on
        // filesystem throughput and test-runner scheduling.
        manager.operations.get_mut("expired").unwrap().started =
            Instant::now() - Duration::from_secs(2);
        for request in [
            TransferRequest::Advance {
                operation_id: "expired".into(),
            },
            TransferRequest::Commit {
                operation_id: "expired".into(),
            },
        ] {
            assert_eq!(
                manager.execute(&root, request).unwrap_err().code,
                Code::Timeout
            );
        }
        assert_eq!(std::fs::read(root.join("destination")).unwrap(), b"keep");
    }

    #[test]
    fn scan_respects_remaining_budget_when_a_large_file_follows_a_small_file() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("source")).unwrap();
        std::fs::write(root.path().join("source/small"), [1; 17]).unwrap();
        std::fs::write(
            root.path().join("source/large"),
            vec![2; 4 * 1024 * 1024 + 13],
        )
        .unwrap();
        let mut scanner = Scanner::new(
            Directory::anchor(root.path()).unwrap(),
            "source".into(),
            InventoryLimits::default(),
            false,
        );
        // Make traversal order explicit: pending nodes are popped from the end.
        scanner.pending = vec!["source/large".into(), "source/small".into()];
        scanner.pending_bytes = scanner.pending.iter().map(String::len).sum();
        let progress = |scanner: &Scanner| {
            scanner
                .entries
                .iter()
                .map(|entry| match entry.content {
                    InventoryContent::File { size_bytes, .. } => size_bytes,
                    InventoryContent::Directory => 0,
                })
                .sum::<u64>()
                + scanner.hashing.as_ref().map_or(0, |task| task.read)
        };
        assert!(!scanner.advance().unwrap());
        assert_eq!(progress(&scanner), 4 * 1024 * 1024);
        assert!(scanner.advance().unwrap());
        assert_eq!(progress(&scanner), 4 * 1024 * 1024 + 30);
        scanner.verify().unwrap();
    }

    #[test]
    fn staging_respects_remaining_budget_when_a_large_file_follows_a_small_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("target")).unwrap();
        let small = vec![1; 17];
        let large = vec![2; 4 * 1024 * 1024 + 13];
        std::fs::write(root.join("target/small"), &small).unwrap();
        std::fs::write(root.join("target/large"), &large).unwrap();
        let mut manager = TransferManager::default();
        manager
            .execute(
                &root,
                TransferRequest::Begin {
                    operation_id: "bounded".into(),
                    selection: TransferSelection::Materialize {
                        destination: EnvironmentPath::new(root.join("target").to_str().unwrap())
                            .unwrap(),
                        on_existing: TransferOnExisting::Replace,
                    },
                    limits: InventoryLimits::default(),
                },
            )
            .unwrap();
        while manager.operations["bounded"].status.phase == TransferPhase::Scanning {
            manager
                .execute(
                    &root,
                    TransferRequest::Advance {
                        operation_id: "bounded".into(),
                    },
                )
                .unwrap();
        }
        let mut entries = vec![InventoryEntry {
            path: String::new(),
            content: InventoryContent::Directory,
        }];
        for (name, bytes) in [("renamed-small", &small), ("renamed-large", &large)] {
            entries.push(InventoryEntry {
                path: name.into(),
                content: InventoryContent::File {
                    size_bytes: bytes.len() as u64,
                    executable: false,
                    digest: engine::BlobRef::from_bytes(bytes).to_string(),
                },
            });
        }
        manager
            .execute(
                &root,
                TransferRequest::Append {
                    operation_id: "bounded".into(),
                    offset: 0,
                    entries,
                    last: true,
                },
            )
            .unwrap();
        manager
            .execute(
                &root,
                TransferRequest::Advance {
                    operation_id: "bounded".into(),
                },
            )
            .unwrap();
        let op = &manager.operations["bounded"];
        assert_eq!(op.status.phase, TransferPhase::Staging);
        let copied: u64 = op.entries[..op.staging_index]
            .iter()
            .map(|entry| match entry.content {
                InventoryContent::File { size_bytes, .. } => size_bytes,
                InventoryContent::Directory => 0,
            })
            .sum::<u64>()
            + op.copying.as_ref().map_or(0, |copy| copy.bytes);
        assert_eq!(copied, 4 * 1024 * 1024);
        manager
            .execute(
                &root,
                TransferRequest::Advance {
                    operation_id: "bounded".into(),
                },
            )
            .unwrap();
        assert_eq!(
            manager.operations["bounded"].status.phase,
            TransferPhase::Ready
        );
        manager
            .execute(
                &root,
                TransferRequest::Commit {
                    operation_id: "bounded".into(),
                },
            )
            .unwrap();
        assert_eq!(
            std::fs::read(root.join("target/renamed-small")).unwrap(),
            small
        );
        assert_eq!(
            std::fs::read(root.join("target/renamed-large")).unwrap(),
            large
        );
    }

    #[test]
    fn expiry_sweep_reclaims_abandoned_state_and_preserves_live_work() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        std::fs::create_dir(&state).unwrap();
        std::fs::write(root.path().join("destination"), b"keep").unwrap();
        for (id, expires_at_ms) in [
            ("expired", journal::now_ms() - 1),
            ("live", journal::now_ms() + 60_000),
        ] {
            let stage = format!(".env-transfer-{id}");
            std::fs::create_dir_all(root.path().join(&stage).join("nested")).unwrap();
            std::fs::write(root.path().join(&stage).join("nested/file"), b"staged").unwrap();
            let spool = state.join(format!("{id}.content-private"));
            std::fs::create_dir(&spool).unwrap();
            std::fs::write(spool.join("file"), b"uploaded").unwrap();
            std::fs::write(state.join(format!("{id}.inventory")), b"inventory").unwrap();
            journal::write(
                &state,
                id,
                &journal::Record {
                    root: root.path().into(),
                    request: TransferRequest::Begin {
                        operation_id: id.into(),
                        selection: TransferSelection::Materialize {
                            destination: EnvironmentPath::new(
                                root.path().join("destination").to_str().unwrap(),
                            )
                            .unwrap(),
                            on_existing: TransferOnExisting::Replace,
                        },
                        limits: Default::default(),
                    },
                    status: TransferStatus {
                        operation_id: id.into(),
                        phase: TransferPhase::Scanning,
                        entries: 0,
                        bytes: 0,
                        transferred_bytes: 0,
                        reused_bytes: 0,
                    },
                    expires_at_ms,
                    stage_name: Some(stage),
                    spool_directory: Some(spool),
                },
            )
            .unwrap();
        }
        TransferManager::cleanup_expired(&state);
        for suffix in ["json", "inventory", "content-private"] {
            assert!(!state.join(format!("expired.{suffix}")).exists());
            assert!(state.join(format!("live.{suffix}")).exists());
        }
        assert!(!root.path().join(".env-transfer-expired").exists());
        assert!(root.path().join(".env-transfer-live/nested/file").exists());
        assert_eq!(
            std::fs::read(root.path().join("destination")).unwrap(),
            b"keep"
        );
    }
}
