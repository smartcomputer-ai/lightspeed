//! Durable transfer records under `<state>/transfers`.
//!
//! An operation is recorded at begin, before its staging directory exists,
//! so a restart can discard its staging directory and spool; when its commit
//! starts; and when it finishes, as its receipt. A receipt keeps the staging
//! and spool records until a sweep confirmed their removal. A completed
//! capture also keeps its sealed inventory beside its receipt, one entry
//! per line, so a retried capture can page it again. The
//! record is this daemon's own file: it is never read by another version,
//! and an undecodable record is renamed aside instead of failing startup.

use std::{
    ffi::OsString,
    fs::File,
    io::{BufRead, BufReader, BufWriter, Read, Write},
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
};

use environment_protocol::{
    data::transfer::{BeginTransferParams, InventoryEntry, TransferStatus},
    error::ProtocolError,
    ids::TransferOperationId,
};
use serde::{Deserialize, Serialize};

use crate::filesystem::io_error;

/// Largest record read back; records hold no inventories.
const MAX_RECORD_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Record {
    pub params: BeginTransferParams,
    pub status: TransferStatus,
    /// The staging directory of an active publish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<StageRecord>,
    /// The upload spool of an active publish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spool: Option<PathBuf>,
    /// A publish's commit started: after a restart, whether it published
    /// is unknown.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub committing: bool,
}

/// Where a staging directory lives: the canonical segments of its parent
/// below the filesystem root (raw bytes, names need not be UTF-8) and its
/// name.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct StageRecord {
    pub parent: Vec<Vec<u8>>,
    pub name: String,
}

impl StageRecord {
    pub fn new(parent: &[OsString], name: &str) -> Self {
        Self {
            parent: parent
                .iter()
                .map(|segment| segment.as_bytes().to_vec())
                .collect(),
            name: name.to_owned(),
        }
    }

    pub fn parent_segments(&self) -> Vec<OsString> {
        self.parent
            .iter()
            .map(|segment| OsString::from_vec(segment.clone()))
            .collect()
    }
}

pub(super) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

fn record_path(directory: &Path, id: &TransferOperationId) -> PathBuf {
    directory.join(format!("{}.json", id.as_str()))
}

const INVENTORY_EXTENSION: &str = "inventory";

fn inventory_path(directory: &Path, id: &TransferOperationId) -> PathBuf {
    directory.join(format!("{}.{INVENTORY_EXTENSION}", id.as_str()))
}

/// Write a completed capture's inventory atomically and durably, before
/// its receipt.
pub(super) fn write_inventory(
    directory: &Path,
    id: &TransferOperationId,
    entries: &[InventoryEntry],
) -> Result<(), ProtocolError> {
    let io = |error| io_error(error, format!("transfer journal {}", directory.display()));
    let temporary = tempfile::NamedTempFile::new_in(directory).map_err(io)?;
    let mut writer = BufWriter::new(temporary);
    for entry in entries {
        serde_json::to_writer(&mut writer, entry).map_err(|error| {
            ProtocolError::internal(format!("encode transfer inventory: {error}"))
        })?;
        writer.write_all(b"\n").map_err(io)?;
    }
    let temporary = writer
        .into_inner()
        .map_err(|error| io(error.into_error()))?;
    temporary.as_file().sync_all().map_err(io)?;
    temporary
        .persist(inventory_path(directory, id))
        .map_err(|error| io(error.error))?;
    File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(io)
}

/// Up to `limit` entries of a retained inventory from `index`, and whether
/// more follow; `Ok(None)` when none is retained.
pub(super) fn read_inventory(
    directory: &Path,
    id: &TransferOperationId,
    index: usize,
    limit: usize,
) -> Result<Option<(Vec<InventoryEntry>, bool)>, ProtocolError> {
    let path = inventory_path(directory, id);
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(error, path.display())),
    };
    let mut lines = BufReader::new(file).lines().skip(index);
    let mut entries = Vec::new();
    for line in lines.by_ref().take(limit) {
        let line = line.map_err(|error| io_error(error, path.display()))?;
        entries.push(serde_json::from_str(&line).map_err(|error| {
            ProtocolError::internal(format!("decode transfer inventory: {error}"))
        })?);
    }
    Ok(Some((entries, lines.next().is_some())))
}

/// Write a record atomically and durably.
pub(super) fn write(
    directory: &Path,
    id: &TransferOperationId,
    record: &Record,
) -> Result<(), ProtocolError> {
    let io = |error| io_error(error, format!("transfer journal {}", directory.display()));
    let mut temporary = tempfile::NamedTempFile::new_in(directory).map_err(io)?;
    serde_json::to_writer(&mut temporary, record)
        .map_err(|error| ProtocolError::internal(format!("encode transfer record: {error}")))?;
    temporary.as_file().sync_all().map_err(io)?;
    temporary
        .persist(record_path(directory, id))
        .map_err(|error| io(error.error))?;
    File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(io)
}

/// Read a record; `Ok(None)` when there is none or it was quarantined.
pub(super) fn read(
    directory: &Path,
    id: &TransferOperationId,
) -> Result<Option<Record>, ProtocolError> {
    read_path(&record_path(directory, id))
}

fn read_path(path: &Path) -> Result<Option<Record>, ProtocolError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(error, path.display())),
    };
    match serde_json::from_reader(file.take(MAX_RECORD_BYTES)) {
        Ok(record) => Ok(Some(record)),
        Err(error) => {
            quarantine(path, &error.to_string());
            Ok(None)
        }
    }
}

/// Remove a record and any inventory retained with it.
pub(super) fn remove(directory: &Path, id: &TransferOperationId) {
    let _ = std::fs::remove_file(record_path(directory, id));
    let _ = std::fs::remove_file(inventory_path(directory, id));
}

/// Retained inventories whose record is gone (a crash between writing the
/// inventory and its receipt, or between removing the two).
pub(super) fn remove_orphan_inventories(directory: &Path) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == INVENTORY_EXTENSION)
            && !path.with_extension("json").exists()
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn quarantine(path: &Path, reason: &str) {
    let aside = path.with_extension("quarantined");
    eprintln!(
        "lightspeed-envd: unreadable transfer record {} moved to {}: {reason}",
        path.display(),
        aside.display()
    );
    let _ = std::fs::rename(path, aside);
}

/// Every readable record with its id, in no particular order.
pub(super) fn all(directory: &Path) -> Vec<(TransferOperationId, Record)> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut records = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let Some(id) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| TransferOperationId::new(stem).ok())
        else {
            continue;
        };
        if let Ok(Some(record)) = read_path(&path) {
            records.push((id, record));
        }
    }
    records
}
