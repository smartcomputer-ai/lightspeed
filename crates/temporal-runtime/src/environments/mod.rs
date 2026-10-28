//! Environment lifecycle management, connectivity, runtime resolution, and source discovery.
use api::*;
use lifecycle::parse_registry_environment_id;
use provider_controllers::ProviderControllerConnector;
use providers::map_environments_error;
use signals::EnvironmentSignals;
use std::{collections::BTreeMap, sync::Arc};
use store_pg::PgStore;
pub(crate) mod credentials;
pub mod fake_controller;
pub(crate) mod lifecycle;
pub(crate) mod provider_controllers;
pub(crate) mod providers;
pub(crate) mod registration;
pub mod signals;

pub mod gateway;
pub(crate) mod prompts;
pub(crate) mod resolver;
pub mod runtime;
pub(crate) mod skills;
pub(crate) mod sources;

#[derive(Clone)]
pub(crate) struct EnvironmentService {
    pub(crate) store: Arc<PgStore>,
    pub(crate) provider_controller_connector: Arc<dyn ProviderControllerConnector>,
    pub(crate) signals: Arc<EnvironmentSignals>,
}
fn now_ms() -> Result<i64, AgentApiError> {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| AgentApiError::internal(e.to_string()))?
            .as_millis(),
    )
    .map_err(|e| AgentApiError::internal(e.to_string()))
}

fn validate_caller_metadata(metadata: &BTreeMap<String, String>) -> Result<(), AgentApiError> {
    environment_protocol::metadata::validate_caller_metadata(metadata)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid metadata: {error}")))
}
