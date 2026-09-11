//! Shared gateway/activity discovery at eligible idle boundaries. Never wakes a machine.
use crate::{
    environment_gateway::EnvironmentGatewayClientConfig, environment_resolver::EnvironmentResolver,
};
use engine::{
    ContextEntryInput, CoreAgentCommand, EnvironmentId, EnvironmentsFeature, SessionId,
    storage::{BlobStore, BlobStoreError},
};
use environment_client::EnvironmentDataClient;
use environment_protocol::{
    data::handshake::{InitializeParams, InitializedParams},
    shared::CURRENT_PROTOCOL_VERSION,
};
use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
    time::Duration,
};
use tools::skills::environment::*;

#[derive(Clone)]
struct CachedObservation {
    fingerprint: String,
    catalog: EnvironmentSkillCatalog,
}
// An optimization only: losing/evicting this cache forces a complete scan. The
// durable semantic snapshot in context remains the last observation on failure.
static OBSERVATIONS: OnceLock<Mutex<BTreeMap<String, CachedObservation>>> = OnceLock::new();
fn observations() -> &'static Mutex<BTreeMap<String, CachedObservation>> {
    OBSERVATIONS.get_or_init(Default::default)
}

pub(crate) async fn refresh(
    blobs: &dyn BlobStore,
    resolver: Option<&EnvironmentResolver>,
    gateway: Option<&EnvironmentGatewayClientConfig>,
    session_id: &SessionId,
    feature: Option<&EnvironmentsFeature>,
    environment_id: Option<&EnvironmentId>,
    current: Option<&ContextEntryInput>,
) -> Result<Option<CoreAgentCommand>, BlobStoreError> {
    // Only runtime-owned entries are refreshed. Public controller entries are independent.
    if current.is_some_and(|entry| {
        !entry
            .origin
            .as_deref()
            .is_some_and(|origin| origin.starts_with("runtime.environment:"))
    }) {
        return Ok(None);
    }
    let Some((feature, config, environment_id)) =
        feature.and_then(|f| Some((f, f.skills.as_ref()?, environment_id?)))
    else {
        return Ok(tools::catalog::clear_catalog_command(
            current,
            ENVIRONMENT_SKILL_CATALOG_CONTEXT_KEY,
        ));
    };
    let attempt = async {
        let resolver = resolver.ok_or("environment discovery resolver unavailable")?;
        let gateway = gateway.ok_or("environment gateway unavailable")?;
        let policy = environments::EnvironmentAccessPolicy::new(
            feature.providers.clone(),
            feature.registration_keys.clone(),
        );
        let environment = resolver
            .read_allowed(environment_id, &policy)
            .await
            .map_err(|e| e.to_string())?;
        if environment.status != environments::EnvironmentStatus::Ready
            || environment.desired_power != environments::PowerState::Running
        {
            return Err("environment is not accessible; discovery does not wake it".to_owned());
        }
        let connection = gateway.connection_for(resolver.universe_id(), &environment);
        let mut client = EnvironmentDataClient::connect(
            &connection.endpoint,
            gateway.connect_options("lightspeed-skill-discovery"),
        )
        .await
        .map_err(|e| e.to_string())?;
        let result = async {
            let initialized = client
                .initialize(&InitializeParams {
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    client_name: "lightspeed-skill-discovery".into(),
                    scope: connection.scope.clone(),
                    resume_connection_id: None,
                })
                .await
                .map_err(|e| e.to_string())?;
            if initialized.protocol_version != CURRENT_PROTOCOL_VERSION
                || !initialized.capabilities.filesystem_scan
                || !initialized.capabilities.filesystem_read
            {
                return Err("endpoint does not support fs/scan discovery".to_owned());
            }
            client
                .initialized(&InitializedParams {})
                .await
                .map_err(|e| e.to_string())?;
            let cwd = crate::environment_sources::working_directory(
                &mut client,
                feature.working_directory.as_deref(),
                initialized.default_cwd.as_deref(),
            )
            .await?;
            let mut query = environment_skill_scan_query(
                config,
                Some(&cwd),
                initialized.home_directory.as_deref(),
            )?;
            let cache_key = serde_json::to_string(&(
                resolver.universe_id(),
                session_id,
                environment_id,
                feature,
                &connection,
                &query,
            ))
            .map_err(|e| e.to_string())?;
            let cached = observations()
                .lock()
                .expect("observation lock")
                .get(&cache_key)
                .cloned();
            query.if_none_match = cached.as_ref().map(|c| c.fingerprint.clone());
            let scan = client.scan(&query).await.map_err(|e| e.to_string())?;
            if !scan.complete {
                return Err(format!(
                    "incomplete environment discovery: {:?}",
                    scan.diagnostics
                ));
            }
            if scan.unchanged {
                return cached
                    .filter(|cached| scan.fingerprint.as_ref() == Some(&cached.fingerprint))
                    .map(|cached| cached.catalog)
                    .ok_or("unexpected unchanged scan without matching observation".to_owned());
            }
            let catalog = environment_skill_catalog(environment_id.as_str(), &scan)?;
            if let Some(fingerprint) = scan.fingerprint {
                let mut cache = observations().lock().expect("observation lock");
                if cache.len() >= 128 {
                    cache.clear();
                }
                cache.insert(
                    cache_key,
                    CachedObservation {
                        fingerprint,
                        catalog: catalog.clone(),
                    },
                );
            }
            Ok(catalog)
        }
        .await;
        let _ = client.close().await;
        result
    };
    let catalog = match tokio::time::timeout(Duration::from_secs(4), attempt).await {
        Ok(Ok(catalog)) => catalog,
        failure => {
            tracing::debug!(?failure, %environment_id, "environment skill discovery unavailable");
            let mut catalog = EnvironmentSkillCatalog::unavailable(environment_id.as_str());
            catalog.warnings.push(format!(
                "Environment skill discovery unavailable: {failure:?}"
            ));
            catalog
        }
    };
    publish_environment_skill_catalog(blobs, current, &catalog).await
}
