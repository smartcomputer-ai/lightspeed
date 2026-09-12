//! Workflow activity discovery at eligible idle boundaries. Never wakes a machine.
use crate::environment_sources::{Discovery, PhaseTimer};
use engine::{
    ContextEntryInput, CoreAgentCommand, SessionId,
    storage::{BlobStore, BlobStoreError},
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
// cached catalog is reused only after the environment confirms its fingerprint.
static OBSERVATIONS: OnceLock<Mutex<BTreeMap<String, CachedObservation>>> = OnceLock::new();
fn observations() -> &'static Mutex<BTreeMap<String, CachedObservation>> {
    OBSERVATIONS.get_or_init(Default::default)
}

pub(crate) async fn refresh(
    blobs: &dyn BlobStore,
    discovery: &mut Discovery<'_>,
    session_id: &SessionId,
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
    let Some((feature, config, environment_id)) = discovery
        .feature
        .and_then(|f| Some((f, f.skills.as_ref()?, discovery.environment_id?)))
    else {
        return Ok(tools::catalog::clear_catalog_command(
            current,
            ENVIRONMENT_SKILL_CATALOG_CONTEXT_KEY,
        ));
    };
    let attempt = async {
        let connection = discovery.connection().await?;
        let mut query = environment_skill_scan_query(
            config,
            Some(&connection.cwd),
            connection.initialized.home_directory.as_deref(),
        )?;
        let cache_key = serde_json::to_string(&(
            session_id,
            environment_id,
            feature,
            &connection.identity,
            &query,
        ))
        .map_err(|e| e.to_string())?;
        let cached = observations()
            .lock()
            .expect("observation lock")
            .get(&cache_key)
            .cloned();
        query.if_none_match = cached.as_ref().map(|c| c.fingerprint.clone());
        let scan = {
            let _timer = PhaseTimer::new("skills_scan");
            connection
                .client
                .scan(&query)
                .await
                .map_err(|e| e.to_string())?
        };
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
    };
    let catalog = match tokio::time::timeout(Duration::from_secs(4), attempt).await {
        Ok(Ok(catalog)) => catalog,
        failure => {
            discovery.discard_connection();
            tracing::debug!(?failure, %environment_id, "environment skill discovery unavailable");
            let mut catalog = EnvironmentSkillCatalog::unavailable(environment_id.as_str());
            catalog.warnings.push(format!(
                "Environment skill discovery unavailable: {failure:?}"
            ));
            catalog
        }
    };
    let _timer = PhaseTimer::new("skills_publication");
    publish_environment_skill_catalog(blobs, current, &catalog).await
}
