//! Capture: walk a source under the shared traversal, hash every selected
//! file in bounded steps, and serve the sealed inventory's content by
//! digest. A file that changes between hashing and reading fails the
//! operation (`aborted`, `sourceChanged`).

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::File,
    io::{Read, Seek, SeekFrom},
    sync::Arc,
};

use environment_protocol::{
    data::{
        fs::{FileKind, TreeContent, TreeFilter},
        transfer::{
            InventoryEntry, SpecialFilePolicy, SymlinkPolicy, TransferLimits, TransferSkip,
            TransferSkipReason,
        },
    },
    error::{AbortReason, PreconditionReason, ProtocolError},
    limits::MAX_LISTED_SKIPS,
    paths::{ContentDigest, EnvironmentPath, TreePath},
};
use sha2::{Digest, Sha256};

use crate::filesystem::{
    confinement::{Confinement, Lookup},
    io_error,
    os::{self, EntryStat},
    walk::{CompiledFilter, Event, LinkMode, Skip, Walker},
};

use super::{STEP_BYTES, STEP_ENTRIES, check_entry_quota, entry_bytes};

/// Where a captured digest's bytes live.
#[derive(Clone, Debug)]
pub(super) struct Source {
    pub canonical: Vec<OsString>,
    pub stat: EntryStat,
}

struct Hashing {
    path: TreePath,
    source: Source,
    file: File,
    hash: Sha256,
    read: u64,
}

pub(super) struct Capture {
    /// Canonical segments of the source; expanded links must stay inside.
    selection: Vec<OsString>,
    walker: Option<Walker>,
    hashing: Option<Hashing>,
    symlinks: SymlinkPolicy,
    special_files: SpecialFilePolicy,
    limits: TransferLimits,
    pub entries: Vec<InventoryEntry>,
    pub sources: BTreeMap<ContentDigest, Source>,
    pub total_bytes: u64,
    manifest_bytes: u64,
    pub skipped: Vec<TransferSkip>,
    pub skipped_count: u32,
}

impl Capture {
    pub fn begin(
        confinement: Arc<Confinement>,
        source: &EnvironmentPath,
        filter: &TreeFilter,
        symlinks: SymlinkPolicy,
        special_files: SpecialFilePolicy,
        limits: TransferLimits,
    ) -> Result<Self, ProtocolError> {
        let compiled = CompiledFilter::compile(filter, "selection.filter")?;
        let resolved = confinement.lookup(source, Lookup::FOLLOW)?;
        confinement.refuse_containing_state(&resolved, source)?;
        let stat = resolved.stat.expect("a followed lookup reports its target");
        let mut capture = Self {
            selection: resolved.segments.clone(),
            walker: None,
            hashing: None,
            symlinks,
            special_files,
            limits,
            entries: Vec::new(),
            sources: BTreeMap::new(),
            total_bytes: 0,
            manifest_bytes: 0,
            skipped: Vec::new(),
            skipped_count: 0,
        };
        match stat.kind {
            FileKind::Directory => {
                capture.record(InventoryEntry {
                    path: TreePath::root(),
                    content: TreeContent::Directory {},
                })?;
                let links = if symlinks == SymlinkPolicy::ExpandInside {
                    LinkMode::Follow
                } else {
                    LinkMode::Report
                };
                capture.walker = Some(Walker::new(
                    confinement,
                    &resolved,
                    compiled,
                    links,
                    None,
                    source,
                )?);
            }
            FileKind::File => {
                let file = resolved
                    .parent
                    .open_read(&resolved.name)
                    .map_err(|error| io_error(error, source))?;
                capture.start_hashing(
                    TreePath::root(),
                    Source {
                        canonical: resolved.segments,
                        stat,
                    },
                    file,
                )?;
            }
            FileKind::Symlink | FileKind::Other => {
                return Err(ProtocolError::failed_precondition(
                    PreconditionReason::NotRegularFile,
                    format!("{source} is not a regular file or directory"),
                ));
            }
        }
        Ok(capture)
    }

    /// One bounded step; `true` once the inventory is sealed.
    pub fn advance(&mut self) -> Result<bool, ProtocolError> {
        let mut allowance = STEP_BYTES;
        for _ in 0..STEP_ENTRIES {
            if self.hashing.is_some() {
                if !self.hash_step(&mut allowance)? {
                    return Ok(false);
                }
                continue;
            }
            let Some(walker) = self.walker.as_mut() else {
                return Ok(true);
            };
            let Some(event) = walker.next_event() else {
                self.walker = None;
                return Ok(true);
            };
            self.visit(event)?;
        }
        Ok(self.walker.is_none() && self.hashing.is_none())
    }

    /// Selection comes first: an entry or skip the filter does not select
    /// is left out silently, before any policy, depth or readability check
    /// could fail the capture. Directories are always walked.
    fn visit(&mut self, event: Event) -> Result<(), ProtocolError> {
        let entry = match event {
            Event::Yield
            | Event::Skip {
                selected: false, ..
            } => return Ok(()),
            Event::Skip {
                path,
                reason: Skip::NonUtf8Name,
                ..
            } => {
                self.skip(path, TransferSkipReason::NonUtf8Name);
                return Ok(());
            }
            Event::Skip {
                path,
                reason: Skip::DanglingLink,
                ..
            } => {
                self.skip(path, TransferSkipReason::Symlink);
                return Ok(());
            }
            Event::Skip {
                reason: Skip::Unreadable(error),
                ..
            } => return Err(error),
            Event::Skip {
                path,
                reason: Skip::LinkOutsideRoot(error),
                ..
            } => {
                return Err(symlink_rejected(
                    path.as_ref().map_or(".", TreePath::as_str),
                    &error.message,
                ));
            }
            Event::Entry(entry) => entry,
        };
        if !entry.selected && !entry.stat.is_dir() {
            return Ok(());
        }
        if entry.via_link && !entry.canonical.starts_with(&self.selection) {
            return Err(symlink_rejected(
                entry.path.as_str(),
                "the link leaves the captured selection",
            ));
        }
        if entry.depth > self.limits.max_depth {
            return Err(ProtocolError::invalid_field(
                "limits.maxDepth",
                format!("{} is deeper than limits.maxDepth", entry.path),
            ));
        }
        match entry.stat.kind {
            FileKind::Directory => self.record(InventoryEntry {
                path: entry.path,
                content: TreeContent::Directory {},
            }),
            FileKind::Symlink => match self.symlinks {
                SymlinkPolicy::Skip => {
                    self.skip(Some(entry.path), TransferSkipReason::Symlink);
                    Ok(())
                }
                SymlinkPolicy::Reject | SymlinkPolicy::ExpandInside => Err(symlink_rejected(
                    entry.path.as_str(),
                    "the capture refuses symlinks",
                )),
            },
            FileKind::Other => match self.special_files {
                SpecialFilePolicy::Skip => {
                    self.skip(Some(entry.path), TransferSkipReason::SpecialFile);
                    Ok(())
                }
                SpecialFilePolicy::Reject => Err(ProtocolError::failed_precondition(
                    PreconditionReason::NotRegularFile,
                    format!("{} is a special file", entry.path),
                )),
            },
            FileKind::File => {
                let file = entry
                    .parent
                    .open_read(&entry.name)
                    .map_err(|error| io_error(error, &entry.path))?;
                self.start_hashing(
                    entry.path,
                    Source {
                        canonical: entry.canonical,
                        stat: entry.stat,
                    },
                    file,
                )
            }
        }
    }

    fn start_hashing(
        &mut self,
        path: TreePath,
        source: Source,
        file: File,
    ) -> Result<(), ProtocolError> {
        let opened = EntryStat::of_file(&file).map_err(|error| io_error(error, &path))?;
        if !opened.is_file() || !opened.unchanged(&source.stat) {
            return Err(source_changed(&path));
        }
        if source.stat.size > self.limits.max_file_bytes {
            return Err(ProtocolError::payload_too_large(
                self.limits.max_file_bytes,
                format!("{path} is larger than limits.maxFileBytes"),
            ));
        }
        self.total_bytes = self.total_bytes.saturating_add(source.stat.size);
        if self.total_bytes > self.limits.max_total_bytes {
            return Err(ProtocolError::payload_too_large(
                self.limits.max_total_bytes,
                "the capture exceeds limits.maxTotalBytes",
            ));
        }
        self.hashing = Some(Hashing {
            path,
            source,
            file,
            hash: Sha256::new(),
            read: 0,
        });
        Ok(())
    }

    /// Hash within `allowance`; `true` when the file is done.
    fn hash_step(&mut self, allowance: &mut u64) -> Result<bool, ProtocolError> {
        let task = self.hashing.as_mut().expect("caller checked");
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            if *allowance == 0 {
                return Ok(false);
            }
            let limit = (buffer.len() as u64).min(*allowance) as usize;
            let read = task
                .file
                .read(&mut buffer[..limit])
                .map_err(|error| io_error(error, &task.path))?;
            if read == 0 {
                break;
            }
            task.read += read as u64;
            if task.read > task.source.stat.size {
                return Err(source_changed(&task.path));
            }
            task.hash.update(&buffer[..read]);
            *allowance -= read as u64;
        }
        let task = self.hashing.take().expect("hashing above");
        let now = EntryStat::of_file(&task.file).map_err(|error| io_error(error, &task.path))?;
        if task.read != task.source.stat.size || !now.unchanged(&task.source.stat) {
            return Err(source_changed(&task.path));
        }
        let digest = ContentDigest::from_sha256(task.hash.finalize().into());
        let executable = task.source.stat.executable();
        let size_bytes = task.source.stat.size;
        self.sources.entry(digest).or_insert(task.source);
        self.record(InventoryEntry {
            path: task.path,
            content: TreeContent::File {
                size_bytes,
                executable,
                digest: Some(digest),
            },
        })?;
        Ok(true)
    }

    fn record(&mut self, entry: InventoryEntry) -> Result<(), ProtocolError> {
        check_entry_quota(self.entries.len() + 1, &self.limits)?;
        self.manifest_bytes += entry_bytes(&entry);
        if self.manifest_bytes > self.limits.max_manifest_bytes {
            return Err(ProtocolError::payload_too_large(
                self.limits.max_manifest_bytes,
                "the inventory exceeds limits.maxManifestBytes",
            ));
        }
        self.entries.push(entry);
        Ok(())
    }

    fn skip(&mut self, path: Option<TreePath>, reason: TransferSkipReason) {
        self.skipped_count = self.skipped_count.saturating_add(1);
        if (self.skipped.len() as u32) < MAX_LISTED_SKIPS {
            self.skipped.push(TransferSkip { path, reason });
        }
    }
}

/// Why reading captured content failed.
pub(super) enum ReadFailure {
    /// The bytes are not the bytes that were hashed.
    Changed(ProtocolError),
    Io(ProtocolError),
}

/// Read `max` bytes of a captured source from `offset`: `(data, truncated)`.
pub(super) fn read_content(
    confinement: &Confinement,
    source: &Source,
    offset: u64,
    max: u64,
) -> Result<(Vec<u8>, bool), ReadFailure> {
    let subject = format!("captured {}", source_label(source));
    let resolved = confinement
        .reopen(&source.canonical, &subject)
        .map_err(|_| ReadFailure::Changed(source_changed(&subject)))?;
    let mut file = resolved
        .parent
        .open_read(&resolved.name)
        .map_err(|error| ReadFailure::Io(io_error(error, &subject)))?;
    let unchanged = |file: &File| {
        EntryStat::of_file(file).is_ok_and(|stat| stat.is_file() && stat.unchanged(&source.stat))
    };
    if !unchanged(&file) {
        return Err(ReadFailure::Changed(source_changed(&subject)));
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| ReadFailure::Io(io_error(error, &subject)))?;
    let take = max.min(source.stat.size - offset);
    let data = os::read_up_to(&mut file, take)
        .map_err(|error| ReadFailure::Io(io_error(error, &subject)))?;
    if data.len() as u64 != take || !unchanged(&file) {
        return Err(ReadFailure::Changed(source_changed(&subject)));
    }
    let truncated = offset + take < source.stat.size;
    Ok((data, truncated))
}

fn source_label(source: &Source) -> String {
    source
        .canonical
        .last()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".into())
}

pub(super) fn source_changed(subject: &dyn std::fmt::Display) -> ProtocolError {
    ProtocolError::aborted(
        Some(AbortReason::SourceChanged),
        format!("{subject} changed during the transfer"),
    )
}

fn symlink_rejected(path: &str, why: &str) -> ProtocolError {
    ProtocolError::failed_precondition(
        PreconditionReason::SymlinkRejected,
        format!("{path}: {why}"),
    )
}
