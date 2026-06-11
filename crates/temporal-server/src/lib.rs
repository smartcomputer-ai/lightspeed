//! Hosted Forge runtime.
//!
//! This crate owns the process-facing Temporal gateway and worker wiring. The
//! deterministic workflow contract remains in `temporal-workflow`.

pub mod config;
pub mod gateway;
pub mod worker;

pub use config::{default_model_from_env, pg_store_from_env, task_queue_from_env};
