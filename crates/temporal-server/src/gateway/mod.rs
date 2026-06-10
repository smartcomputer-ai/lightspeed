//! HTTP/JSON-RPC gateway over the Temporal-backed agent workflow.

pub mod http;
mod service;

pub use crate::config::{default_model_from_env, pg_store_from_env};
pub use http::{
    DEFAULT_GATEWAY_BIND, DEFAULT_MAX_REQUEST_BODY_BYTES, GatewayServerConfig, gateway_router,
    public_base_url_or_default, serve_gateway, serve_gateway_with_client_store,
};
pub use service::{
    DEFAULT_PUBLIC_BASE_URL, GatewayAgentApi, GatewayAgentApiBuilder, OAuthCallbackOutcome,
};
pub use temporal_workflow::{
    AgentAdmission, AgentAdmissionFailure, AgentAdmissionFailureKind, AgentCompletedRunSummary,
    AgentSessionArgs, AgentSessionStatus, AgentSessionWorkflow, DEFAULT_MODEL, DEFAULT_TASK_QUEUE,
    DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET, connect_temporal, default_session_config,
};
