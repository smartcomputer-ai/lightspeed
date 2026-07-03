//! Logical storage contracts for agent runners.
//!
//! These traits describe what the agent runtime needs without choosing a
//! production backend. Local runners can use the in-memory implementations,
//! while Postgres/Temporal runners adapt these contracts to their own
//! durability model.

pub mod blobs;
pub mod session;

pub use crate::session::{
    SessionEntry, StoredSessionEntry, UncommittedSessionEvent, UncommittedStoredEvent,
};
pub use blobs::{
    BlobCacheLimits, BlobCacheStats, BlobEdge, BlobGraphStore, BlobInfo, BlobStore, BlobStoreError,
    CachedBlobStore, InMemoryBlobCache, InMemoryBlobStore, SessionBlobRoot, ensure_engine_blobs,
};
pub use session::{
    AppendSessionEvents, AppendSessionEventsResult, CreateClonedSession, CreateForkedSession,
    CreateSession, InMemorySessionStore, ListSessionLinks, ReadSessionEvents, SessionLinkDirection,
    SessionLinkRecord, SessionPage, SessionRecord, SessionStore, SessionStoreError,
    UpsertSessionLink, largest_safe_fork_seq, validate_fork_point, validate_relationship,
};
