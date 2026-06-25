use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicU64, Ordering},
};

use api::{
    AgentApiError, AgentApiErrorKind, AgentApiOutcome, EnvironmentProviderCapabilitiesView,
    EnvironmentProviderHeartbeatParams, EnvironmentProviderHeartbeatResponse,
    EnvironmentProviderImplementationView, EnvironmentProviderKindView,
    EnvironmentProviderRegisterParams, EnvironmentProviderRegisterResponse,
    EnvironmentProviderUnregisterParams, EnvironmentProviderUnregisterResponse,
    EnvironmentTargetStatusView, EnvironmentTargetSummaryView, HostCapabilitiesView,
    HostControllerConnectionView, HostScopeView, HostTransportView, JsonRpcRequest,
    JsonRpcResponse, METHOD_ENVIRONMENT_PROVIDERS_HEARTBEAT, METHOD_ENVIRONMENT_PROVIDERS_REGISTER,
    METHOD_ENVIRONMENT_PROVIDERS_UNREGISTER, RequestId,
};
use host_protocol::{
    control::targets::{HostTargetStatus, HostTargetSummary},
    shared::{HostCapabilities, HostScope, HostTransport, ImplementationInfo},
};
use serde::{Serialize, de::DeserializeOwned};

use crate::{BridgeRuntime, config::BridgeConfig};

pub struct GatewayClient {
    endpoint: String,
    bearer_token: Option<String>,
    client: reqwest::Client,
    next_id: AtomicU64,
}

impl GatewayClient {
    pub fn new(endpoint: impl Into<String>, bearer_token: Option<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            bearer_token,
            client: reqwest::Client::new(),
            next_id: AtomicU64::new(1),
        }
    }

    pub async fn register(
        &self,
        config: &BridgeConfig,
        runtime: &BridgeRuntime,
    ) -> Result<AgentApiOutcome<EnvironmentProviderRegisterResponse>, AgentApiError> {
        self.request(
            METHOD_ENVIRONMENT_PROVIDERS_REGISTER,
            EnvironmentProviderRegisterParams {
                provider_id: config.provider_id.clone(),
                provider_kind: EnvironmentProviderKindView::Bridge,
                controller_connection: HostControllerConnectionView {
                    endpoint: runtime.controller_endpoint(),
                    transport: HostTransportView::WebSocket,
                },
                capabilities: EnvironmentProviderCapabilitiesView {
                    list_targets: true,
                    create_target: false,
                    attach_target: true,
                    get_target: true,
                    close_target: true,
                },
                implementation: implementation_view(runtime.implementation()),
                lease_ttl_ms: config.lease_ttl_ms_i64(),
                display_name: Some(config.display_name()),
                metadata: BTreeMap::new(),
            },
        )
        .await
    }

    pub async fn heartbeat(
        &self,
        config: &BridgeConfig,
        target: HostTargetSummary,
    ) -> Result<AgentApiOutcome<EnvironmentProviderHeartbeatResponse>, AgentApiError> {
        self.request(
            METHOD_ENVIRONMENT_PROVIDERS_HEARTBEAT,
            EnvironmentProviderHeartbeatParams {
                provider_id: config.provider_id.clone(),
                lease_ttl_ms: Some(config.lease_ttl_ms_i64()),
                observed_targets: vec![target_summary_view(target)],
            },
        )
        .await
    }

    pub async fn unregister(
        &self,
        config: &BridgeConfig,
    ) -> Result<AgentApiOutcome<EnvironmentProviderUnregisterResponse>, AgentApiError> {
        self.request(
            METHOD_ENVIRONMENT_PROVIDERS_UNREGISTER,
            EnvironmentProviderUnregisterParams {
                provider_id: config.provider_id.clone(),
            },
        )
        .await
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

        let mut builder = self.client.post(&self.endpoint).json(&request);
        if let Some(token) = &self.bearer_token {
            builder = builder.bearer_auth(token);
        }
        let response = builder
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

fn agent_error_from_json_rpc(error: api::JsonRpcError) -> AgentApiError {
    if let Some(error) = error.data {
        return error;
    }
    let kind = match error.code {
        -32602 => AgentApiErrorKind::InvalidRequest,
        -32004 => AgentApiErrorKind::NotFound,
        -32009 => AgentApiErrorKind::Conflict,
        -32010 => AgentApiErrorKind::Rejected,
        _ => AgentApiErrorKind::Internal,
    };
    AgentApiError::new(kind, error.message)
}

fn implementation_view(value: ImplementationInfo) -> EnvironmentProviderImplementationView {
    EnvironmentProviderImplementationView {
        name: value.name,
        version: value.version,
    }
}

fn target_summary_view(value: HostTargetSummary) -> EnvironmentTargetSummaryView {
    EnvironmentTargetSummaryView {
        target_id: value.target_id.0,
        status: target_status_view(value.status),
        scope: scope_view(value.scope),
        capabilities: capabilities_view(value.capabilities),
        display_name: value.display_name,
        default_cwd: value.default_cwd.map(|path| path.to_string()),
        metadata: value.metadata,
    }
}

fn target_status_view(value: HostTargetStatus) -> EnvironmentTargetStatusView {
    match value {
        HostTargetStatus::Creating => EnvironmentTargetStatusView::Creating,
        HostTargetStatus::Starting => EnvironmentTargetStatusView::Starting,
        HostTargetStatus::Ready => EnvironmentTargetStatusView::Ready,
        HostTargetStatus::Stopped => EnvironmentTargetStatusView::Stopped,
        HostTargetStatus::Closing => EnvironmentTargetStatusView::Closing,
        HostTargetStatus::Closed => EnvironmentTargetStatusView::Closed,
        HostTargetStatus::Failed => EnvironmentTargetStatusView::Failed,
        HostTargetStatus::Unknown => EnvironmentTargetStatusView::Unknown,
    }
}

fn scope_view(value: HostScope) -> HostScopeView {
    match value {
        HostScope::Default => HostScopeView::Default,
        HostScope::Session { session_id } => HostScopeView::Session { session_id },
    }
}

fn capabilities_view(value: HostCapabilities) -> HostCapabilitiesView {
    HostCapabilitiesView {
        filesystem_read: value.filesystem_read,
        filesystem_write: value.filesystem_write,
        process_start: value.process_start,
        process_stdin: value.process_stdin,
        process_terminate: value.process_terminate,
        process_output_polling: value.process_output_polling,
        process_output_notifications: value.process_output_notifications,
        process_pty: value.process_pty,
        job_start: value.job_start,
        job_list: value.job_list,
        job_read: value.job_read,
        job_cancel: value.job_cancel,
        job_wait_hint: value.job_wait_hint,
        job_dependencies: value.job_dependencies,
        job_queue_keys: value.job_queue_keys,
    }
}

#[allow(dead_code)]
fn transport_view(value: HostTransport) -> HostTransportView {
    match value {
        HostTransport::WebSocket => HostTransportView::WebSocket,
        HostTransport::Http => HostTransportView::Http,
        HostTransport::Stdio => HostTransportView::Stdio,
        HostTransport::Ssh => HostTransportView::Ssh,
        HostTransport::Provider { provider_type } => HostTransportView::Provider { provider_type },
    }
}
