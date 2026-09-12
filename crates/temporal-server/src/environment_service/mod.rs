//! Shared environment service for environment APIs and background reconciliation.
use crate::environment_gateway::EnvironmentGatewayClientConfig;
use api::*;
use environment_lifecycle::parse_registry_environment_id;
use environment_providers::{map_environments_error, parse_environment_provider_id};
use provider_controllers::{ProviderControllerConnector, finish_provider_controller};
use std::{collections::BTreeMap, sync::Arc};
use store_pg::PgStore;
pub(crate) mod environment_credentials;
pub(crate) mod environment_lifecycle;
pub(crate) mod environment_power;
pub(crate) mod environment_providers;
pub(crate) mod environment_registration;
pub(crate) mod provider_controllers;
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
