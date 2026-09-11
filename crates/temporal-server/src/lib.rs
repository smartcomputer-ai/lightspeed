//! Hosted Lightspeed runtime.
//!
//! This crate owns the process-facing Temporal gateway and worker wiring. The
//! deterministic workflow contract remains in `temporal-workflow`.

pub mod bots;
pub mod channels;
pub(crate) mod checkpoint;
pub mod config;
pub(crate) mod credential_injection;
pub mod environment;
pub mod environment_gateway;
mod environment_prompts;
pub(crate) mod environment_resolver;
mod environment_skills;
mod environment_sources;
pub mod gateway;
pub mod roles;
pub(crate) mod session_deletion;
pub mod subagents;
pub mod universe;
pub mod worker;

pub use config::{
    DeploymentStores, GatewayAuthMode, default_model_from_env, gateway_auth_mode_from_env,
    pg_store_from_env, task_queue_from_env, universe_id_from_env,
};
pub use universe::{UniverseError, UniverseRuntime, UniverseState};
