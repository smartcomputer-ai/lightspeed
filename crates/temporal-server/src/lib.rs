//! Hosted Lightspeed runtime.
//!
//! This crate owns the process-facing Temporal gateway and worker wiring. The
//! deterministic workflow contract remains in `temporal-workflow`.

pub mod config;
pub(crate) mod credential_injection;
pub mod environment;
pub mod fleet;
pub mod gateway;
pub(crate) mod transcript;
pub mod universe;
pub mod worker;

pub use config::{
    DeploymentStores, GatewayAuthMode, default_model_from_env, gateway_auth_mode_from_env,
    pg_store_from_env, task_queue_from_env, universe_id_from_env,
};
pub use universe::{UniverseError, UniverseRuntime, UniverseState};
