use engine::storage::{
    DeleteClosedSessions, DeleteClosedSessionsResult, SessionStore, SessionStoreError,
};
use store_pg::PgStore;

#[derive(Clone, Copy, Debug)]
pub(crate) enum SessionDeletionCause {
    Manual,
    Retention,
}

impl SessionDeletionCause {
    fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Retention => "retention",
        }
    }
}

/// Delete a closed session subtree. Environment lifecycles are independent.
pub(crate) async fn delete_session_subtree(
    store: &PgStore,
    request: DeleteClosedSessions,
    cause: SessionDeletionCause,
) -> Result<DeleteClosedSessionsResult, SessionStoreError> {
    let requested_session_id = request.session_id.clone();
    let cascade = request.cascade;
    let deleted = SessionStore::delete_closed_sessions(store, request).await?;
    tracing::info!(
        target: "temporal_server",
        requested_session_id = %requested_session_id,
        retention_root_session_id = %deleted.target.retention_root_session_id,
        deleted_session_count = deleted.deleted_session_ids.len(),
        cascade,
        cause = cause.as_str(),
        "session deletion complete"
    );
    Ok(deleted)
}
