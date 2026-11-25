//! Resource HTTP gateway over the Temporal-backed agent workflow.

pub mod authentication;
pub mod deployment;
mod enforcement;
pub mod http;
mod peers;
pub mod registration;
pub mod replica;
pub mod request_context;
pub(crate) mod service;
mod watch;

pub use crate::config::pg_store_from_env;
pub use deployment::GatewayDeploymentApi;
pub use http::{
    ACTOR_HEADER, DEFAULT_ENVIRONMENT_GATEWAY_BIND, DEFAULT_GATEWAY_BIND,
    DEFAULT_MAX_BLOB_UPLOAD_BYTES, DEFAULT_MAX_REQUEST_BODY_BYTES, DEFAULT_PUBLIC_BIND,
    GatewayRoutes, GatewayState, UNIVERSE_HEADER, gateway_router, gateway_router_with_limits,
    prewarm_single_universe,
};
pub use service::{
    DEFAULT_PUBLIC_BASE_URL, GatewayAgentApi, GatewayAgentApiBuilder, OAuthCallbackOutcome,
    ReconcileFailureLog,
};
pub use temporal_workflow::{
    AgentAdmission, AgentAdmissionFailure, AgentAdmissionFailureKind, AgentFinishedRunSummary,
    AgentSessionArgs, AgentSessionStatus, AgentSessionWorkflow, DEFAULT_TASK_QUEUE,
    DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET, TemporalConfigError,
    TemporalConnectionConfig, TemporalConnectionSettings, connect_temporal, default_session_config,
};
