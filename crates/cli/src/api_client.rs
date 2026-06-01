use std::sync::atomic::{AtomicU64, Ordering};

use api::{
    AgentApiError, AgentApiErrorKind, AgentApiOutcome, BlobGetParams, BlobGetResponse,
    BlobHasManyParams, BlobHasManyResponse, BlobPutManyParams, BlobPutManyResponse, JsonRpcRequest,
    JsonRpcResponse, METHOD_BLOB_GET, METHOD_BLOB_HAS_MANY, METHOD_BLOB_PUT_MANY, METHOD_RUN_START,
    METHOD_SESSION_EVENTS_READ, METHOD_SESSION_READ, METHOD_SESSION_START,
    METHOD_VFS_SNAPSHOT_COMMIT, METHOD_VFS_SNAPSHOT_READ, RequestId, RunStartParams,
    RunStartResponse, SessionEventsReadParams, SessionEventsReadResponse, SessionReadParams,
    SessionReadResponse, SessionStartParams, SessionStartResponse, VfsSnapshotCommitParams,
    VfsSnapshotCommitResponse, VfsSnapshotReadParams, VfsSnapshotReadResponse,
};
use serde::{Serialize, de::DeserializeOwned};

pub(crate) struct HttpAgentApi {
    endpoint: String,
    client: reqwest::Client,
    next_id: AtomicU64,
}

impl HttpAgentApi {
    pub(crate) fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            client: reqwest::Client::new(),
            next_id: AtomicU64::new(1),
        }
    }

    pub(crate) async fn open_or_start_session(
        &self,
        params: SessionStartParams,
    ) -> Result<AgentApiOutcome<SessionStartResponse>, AgentApiError> {
        match self.start_session(params.clone()).await {
            Ok(outcome) => Ok(outcome),
            Err(error)
                if matches!(error.kind, AgentApiErrorKind::Conflict)
                    && params.session_id.is_some() =>
            {
                self.read_session(SessionReadParams {
                    session_id: params.session_id.expect("checked session id present"),
                })
                .await
                .map(|outcome| {
                    AgentApiOutcome::with_notifications(
                        SessionStartResponse {
                            session: outcome.result.session,
                        },
                        outcome.notifications,
                    )
                })
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn start_session(
        &self,
        params: SessionStartParams,
    ) -> Result<AgentApiOutcome<SessionStartResponse>, AgentApiError> {
        self.request(METHOD_SESSION_START, params).await
    }

    pub(crate) async fn read_session(
        &self,
        params: SessionReadParams,
    ) -> Result<AgentApiOutcome<SessionReadResponse>, AgentApiError> {
        self.request(METHOD_SESSION_READ, params).await
    }

    pub(crate) async fn read_session_events(
        &self,
        params: SessionEventsReadParams,
    ) -> Result<AgentApiOutcome<SessionEventsReadResponse>, AgentApiError> {
        self.request(METHOD_SESSION_EVENTS_READ, params).await
    }

    pub(crate) async fn start_run(
        &self,
        params: RunStartParams,
    ) -> Result<AgentApiOutcome<RunStartResponse>, AgentApiError> {
        self.request(METHOD_RUN_START, params).await
    }

    pub(crate) async fn put_blobs(
        &self,
        params: BlobPutManyParams,
    ) -> Result<AgentApiOutcome<BlobPutManyResponse>, AgentApiError> {
        self.request(METHOD_BLOB_PUT_MANY, params).await
    }

    pub(crate) async fn has_blobs(
        &self,
        params: BlobHasManyParams,
    ) -> Result<AgentApiOutcome<BlobHasManyResponse>, AgentApiError> {
        self.request(METHOD_BLOB_HAS_MANY, params).await
    }

    pub(crate) async fn get_blob(
        &self,
        params: BlobGetParams,
    ) -> Result<AgentApiOutcome<BlobGetResponse>, AgentApiError> {
        self.request(METHOD_BLOB_GET, params).await
    }

    pub(crate) async fn commit_vfs_snapshot(
        &self,
        params: VfsSnapshotCommitParams,
    ) -> Result<AgentApiOutcome<VfsSnapshotCommitResponse>, AgentApiError> {
        self.request(METHOD_VFS_SNAPSHOT_COMMIT, params).await
    }

    pub(crate) async fn read_vfs_snapshot(
        &self,
        params: VfsSnapshotReadParams,
    ) -> Result<AgentApiOutcome<VfsSnapshotReadResponse>, AgentApiError> {
        self.request(METHOD_VFS_SNAPSHOT_READ, params).await
    }

    async fn request<P, R>(
        &self,
        method: &str,
        params: P,
    ) -> Result<AgentApiOutcome<R>, AgentApiError>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let id = RequestId::Number(self.next_id.fetch_add(1, Ordering::Relaxed));
        let request = JsonRpcRequest {
            id,
            method: method.to_owned(),
            params: Some(serde_json::to_value(params).map_err(|error| {
                AgentApiError::invalid_request(format!("failed to encode API params: {error}"))
            })?),
        };
        let response = self
            .client
            .post(&self.endpoint)
            .json(&request)
            .send()
            .await
            .map_err(|error| AgentApiError::internal(format!("API request failed: {error}")))?
            .error_for_status()
            .map_err(|error| AgentApiError::internal(format!("API request failed: {error}")))?
            .json::<JsonRpcResponse>()
            .await
            .map_err(|error| AgentApiError::internal(format!("invalid API response: {error}")))?;
        if let Some(error) = response.error {
            return Err(agent_error_from_json_rpc(error));
        }
        let value = response
            .result
            .ok_or_else(|| AgentApiError::internal("JSON-RPC response missing result"))?;
        serde_json::from_value::<AgentApiOutcome<R>>(value)
            .map_err(|error| AgentApiError::internal(format!("invalid API result: {error}")))
    }
}

pub(crate) fn api_error(error: api::AgentApiError) -> anyhow::Error {
    anyhow::anyhow!("{error}")
}

fn agent_error_from_json_rpc(error: api::JsonRpcError) -> AgentApiError {
    let kind = match error.code {
        -32602 => AgentApiErrorKind::InvalidRequest,
        -32004 => AgentApiErrorKind::NotFound,
        -32009 => AgentApiErrorKind::Conflict,
        -32010 => AgentApiErrorKind::Rejected,
        _ => AgentApiErrorKind::Internal,
    };
    AgentApiError::new(kind, error.message)
}
