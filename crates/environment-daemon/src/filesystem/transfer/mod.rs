//! `transfer/*`: one logical capture, materialize or file write as many
//! bounded calls.
//!
//! Each operation has its own lock; uploads and captured reads of
//! different digests proceed concurrently. Only active operations are held
//! in memory and count against `maxActiveOperations`: a finished operation
//! (complete, aborted or failed) is journaled as its receipt, evicted, and
//! answered from the journal for `receiptRetentionMs` after it finished.
//! A completed capture's sealed inventory is retained with its receipt and
//! stays pageable, so a caller retrying a capture whose answer it lost
//! gets the tree it first captured without revisiting the live source.
//!
//! Errors of the operation's own work (walking, hashing, staging,
//! publishing, a source that changed, a digest mismatch, the duration
//! running out) move it to `failed`; every later call except
//! `transfer/getStatus` and `transfer/abort` then answers that error. A
//! request the daemon refuses without acting on it (invalid params, a call
//! in the wrong phase, an unexpected offset, an unknown digest) leaves the
//! operation as it was.

mod capture;
mod journal;
mod publish;
#[cfg(test)]
mod tests;

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex, MutexGuard, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use environment_protocol::{
    bytes::ByteChunk,
    data::{
        handshake::TransferCeilings,
        transfer::{
            AppendTransferInventoryParams, BeginTransferParams, InventoryEntry,
            ListTransferMissingParams, ListTransferMissingResponse, ReadTransferContentParams,
            ReadTransferContentResponse, ReadTransferInventoryParams,
            ReadTransferInventoryResponse, TransferLimits, TransferOperationParams, TransferPhase,
            TransferSelection, TransferStatus, WriteTransferContentParams,
            WriteTransferContentResponse,
        },
    },
    error::{BusyReason, Capability, NotFoundReason, PreconditionReason, ProtocolError},
    ids::TransferOperationId,
};

use super::{confinement::Confinement, io_error, search::check_quota};
use capture::{Capture, ReadFailure};
use journal::Record;
use publish::{ChunkFailure, Kind, Publish};

/// Content hashed or copied by one `transfer/advance`.
const STEP_BYTES: u64 = 4 * 1024 * 1024;
/// Entries visited by one `transfer/advance`.
const STEP_ENTRIES: usize = 1024;
/// How often finished receipts and overdue operations are swept.
const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

fn check_entry_quota(count: usize, limits: &TransferLimits) -> Result<(), ProtocolError> {
    if count as u64 > u64::from(limits.max_entries) {
        return Err(ProtocolError::too_many_entries(
            limits.max_entries,
            "the inventory exceeds limits.maxEntries",
        ));
    }
    Ok(())
}

fn entry_bytes(entry: &InventoryEntry) -> u64 {
    serde_json::to_vec(entry).map_or(0, |encoded| encoded.len() as u64)
}

/// The daemon's transfer operations.
#[derive(Clone)]
pub(crate) struct Transfers {
    shared: Arc<Shared>,
}

struct Shared {
    confinement: Arc<Confinement>,
    journal: PathBuf,
    ceilings: TransferCeilings,
    writable: bool,
    active: Mutex<BTreeMap<TransferOperationId, Arc<Operation>>>,
}

struct Operation {
    id: TransferOperationId,
    params: BeginTransferParams,
    /// Canonical destination of a publish, for overlap checks.
    destination: Option<Vec<std::ffi::OsString>>,
    /// Set (under `state`) once the operation finished; read without it.
    finished: AtomicBool,
    state: Mutex<State>,
}

struct State {
    status: TransferStatus,
    deadline_ms: i64,
    work: Work,
    /// The receipt of a finished operation whose journal write failed. The
    /// operation stays in memory, answering from here, until a retry
    /// persists it.
    unpersisted: Option<Record>,
}

enum Work {
    Capture(Box<Capture>),
    Publish(Box<Publish>),
    Finished,
}

/// How a finished operation answers each call.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Call {
    Begin,
    Abort,
    Commit,
    ReadInventory,
    Other,
}

enum Found {
    Active(Arc<Operation>),
    Finished(TransferStatus),
}

impl Transfers {
    /// Journal under `<state_dir>/transfers`. Operations a restart
    /// interrupted are recorded as failed (`aborted`) and their staging is
    /// discarded.
    pub fn new(
        confinement: Arc<Confinement>,
        ceilings: TransferCeilings,
        writable: bool,
    ) -> Result<Self, ProtocolError> {
        let journal = confinement.state_dir().join("transfers");
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(super::os::PRIVATE_DIRECTORY_MODE)
                .create(&journal)
                .map_err(|error| io_error(error, journal.display()))?;
        }
        let shared = Arc::new(Shared {
            confinement,
            journal,
            ceilings,
            writable,
            active: Mutex::new(BTreeMap::new()),
        });
        shared.recover();
        let weak = Arc::downgrade(&shared);
        std::thread::Builder::new()
            .name("transfer-sweep".into())
            .spawn(move || sweep_loop(weak))
            .map_err(|error| ProtocolError::internal(format!("start transfer sweep: {error}")))?;
        Ok(Self { shared })
    }

    pub fn begin(&self, params: BeginTransferParams) -> Result<TransferStatus, ProtocolError> {
        self.shared.begin(params)
    }

    pub fn advance(
        &self,
        params: TransferOperationParams,
    ) -> Result<TransferStatus, ProtocolError> {
        self.shared.advance(&params.operation_id)
    }

    pub fn get_status(
        &self,
        params: TransferOperationParams,
    ) -> Result<TransferStatus, ProtocolError> {
        match self.shared.find(&params.operation_id)? {
            Found::Finished(status) => Ok(status),
            Found::Active(operation) => Ok(operation.lock().status.clone()),
        }
    }

    pub fn read_inventory(
        &self,
        params: ReadTransferInventoryParams,
    ) -> Result<ReadTransferInventoryResponse, ProtocolError> {
        self.shared.read_inventory(params)
    }

    pub fn append(
        &self,
        params: AppendTransferInventoryParams,
    ) -> Result<TransferStatus, ProtocolError> {
        self.shared.append(params)
    }

    pub fn list_missing(
        &self,
        params: ListTransferMissingParams,
    ) -> Result<ListTransferMissingResponse, ProtocolError> {
        self.shared.list_missing(params)
    }

    pub fn read(
        &self,
        params: ReadTransferContentParams,
    ) -> Result<ReadTransferContentResponse, ProtocolError> {
        self.shared.read(params)
    }

    pub fn write(
        &self,
        params: WriteTransferContentParams,
    ) -> Result<WriteTransferContentResponse, ProtocolError> {
        self.shared.write(params)
    }

    pub fn commit(&self, params: TransferOperationParams) -> Result<TransferStatus, ProtocolError> {
        self.shared.commit(&params.operation_id)
    }

    pub fn abort(&self, params: TransferOperationParams) -> Result<TransferStatus, ProtocolError> {
        self.shared.abort(&params.operation_id)
    }
}

impl Operation {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Shared {
    fn retention_ms(&self) -> i64 {
        i64::try_from(self.ceilings.receipt_retention_ms).unwrap_or(i64::MAX)
    }

    fn find(&self, id: &TransferOperationId) -> Result<Found, ProtocolError> {
        if let Some(operation) = self.active_map().get(id).cloned() {
            return Ok(Found::Active(operation));
        }
        let Some(record) = journal::read(&self.journal, id)? else {
            return Err(ProtocolError::not_found(format!("unknown transfer {id}")));
        };
        if !record.status.phase.is_finished() {
            // Only a record left by an earlier daemon instance can be
            // active without being in memory; recovery normally converts
            // it already.
            return Ok(Found::Finished(self.interrupted(id, record)));
        }
        if record
            .status
            .receipt_expires_at_ms
            .is_some_and(|expires| expires <= journal::now_ms())
        {
            return Err(ProtocolError::not_found_because(
                NotFoundReason::Expired,
                format!("the receipt of transfer {id} expired"),
            ));
        }
        Ok(Found::Finished(record.status))
    }

    fn active_map(&self) -> MutexGuard<'_, BTreeMap<TransferOperationId, Arc<Operation>>> {
        self.active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Drop a finished operation from memory once its receipt is on disk;
    /// retries a receipt write that failed.
    fn evict(&self, operation: &Arc<Operation>) {
        if !operation.finished.load(Ordering::Acquire) {
            return;
        }
        {
            let mut state = operation.lock();
            if let Some(record) = &state.unpersisted {
                if let Err(error) = journal::write(&self.journal, &operation.id, record) {
                    eprintln!(
                        "lightspeed-envd: journaling the receipt of transfer {}: {error}",
                        operation.id
                    );
                    return;
                }
                state.unpersisted = None;
            }
        }
        let mut active = self.active_map();
        if active
            .get(&operation.id)
            .is_some_and(|current| Arc::ptr_eq(current, operation))
        {
            active.remove(&operation.id);
        }
    }

    /// Run `body` on an active operation, answering finished ones per
    /// `call`, failing an overdue one, and evicting it once finished.
    fn with_active<T>(
        &self,
        id: &TransferOperationId,
        call: Call,
        body: impl FnOnce(&Self, &Arc<Operation>, &mut State) -> Result<T, ProtocolError>,
        finished: impl FnOnce(TransferStatus) -> Result<T, ProtocolError>,
    ) -> Result<T, ProtocolError> {
        let operation = match self.find(id)? {
            Found::Finished(status) => return answer_finished(status, call).and_then(finished),
            Found::Active(operation) => operation,
        };
        let result = {
            let mut state = operation.lock();
            if state.status.phase.is_finished() {
                answer_finished(state.status.clone(), call).and_then(finished)
            } else if call != Call::Abort && journal::now_ms() >= state.deadline_ms {
                let error = ProtocolError::deadline_exceeded(format!(
                    "transfer {id} ran past its maxDurationMs"
                ));
                Err(self.fail(&operation, &mut state, error))
            } else {
                body(self, &operation, &mut state)
            }
        };
        self.evict(&operation);
        result
    }

    /// Record `phase` (complete, aborted, failed), release the work and
    /// journal the receipt.
    fn finish(
        &self,
        operation: &Operation,
        state: &mut State,
        phase: TransferPhase,
        error: Option<ProtocolError>,
    ) {
        state.status.phase = phase;
        state.status.error = error;
        state.status.deadline_at_ms = None;
        state.status.receipt_expires_at_ms =
            Some(journal::now_ms().saturating_add(self.retention_ms()));
        // The staging directory and spool are removed in the background;
        // the receipt names them until a sweep confirms they are gone.
        let (stage, spool) = match &state.work {
            Work::Publish(publish) => (publish.created_stage_record(), publish.spool_path()),
            Work::Capture(_) | Work::Finished => (None, None),
        };
        state.work = Work::Finished;
        let record = Record {
            params: operation.params.clone(),
            status: state.status.clone(),
            stage,
            spool,
            committing: false,
        };
        if let Err(error) = journal::write(&self.journal, &operation.id, &record) {
            eprintln!(
                "lightspeed-envd: journaling the receipt of transfer {}: {error}",
                operation.id
            );
            state.unpersisted = Some(record);
        }
        operation.finished.store(true, Ordering::Release);
    }

    fn fail(
        &self,
        operation: &Operation,
        state: &mut State,
        error: ProtocolError,
    ) -> ProtocolError {
        self.finish(operation, state, TransferPhase::Failed, Some(error.clone()));
        error
    }

    fn begin(&self, params: BeginTransferParams) -> Result<TransferStatus, ProtocolError> {
        self.check_limits(&params.limits)?;
        let publishes = !matches!(params.selection, TransferSelection::Capture { .. });
        if publishes && !self.writable {
            return Err(ProtocolError::capability_unavailable(
                Capability::FilesystemWrite,
                "the filesystem is read-only",
            ));
        }
        let id = params.operation_id.clone();
        if let Some(existing) = self.existing(&params)? {
            return Ok(existing);
        }
        // Checked again at insert; this avoids staging work that would be
        // refused anyway.
        self.check_capacity(&self.active_map())?;
        let (mut work, destination) = self.start(&params)?;
        let now = journal::now_ms();
        let deadline_ms =
            now.saturating_add(i64::try_from(params.limits.max_duration_ms).unwrap_or(i64::MAX));
        let (phase, stage, spool) = match &work {
            Work::Capture(_) => (TransferPhase::Scanning, None, None),
            Work::Publish(publish) => (
                TransferPhase::Inventory,
                Some(publish.stage_record()),
                publish.spool_path(),
            ),
            Work::Finished => unreachable!("start returns work"),
        };
        let status = TransferStatus {
            operation_id: id.clone(),
            phase,
            entry_count: 0,
            total_bytes: 0,
            transferred_bytes: 0,
            reused_bytes: 0,
            skipped: Vec::new(),
            skipped_entry_count: 0,
            error: None,
            deadline_at_ms: Some(deadline_ms),
            receipt_expires_at_ms: None,
        };
        let mut active = self.active_map();
        if active.contains_key(&id) {
            drop(active);
            discard(work);
            return self.existing(&params)?.ok_or_else(|| {
                ProtocolError::internal(format!("transfer {id} vanished during begin"))
            });
        }
        let admitted = self.check_capacity(&active).and_then(|()| {
            let overlaps = destination.as_ref().is_some_and(|destination| {
                active.values().any(|other| {
                    !other.finished.load(Ordering::Acquire)
                        && other.destination.as_ref().is_some_and(|other| {
                            other.starts_with(destination) || destination.starts_with(other)
                        })
                })
            });
            if overlaps {
                return Err(ProtocolError::busy(
                    Some(BusyReason::TransferCapacity),
                    None,
                    "the destination overlaps an active transfer",
                ));
            }
            Ok(())
        });
        // The record names the staging directory before it exists, so an
        // exit at any later point leaves nothing a restart cannot find.
        let recorded = admitted.and_then(|()| {
            journal::write(
                &self.journal,
                &id,
                &Record {
                    params: params.clone(),
                    status: status.clone(),
                    stage,
                    spool,
                    committing: false,
                },
            )
        });
        if let Err(error) = recorded {
            drop(active);
            discard(work);
            return Err(error);
        }
        if let Work::Publish(publish) = &mut work
            && let Err(error) = publish.create_stage()
        {
            journal::remove(&self.journal, &id);
            drop(active);
            discard(work);
            return Err(error);
        }
        active.insert(
            id.clone(),
            Arc::new(Operation {
                id,
                params,
                destination,
                finished: AtomicBool::new(false),
                state: Mutex::new(State {
                    status: status.clone(),
                    deadline_ms,
                    work,
                    unpersisted: None,
                }),
            }),
        );
        Ok(status)
    }

    fn check_capacity(
        &self,
        active: &BTreeMap<TransferOperationId, Arc<Operation>>,
    ) -> Result<(), ProtocolError> {
        let running = active
            .values()
            .filter(|operation| !operation.finished.load(Ordering::Acquire))
            .count();
        if running >= self.ceilings.max_active_operations as usize {
            return Err(ProtocolError::busy(
                Some(BusyReason::TransferCapacity),
                None,
                format!(
                    "{running} transfers are active; the ceiling is {}",
                    self.ceilings.max_active_operations
                ),
            ));
        }
        Ok(())
    }

    /// The answer to a repeated begin, or `None` for a new id.
    fn existing(
        &self,
        params: &BeginTransferParams,
    ) -> Result<Option<TransferStatus>, ProtocolError> {
        let id = &params.operation_id;
        let in_use = || {
            ProtocolError::failed_precondition(
                PreconditionReason::OperationIdInUse,
                format!("transfer {id} was begun with different input"),
            )
        };
        let active = self.active_map().get(id).cloned();
        if let Some(operation) = active {
            if operation.params != *params {
                return Err(in_use());
            }
            let status = operation.lock().status.clone();
            return answer_finished_or_active(status, Call::Begin).map(Some);
        }
        match journal::read(&self.journal, id)? {
            None => Ok(None),
            Some(record) if record.params != *params => Err(in_use()),
            Some(_) => match self.find(id)? {
                Found::Finished(status) => answer_finished(status, Call::Begin).map(Some),
                Found::Active(operation) => {
                    let status = operation.lock().status.clone();
                    answer_finished_or_active(status, Call::Begin).map(Some)
                }
            },
        }
    }

    fn check_limits(&self, limits: &TransferLimits) -> Result<(), ProtocolError> {
        let ceilings = &self.ceilings;
        check_quota(
            "limits.maxEntries",
            limits.max_entries.into(),
            ceilings.max_entries.into(),
        )?;
        let above = |field: &str, value: u64, ceiling: u64| {
            if value > ceiling {
                Err(ProtocolError::above_ceiling(field, value, ceiling))
            } else {
                Ok(())
            }
        };
        above(
            "limits.maxDepth",
            limits.max_depth.into(),
            ceilings.max_depth.into(),
        )?;
        above(
            "limits.maxFileBytes",
            limits.max_file_bytes,
            ceilings.max_file_bytes,
        )?;
        above(
            "limits.maxTotalBytes",
            limits.max_total_bytes,
            ceilings.max_total_bytes,
        )?;
        check_quota(
            "limits.maxManifestBytes",
            limits.max_manifest_bytes,
            ceilings.max_manifest_bytes,
        )?;
        check_quota(
            "limits.maxDurationMs",
            limits.max_duration_ms,
            ceilings.max_duration_ms,
        )
    }

    fn start(
        &self,
        params: &BeginTransferParams,
    ) -> Result<(Work, Option<Vec<std::ffi::OsString>>), ProtocolError> {
        let confinement = self.confinement.clone();
        let limits = params.limits;
        let (destination, kind) = match &params.selection {
            TransferSelection::Capture {
                source,
                filter,
                symlinks,
                special_files,
            } => {
                let capture = Capture::begin(
                    confinement,
                    source,
                    filter,
                    *symlinks,
                    *special_files,
                    limits,
                )?;
                return Ok((Work::Capture(Box::new(capture)), None));
            }
            TransferSelection::Materialize {
                destination,
                on_existing,
            } => (
                destination,
                Kind::Materialize {
                    on_existing: *on_existing,
                },
            ),
            TransferSelection::WriteFile {
                destination,
                create,
                if_match,
                executable,
            } => (
                destination,
                Kind::WriteFile {
                    create: *create,
                    if_match: if_match.clone(),
                    executable: *executable,
                },
            ),
        };
        let spool = tempfile::Builder::new()
            .prefix(&format!("{}.spool-", params.operation_id))
            .tempdir_in(&self.journal)
            .map_err(|error| io_error(error, self.journal.display()))?;
        let publish = Publish::begin(confinement, destination, kind, limits, spool)?;
        let segments = publish.destination_segments.clone();
        Ok((Work::Publish(Box::new(publish)), Some(segments)))
    }

    fn advance(&self, id: &TransferOperationId) -> Result<TransferStatus, ProtocolError> {
        self.with_active(
            id,
            Call::Other,
            |shared, operation, state| {
                let step = match &mut state.work {
                    Work::Capture(capture) => advance_capture(capture, &mut state.status),
                    Work::Publish(publish) => advance_publish(publish, &mut state.status),
                    Work::Finished => Ok(()),
                };
                match step {
                    Ok(()) => Ok(state.status.clone()),
                    Err(error) => Err(shared.fail(operation, state, error)),
                }
            },
            Ok,
        )
    }

    fn read_inventory(
        &self,
        params: ReadTransferInventoryParams,
    ) -> Result<ReadTransferInventoryResponse, ProtocolError> {
        check_quota(
            "limit",
            params.limit.into(),
            self.ceilings.max_inventory_page_entries.into(),
        )?;
        let retained = |status: TransferStatus| {
            let index = params.index as usize;
            let page = journal::read_inventory(
                &self.journal,
                &params.operation_id,
                index,
                params.limit as usize,
            )?;
            let Some((entries, more)) = page else {
                return Err(phase_error("the transfer is finished"));
            };
            if entries.is_empty() && index > status.entry_count as usize {
                return Err(ProtocolError::invalid_field(
                    "index",
                    format!("the inventory has {} entries", status.entry_count),
                ));
            }
            let next_index = more.then_some(params.index + entries.len() as u32);
            Ok(ReadTransferInventoryResponse {
                entries,
                next_index,
            })
        };
        self.with_active(
            &params.operation_id,
            Call::ReadInventory,
            |_, _, state| {
                let Work::Capture(capture) = &state.work else {
                    return Err(phase_error("only a capture has an inventory to read"));
                };
                if state.status.phase != TransferPhase::Inventory {
                    return Err(phase_error("the capture's inventory is not sealed yet"));
                }
                let count = capture.entries.len();
                let start = params.index as usize;
                if start > count {
                    return Err(ProtocolError::invalid_field(
                        "index",
                        format!("the inventory has {count} entries"),
                    ));
                }
                let end = (start + params.limit as usize).min(count);
                Ok(ReadTransferInventoryResponse {
                    entries: capture.entries[start..end].to_vec(),
                    next_index: (end < count).then_some(end as u32),
                })
            },
            retained,
        )
    }

    fn append(
        &self,
        params: AppendTransferInventoryParams,
    ) -> Result<TransferStatus, ProtocolError> {
        let page = self.ceilings.max_inventory_page_entries;
        if params.entries.len() as u64 > u64::from(page) {
            return Err(ProtocolError::above_ceiling(
                "entries",
                params.entries.len() as u64,
                page.into(),
            ));
        }
        self.with_active(
            &params.operation_id,
            Call::Other,
            |shared, operation, state| {
                let Work::Publish(publish) = &mut state.work else {
                    return Err(phase_error("a capture takes no inventory"));
                };
                // A retried page, the sealing one included, answers the
                // current status in any phase.
                if publish.is_repeat(params.index, &params.entries, params.last) {
                    return Ok(state.status.clone());
                }
                if state.status.phase != TransferPhase::Inventory {
                    return Err(phase_error("the inventory is already sealed"));
                }
                let sealed = publish.append(params.index, params.entries, params.last)?;
                state.status.entry_count = publish.entries.len() as u32;
                state.status.total_bytes = publish.total_bytes;
                if sealed {
                    if let Err(error) = publish.start_scan() {
                        return Err(shared.fail(operation, state, error));
                    }
                    state.status.phase = TransferPhase::Scanning;
                }
                Ok(state.status.clone())
            },
            |_| Err(phase_error("the transfer is finished")),
        )
    }

    fn list_missing(
        &self,
        params: ListTransferMissingParams,
    ) -> Result<ListTransferMissingResponse, ProtocolError> {
        check_quota(
            "limit",
            params.limit.into(),
            self.ceilings.max_inventory_page_entries.into(),
        )?;
        self.with_active(
            &params.operation_id,
            Call::Other,
            |_, _, state| {
                let Work::Publish(publish) = &state.work else {
                    return Err(phase_error("a capture needs no content"));
                };
                if !matches!(
                    state.status.phase,
                    TransferPhase::Content | TransferPhase::Staging | TransferPhase::Ready
                ) {
                    return Err(phase_error("the destination scan has not finished"));
                }
                let missing = publish.missing_digests();
                let limit = params.limit as usize;
                Ok(ListTransferMissingResponse {
                    remaining_count: missing.len().saturating_sub(limit) as u32,
                    digests: missing.into_iter().take(limit).collect(),
                })
            },
            |_| Err(phase_error("the transfer is finished")),
        )
    }

    fn read(
        &self,
        params: ReadTransferContentParams,
    ) -> Result<ReadTransferContentResponse, ProtocolError> {
        check_quota(
            "maxBytes",
            params.max_bytes,
            self.ceilings.max_content_chunk_bytes,
        )?;
        let id = params.operation_id.clone();
        let (operation, source) = self.with_active(
            &id,
            Call::Other,
            |_, operation, state| {
                let Work::Capture(capture) = &state.work else {
                    return Err(phase_error("only a capture serves content"));
                };
                if state.status.phase != TransferPhase::Inventory {
                    return Err(phase_error("the capture's inventory is not sealed yet"));
                }
                let source = capture
                    .sources
                    .get(&params.digest)
                    .cloned()
                    .ok_or_else(|| {
                        ProtocolError::not_found(format!("{} is not in the capture", params.digest))
                    })?;
                if params.offset > source.stat.size {
                    return Err(ProtocolError::invalid_field(
                        "offset",
                        format!("the content has {} bytes", source.stat.size),
                    ));
                }
                Ok((operation.clone(), source))
            },
            |_| Err(phase_error("the transfer is finished")),
        )?;
        let read =
            capture::read_content(&self.confinement, &source, params.offset, params.max_bytes);
        let mut state = operation.lock();
        let result = match read {
            Ok((data, truncated)) => {
                if state.status.phase == TransferPhase::Inventory {
                    state.status.transferred_bytes += data.len() as u64;
                }
                Ok(ReadTransferContentResponse {
                    data: ByteChunk::from(data),
                    truncated,
                })
            }
            Err(ReadFailure::Changed(error) | ReadFailure::Io(error)) => {
                if state.status.phase.is_finished() {
                    Err(error)
                } else {
                    Err(self.fail(&operation, &mut state, error))
                }
            }
        };
        drop(state);
        self.evict(&operation);
        result
    }

    fn write(
        &self,
        params: WriteTransferContentParams,
    ) -> Result<WriteTransferContentResponse, ProtocolError> {
        let chunk = self.ceilings.max_content_chunk_bytes;
        if params.data.len() as u64 > chunk {
            return Err(ProtocolError::above_ceiling(
                "data",
                params.data.len() as u64,
                chunk,
            ));
        }
        let id = params.operation_id.clone();
        let (operation, upload) = self.with_active(
            &id,
            Call::Other,
            |shared, operation, state| {
                let Work::Publish(publish) = &mut state.work else {
                    return Err(phase_error("a capture takes no content"));
                };
                if state.status.phase != TransferPhase::Content {
                    return Err(phase_error("the operation is not collecting content"));
                }
                let upload = match publish.upload(&params.digest) {
                    Ok(Some(upload)) => upload,
                    Ok(None) => {
                        return Err(ProtocolError::not_found(format!(
                            "{} is not in the inventory",
                            params.digest
                        )));
                    }
                    Err(error) => return Err(shared.fail(operation, state, error)),
                };
                Ok((operation.clone(), upload))
            },
            |_| Err(phase_error("the transfer is finished")),
        )?;
        let written = {
            let mut upload = upload
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let before = upload.length();
            upload
                .write(&params.digest, params.offset, params.data.as_slice())
                .map(|next| (next, next.saturating_sub(before)))
        };
        let mut state = operation.lock();
        let result = match written {
            Ok((next_offset, added)) => {
                state.status.transferred_bytes += added;
                Ok(WriteTransferContentResponse { next_offset })
            }
            Err(ChunkFailure::Offset(error)) => Err(error),
            Err(ChunkFailure::Mismatch(error) | ChunkFailure::Io(error)) => {
                if state.status.phase.is_finished() {
                    Err(error)
                } else {
                    Err(self.fail(&operation, &mut state, error))
                }
            }
        };
        drop(state);
        self.evict(&operation);
        result
    }

    fn commit(&self, id: &TransferOperationId) -> Result<TransferStatus, ProtocolError> {
        self.with_active(
            id,
            Call::Commit,
            |shared, operation, state| {
                let outcome = match &mut state.work {
                    // The inventory is retained before the receipt that
                    // answers for it.
                    Work::Capture(capture) if state.status.phase == TransferPhase::Inventory => {
                        journal::write_inventory(&shared.journal, &operation.id, &capture.entries)
                    }
                    Work::Publish(publish) if state.status.phase == TransferPhase::Ready => {
                        // Recorded first: a restart during the commit must
                        // not claim that nothing was published.
                        journal::write(
                            &shared.journal,
                            &operation.id,
                            &Record {
                                params: operation.params.clone(),
                                status: state.status.clone(),
                                stage: publish.created_stage_record(),
                                spool: publish.spool_path(),
                                committing: true,
                            },
                        )?;
                        publish.commit()
                    }
                    _ => return Err(phase_error("the operation is not ready to commit")),
                };
                match outcome {
                    Ok(()) => {
                        shared.finish(operation, state, TransferPhase::Complete, None);
                        Ok(state.status.clone())
                    }
                    Err(error) => Err(shared.fail(operation, state, error)),
                }
            },
            Ok,
        )
    }

    fn abort(&self, id: &TransferOperationId) -> Result<TransferStatus, ProtocolError> {
        self.with_active(
            id,
            Call::Abort,
            |shared, operation, state| {
                shared.finish(operation, state, TransferPhase::Aborted, None);
                Ok(state.status.clone())
            },
            Ok,
        )
    }

    /// Convert operations left active by an earlier daemon instance and
    /// drop expired receipts.
    fn recover(&self) {
        for (id, record) in journal::all(&self.journal) {
            if !record.status.phase.is_finished() {
                self.interrupted(&id, record);
            } else {
                self.settle_receipt(&id, record, journal::now_ms());
            }
        }
        journal::remove_orphan_inventories(&self.journal);
        // Spools of operations without a record (a crash between creating
        // the spool and the first journal write).
        if let Ok(entries) = std::fs::read_dir(&self.journal) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if let Some((id, _)) = name.split_once(".spool-")
                    && let Ok(id) = TransferOperationId::new(id)
                    && journal::read(&self.journal, &id).ok().flatten().is_none()
                {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
    }

    /// Remove the staging directory and spool a record names.
    fn release(&self, record: &mut Record) {
        if let Some(stage) = record.stage.take() {
            publish::discard_stage(&self.confinement, &stage);
        }
        if let Some(spool) = record.spool.take()
            && spool.parent() == Some(self.journal.as_path())
        {
            let _ = std::fs::remove_dir_all(spool);
        }
    }

    /// A journaled receipt: removed once expired; otherwise its staging
    /// and spool, which the finishing instance removes in the background,
    /// are made sure of and dropped from the record.
    fn settle_receipt(&self, id: &TransferOperationId, mut record: Record, now: i64) {
        let expired = record
            .status
            .receipt_expires_at_ms
            .is_none_or(|expires| expires <= now);
        if expired {
            self.release(&mut record);
            journal::remove(&self.journal, id);
        } else if record.stage.is_some() || record.spool.is_some() {
            self.release(&mut record);
            if let Err(error) = journal::write(&self.journal, id, &record) {
                eprintln!("lightspeed-envd: settling the receipt of transfer {id}: {error}");
            }
        }
    }

    /// Record an operation a restart interrupted as failed (`aborted`) and
    /// discard its staging and spool.
    fn interrupted(&self, id: &TransferOperationId, mut record: Record) -> TransferStatus {
        self.release(&mut record);
        record.status.phase = TransferPhase::Failed;
        record.status.error = Some(ProtocolError::aborted(
            None,
            if record.committing {
                "the daemon restarted while the transfer was committing; the destination may \
                 hold the published tree"
            } else {
                "the daemon restarted while the transfer was active; nothing was committed"
            },
        ));
        record.committing = false;
        record.status.deadline_at_ms = None;
        record.status.receipt_expires_at_ms =
            Some(journal::now_ms().saturating_add(self.retention_ms()));
        if let Err(error) = journal::write(&self.journal, id, &record) {
            eprintln!("lightspeed-envd: recording interrupted transfer {id}: {error}");
        }
        record.status
    }

    /// Fail overdue operations and remove expired receipts.
    fn sweep(&self) {
        let operations: Vec<_> = self.active_map().values().cloned().collect();
        for operation in operations {
            {
                let mut state = operation.lock();
                if !state.status.phase.is_finished() && journal::now_ms() >= state.deadline_ms {
                    let error = ProtocolError::deadline_exceeded(format!(
                        "transfer {} ran past its maxDurationMs",
                        operation.id
                    ));
                    self.fail(&operation, &mut state, error);
                }
            }
            self.evict(&operation);
        }
        let now = journal::now_ms();
        // Receipts that finished at least a sweep ago: the background
        // removal of their staging had its time.
        let finished_before = now.saturating_sub(SWEEP_INTERVAL.as_millis() as i64);
        let retention = self.retention_ms();
        for (id, record) in journal::all(&self.journal) {
            let due = record.status.receipt_expires_at_ms.is_none_or(|expires| {
                expires <= now || expires.saturating_sub(retention) <= finished_before
            });
            if record.status.phase.is_finished() && due && !self.active_map().contains_key(&id) {
                self.settle_receipt(&id, record, now);
            }
        }
    }
}

fn sweep_loop(shared: Weak<Shared>) {
    loop {
        std::thread::sleep(SWEEP_INTERVAL);
        let Some(shared) = shared.upgrade() else {
            return;
        };
        shared.sweep();
    }
}

/// Undo a begin refused before its operation was recorded.
fn discard(work: Work) {
    if let Work::Publish(publish) = work {
        publish.discard();
    }
}

fn phase_error(message: &str) -> ProtocolError {
    ProtocolError::failed_precondition(PreconditionReason::TransferPhase, message)
}

/// A finished operation's answer: its status for abort, an identical begin
/// and (when complete) commit and inventory reads; its error when it failed; otherwise a phase
/// error. `transfer/getStatus` always answers the status.
fn answer_finished(status: TransferStatus, call: Call) -> Result<TransferStatus, ProtocolError> {
    match (call, status.phase) {
        (Call::Abort, _) => Ok(status),
        (_, TransferPhase::Failed) => Err(status
            .error
            .clone()
            .unwrap_or_else(|| ProtocolError::internal("the transfer failed"))),
        (Call::Begin, _) | (Call::Commit | Call::ReadInventory, TransferPhase::Complete) => {
            Ok(status)
        }
        _ => Err(phase_error("the transfer is finished")),
    }
}

fn answer_finished_or_active(
    status: TransferStatus,
    call: Call,
) -> Result<TransferStatus, ProtocolError> {
    if status.phase.is_finished() {
        answer_finished(status, call)
    } else {
        Ok(status)
    }
}

fn advance_capture(
    capture: &mut Capture,
    status: &mut TransferStatus,
) -> Result<(), ProtocolError> {
    if status.phase == TransferPhase::Scanning && capture.advance()? {
        status.phase = TransferPhase::Inventory;
    }
    status.entry_count = capture.entries.len() as u32;
    status.total_bytes = capture.total_bytes;
    status.skipped = capture.skipped.clone();
    status.skipped_entry_count = capture.skipped_count;
    Ok(())
}

fn advance_publish(
    publish: &mut Publish,
    status: &mut TransferStatus,
) -> Result<(), ProtocolError> {
    if status.phase == TransferPhase::Scanning && publish.scan_step()? {
        status.phase = TransferPhase::Content;
    }
    if status.phase == TransferPhase::Content && publish.missing_digests().is_empty() {
        status.phase = TransferPhase::Staging;
    }
    if status.phase == TransferPhase::Staging && publish.stage_step()? {
        status.phase = TransferPhase::Ready;
    }
    status.reused_bytes = publish.reused_bytes;
    Ok(())
}
