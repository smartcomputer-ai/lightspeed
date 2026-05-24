//! Public gateway over the Temporal-backed agent workflow.

mod config;
mod service;

pub use config::{default_model_from_env, pg_store_from_env};
pub use service::{GatewayAgentApi, GatewayAgentApiBuilder};
pub use workflow::{
    AgentAdmission, AgentAdmissionFailure, AgentAdmissionFailureKind, AgentCompletedRunSummary,
    AgentSessionArgs, AgentSessionStatus, AgentSessionWorkflow, DEFAULT_MODEL, DEFAULT_TASK_QUEUE,
    DEFAULT_TEMPORAL_NAMESPACE, DEFAULT_TEMPORAL_TARGET, connect_temporal, default_session_config,
};
