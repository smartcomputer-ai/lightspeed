//! Operator-scoped (deployment-level) service: universe lifecycle over the
//! shared deployment resources, above the universe-bound store boundary.
//!
//! Purge ordering (`operator/universes/delete`): terminate live session
//! workflows, sweep externally stored blob objects, delete the `universes`
//! row (every universe-scoped table cascades from it), then evict the cached
//! runtime state. Each step is idempotent and the row is deleted last, so a
//! partial failure leaves the universe visible and a re-run resumes where it
//! stopped. Callers stop routing traffic to the universe before purging (the
//! platform archives first); a write racing the purge can lazily re-insert an
//! empty universe row via `ensure_universe`, which a re-run removes.

use std::sync::Arc;

use std::time::{Duration, Instant};

use api::{
    AgentApiError, AgentApiOutcome, OperatorApiService, OperatorOutboundMessageView,
    OperatorOutboxReadParams, OperatorOutboxReadResponse, OperatorUniverseCreateParams,
    OperatorUniverseCreateResponse, OperatorUniverseDeleteParams, OperatorUniverseDeleteResponse,
    OperatorUniverseListParams, OperatorUniverseListResponse, OperatorUniverseReadParams,
    OperatorUniverseReadResponse, OperatorUniverseView,
};
use async_trait::async_trait;
use engine::SessionId;
use object_store::ObjectStoreExt as _;
use object_store::path::Path as ObjectPath;
use temporal_workflow::{AgentSessionWorkflow, compose_workflow_id};
use temporalio_client::{Client, WorkflowTerminateOptions, errors::WorkflowInteractionError};
use uuid::Uuid;

use crate::universe::UniverseRuntime;

use super::service::{map_messaging_error, outbound_message_view};

/// Server-side cap for `operator/outbox/read` long-poll waits; requests
/// above the cap are clamped, matching the per-universe `outbox/read`.
const OUTBOX_WAIT_CAP: Duration = Duration::from_secs(30);
const OUTBOX_POLL_INTERVAL: Duration = Duration::from_millis(250);

pub struct GatewayOperatorApi {
    runtime: Arc<UniverseRuntime>,
}

impl GatewayOperatorApi {
    pub fn new(runtime: Arc<UniverseRuntime>) -> Self {
        Self { runtime }
    }

    fn pool(&self) -> &sqlx::PgPool {
        self.runtime.stores().pool()
    }

    fn temporal(&self) -> &Client {
        self.runtime.client()
    }

    async fn read_universe_view(
        &self,
        universe_id: Uuid,
    ) -> Result<Option<OperatorUniverseView>, AgentApiError> {
        let stats = store_pg::read_universe_stats(self.pool(), universe_id)
            .await
            .map_err(map_store_error)?;
        Ok(stats.map(universe_view))
    }

    /// Terminate the session's live workflow. `NotFound` covers both "never
    /// started" and "already closed" and counts as nothing-to-terminate; any
    /// other failure aborts the purge before rows are touched (a purge that
    /// leaves a live workflow writing into a half-deleted universe is worse
    /// than a retryable error).
    async fn terminate_session_workflow(
        &self,
        universe_id: Uuid,
        session_id: &str,
    ) -> Result<bool, AgentApiError> {
        let session_id = SessionId::try_new(session_id).map_err(|error| {
            AgentApiError::internal(format!("stored session id is invalid: {error}"))
        })?;
        let handle =
            self.temporal()
                .get_workflow_handle::<AgentSessionWorkflow>(compose_workflow_id(
                    universe_id,
                    &session_id,
                ));
        match handle
            .terminate(
                WorkflowTerminateOptions::builder()
                    .reason("operator universe purge")
                    .build(),
            )
            .await
        {
            Ok(()) => Ok(true),
            Err(WorkflowInteractionError::NotFound(_)) => Ok(false),
            Err(error) => Err(AgentApiError::internal(format!(
                "terminate session workflow {session_id}: {error}"
            ))),
        }
    }
}

#[async_trait]
impl OperatorApiService for GatewayOperatorApi {
    async fn create_universe(
        &self,
        params: OperatorUniverseCreateParams,
    ) -> Result<AgentApiOutcome<OperatorUniverseCreateResponse>, AgentApiError> {
        let universe_id = parse_universe_id(&params.universe_id)?;
        let created = store_pg::create_universe(self.pool(), universe_id)
            .await
            .map_err(map_store_error)?;
        let universe = self.read_universe_view(universe_id).await?.ok_or_else(|| {
            AgentApiError::internal(format!("universe disappeared after create: {universe_id}"))
        })?;
        Ok(AgentApiOutcome::new(OperatorUniverseCreateResponse {
            universe,
            created,
        }))
    }

    async fn list_universes(
        &self,
        _params: OperatorUniverseListParams,
    ) -> Result<AgentApiOutcome<OperatorUniverseListResponse>, AgentApiError> {
        let universes = store_pg::list_universe_stats(self.pool())
            .await
            .map_err(map_store_error)?;
        Ok(AgentApiOutcome::new(OperatorUniverseListResponse {
            universes: universes.into_iter().map(universe_view).collect(),
        }))
    }

    async fn read_universe(
        &self,
        params: OperatorUniverseReadParams,
    ) -> Result<AgentApiOutcome<OperatorUniverseReadResponse>, AgentApiError> {
        let universe_id = parse_universe_id(&params.universe_id)?;
        let universe = self
            .read_universe_view(universe_id)
            .await?
            .ok_or_else(|| AgentApiError::not_found(format!("unknown universe: {universe_id}")))?;
        Ok(AgentApiOutcome::new(OperatorUniverseReadResponse {
            universe,
        }))
    }

    async fn delete_universe(
        &self,
        params: OperatorUniverseDeleteParams,
    ) -> Result<AgentApiOutcome<OperatorUniverseDeleteResponse>, AgentApiError> {
        let universe_id = parse_universe_id(&params.universe_id)?;
        if !store_pg::universe_exists(self.pool(), universe_id)
            .await
            .map_err(map_store_error)?
        {
            return Err(AgentApiError::not_found(format!(
                "unknown universe: {universe_id}"
            )));
        }

        let session_ids = store_pg::list_universe_session_ids(self.pool(), universe_id)
            .await
            .map_err(map_store_error)?;
        let mut workflows_terminated = 0u64;
        for session_id in &session_ids {
            if self
                .terminate_session_workflow(universe_id, session_id)
                .await?
            {
                workflows_terminated += 1;
            }
        }

        let object_keys = store_pg::list_universe_object_keys(self.pool(), universe_id)
            .await
            .map_err(map_store_error)?;
        let mut blob_objects_deleted = 0u64;
        if !object_keys.is_empty() {
            let Some(object_store) = self.runtime.stores().object_store() else {
                return Err(AgentApiError::internal(format!(
                    "universe {universe_id} has externally stored blobs but no object store is configured"
                )));
            };
            for key in &object_keys {
                match object_store.delete(&ObjectPath::from(key.as_str())).await {
                    Ok(()) => blob_objects_deleted += 1,
                    // Already swept by an earlier partial purge.
                    Err(object_store::Error::NotFound { .. }) => {}
                    Err(error) => {
                        return Err(AgentApiError::internal(format!(
                            "delete blob object {key}: {error}"
                        )));
                    }
                }
            }
        }

        store_pg::delete_universe(self.pool(), universe_id)
            .await
            .map_err(map_store_error)?;
        self.runtime.evict(universe_id).await;

        tracing::info!(
            target: "temporal_server",
            universe_id = %universe_id,
            sessions = session_ids.len(),
            workflows_terminated,
            blob_objects_deleted,
            "universe purged"
        );
        Ok(AgentApiOutcome::new(OperatorUniverseDeleteResponse {
            universe_id: universe_id.to_string(),
            workflows_terminated,
            blob_objects_deleted,
        }))
    }

    async fn read_outbox(
        &self,
        params: OperatorOutboxReadParams,
    ) -> Result<AgentApiOutcome<OperatorOutboxReadResponse>, AgentApiError> {
        let after = params.after.unwrap_or(0);
        let limit = params.limit.unwrap_or(64).clamp(1, 256) as usize;
        let wait =
            Duration::from_millis(u64::from(params.wait_ms.unwrap_or(0))).min(OUTBOX_WAIT_CAP);
        let deadline = Instant::now() + wait;
        loop {
            let entries = store_pg::read_pending_outbound_all_universes(self.pool(), after, limit)
                .await
                .map_err(map_messaging_error)?;
            if !entries.is_empty() || Instant::now() >= deadline {
                let next_after = entries
                    .last()
                    .map(|entry| entry.message.seq)
                    .unwrap_or(after);
                let entries = entries
                    .into_iter()
                    .map(|entry| OperatorOutboundMessageView {
                        universe_id: entry.universe_id.to_string(),
                        message: outbound_message_view(entry.message),
                    })
                    .collect();
                return Ok(AgentApiOutcome::new(OperatorOutboxReadResponse {
                    entries,
                    next_after,
                }));
            }
            tokio::time::sleep(OUTBOX_POLL_INTERVAL).await;
        }
    }
}

fn parse_universe_id(value: &str) -> Result<Uuid, AgentApiError> {
    Uuid::parse_str(value.trim())
        .map_err(|error| AgentApiError::invalid_request(format!("invalid universe id: {error}")))
}

fn universe_view(stats: store_pg::UniverseStats) -> OperatorUniverseView {
    OperatorUniverseView {
        universe_id: stats.universe_id.to_string(),
        slug: stats.slug,
        created_at_ms: u64::try_from(stats.created_at_ms).unwrap_or(0),
        last_activity_at_ms: stats
            .last_activity_at_ms
            .map(|value| u64::try_from(value).unwrap_or(0)),
        sessions: stats.sessions,
        workspaces: stats.workspaces,
        profiles: stats.profiles,
        blob_bytes: stats.blob_bytes,
    }
}

fn map_store_error(error: store_pg::PgStoreError) -> AgentApiError {
    AgentApiError::internal(error.to_string())
}
