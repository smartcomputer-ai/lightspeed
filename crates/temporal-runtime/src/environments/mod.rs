//! Environment lifecycle management, connectivity, runtime resolution, and source discovery.
use crate::environments::gateway::EnvironmentGatewayClientConfig;
use api::*;
use lifecycle::parse_registry_environment_id;
use provider_controllers::{ProviderControllerConnector, finish_provider_controller};
use providers::{map_environments_error, parse_environment_provider_id};
use std::{collections::BTreeMap, sync::Arc};
use store_pg::PgStore;
pub(crate) mod credentials;
pub(crate) mod lifecycle;
pub(crate) mod power;
pub(crate) mod provider_controllers;
pub(crate) mod providers;
pub(crate) mod registration;

pub mod gateway;
pub(crate) mod prompts;
pub(crate) mod resolver;
pub mod runtime;
pub(crate) mod skills;
pub(crate) mod sources;

#[derive(Clone)]
pub(crate) struct EnvironmentService {
    pub(crate) store: Arc<PgStore>,
    pub(crate) environment_gateway: EnvironmentGatewayClientConfig,
    pub(crate) provider_controller_connector: Arc<dyn ProviderControllerConnector>,
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
    environment_protocol::registration::validate_registration_metadata(None, metadata)
        .map_err(|message| AgentApiError::invalid_request(format!("invalid metadata: {message}")))
}
