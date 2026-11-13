//! Logical storage contracts for agent runners.
//!
//! These traits describe what the agent runtime needs without choosing a
//! production backend. Local runners can use the in-memory implementations,
//! while Postgres/Temporal runners adapt these contracts to their own
//! durability model.

pub mod blob_roots;
pub mod blobs;
pub mod session;
pub mod summary;

pub use crate::session::{
    SessionEntry, StoredSessionEntry, UncommittedSessionEvent, UncommittedStoredEvent,
};
pub use blob_roots::{event_blob_roots, typed_blob_refs};
pub use blobs::{
    BlobEdge, BlobGraphStore, BlobInfo, BlobSource, BlobStore, BlobStoreError,
    ENGINE_BLOB_CONTENTS, InMemoryBlobStore, MAX_BLOB_RANGE_BYTES, collect_blob_refs,
    ensure_harness_blobs, harness_blob_refs, record_contains_edges,
};
pub use session::{
    AdvanceSessionCheckpoint, AppendSessionEvents, AppendSessionEventsResult, CreateClonedSession,
    CreateForkedSession, CreateSession, DeleteClosedSessions, DeleteClosedSessionsResult,
    InMemorySessionStore, ListSessions, ReadRunRecordEntries, ReadSessionEventRange,
    ReadSessionEvents, RetentionRootCursor, SessionActivity, SessionCheckpoint,
    SessionLifecycleStatus, SessionListCursor, SessionListPage, SessionOrigin, SessionOriginKind,
    SessionOriginLimit, SessionPage, SessionRecord, SessionStore, SessionStoreError,
    apply_lifecycle_projection, check_origin_limits, is_terminal_run_entry, largest_safe_fork_seq,
    largest_safe_fork_seq_from_state, lifecycle_at_fork, metadata_matches, validate_fork_point,
};
