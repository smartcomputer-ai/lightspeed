//! Workflow activity discovery at eligible idle boundaries. Never wakes a machine.
use crate::environments::sources::{Discovery, PhaseTimer};
use harness::{
    ContextEntryInput, CoreAgentCommand, SessionId,
    storage::{BlobStore, BlobStoreError},
};
use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
    time::Duration,
};
use tools::skills::environment::*;

// An optimization only: losing/evicting this cache forces a full scan. A
// cached catalog contributes only the roots the environment confirms
// unchanged by their fingerprints.
static OBSERVATIONS: OnceLock<Mutex<BTreeMap<String, EnvironmentSkillCatalog>>> = OnceLock::new();
fn observations() -> &'static Mutex<BTreeMap<String, EnvironmentSkillCatalog>> {
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
    let scope = serde_json::to_string(&(
        config,
        feature
            .attachment(environment_id.as_str())
            .map(|attachment| (attachment.working_directory.as_deref(), attachment.access)),
    ))
    .expect("serialize discovery scope");
    let attempt = async {
        let connection = discovery.connection().await?;
        let query = environment_skill_scan_query(
            config,
            Some(connection.cwd.as_str()),
            connection
                .initialized
                .home_directory
                .as_ref()
                .map(|home| home.as_str()),
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
        let catalog = {
            let _timer = PhaseTimer::new("skills_scan");
            let mut cached_query = query.clone();
            if let Some(previous) = &cached {
                apply_cached_fingerprints(&mut cached_query, previous);
            }
            let scan = connection.scan(cached_query).await?;
            match environment_skill_catalog(environment_id.as_str(), &scan, cached.as_ref()) {
                Ok(catalog) => catalog,
                // A root answered unchanged against an observation the
                // catalog cannot reuse: observe every root in full.
                Err(_) if cached.is_some() => {
                    let scan = connection.scan(query).await?;
                    environment_skill_catalog(environment_id.as_str(), &scan, None)?
                }
                Err(error) => return Err(error),
            }
        };
        let mut cache = observations().lock().expect("observation lock");
        if cache.len() >= 128 {
            cache.clear();
        }
        cache.insert(cache_key, catalog.clone());
        Ok::<_, String>(catalog)
    };
    let mut catalog = match tokio::time::timeout(Duration::from_secs(4), attempt).await {
        Ok(Ok(catalog)) => catalog,
        failure => {
            discovery.discard_connection();
            tracing::debug!(?failure, %environment_id, "environment skill discovery unavailable");
            unavailable_observation(blobs, current, environment_id.as_str(), &scope).await?
        }
    };
    catalog.discovery_scope = Some(scope);
    let _timer = PhaseTimer::new("skills_publication");
    publish_environment_skill_catalog(blobs, current, &catalog).await
}

async fn unavailable_observation(
    blobs: &dyn BlobStore,
    current: Option<&ContextEntryInput>,
    environment_id: &str,
    scope: &str,
) -> Result<EnvironmentSkillCatalog, BlobStoreError> {
    let previous = match current.and_then(|entry| entry.provenance_ref.as_ref()) {
        Some(reference) => {
            serde_json::from_slice::<EnvironmentSkillCatalog>(&blobs.read_bytes(reference).await?)
                .ok()
        }
        None => None,
    };
    let mut catalog = previous
        .filter(|catalog| {
            catalog.environment_id == environment_id
                && catalog.discovery_scope.as_deref() == Some(scope)
                && catalog.availability != EnvironmentSkillAvailability::Unavailable
        })
        .unwrap_or_else(|| EnvironmentSkillCatalog::unavailable(environment_id));
    if catalog.availability != EnvironmentSkillAvailability::Unavailable {
        catalog.availability = EnvironmentSkillAvailability::Stale;
    }
    let warning = "Environment skill discovery unavailable; retry discovery when the environment is reachable.".to_owned();
    if !catalog.warnings.contains(&warning) {
        catalog.warnings.push(warning);
    }
    Ok(catalog)
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness::storage::InMemoryBlobStore;

    #[tokio::test(flavor = "current_thread")]
    async fn failed_discovery_preserves_only_the_same_persisted_scope() {
        let blobs = InMemoryBlobStore::new();
        let mut catalog = EnvironmentSkillCatalog::unavailable("machine");
        catalog.availability = EnvironmentSkillAvailability::Available;
        catalog.discovery_scope = Some("configured roots".into());
        catalog.skills.push(EnvironmentSkill {
            root: "/skills".into(),
            skill_id: tools::skills::SkillId::new("internal-id"),
            name: "review".into(),
            description: "Review changes".into(),
            short_description: None,
            skill_dir_path: "/skills/review".into(),
            skill_doc_path: "/skills/review/SKILL.md".into(),
        });
        let Some(CoreAgentCommand::UpsertContext { entry, .. }) =
            publish_environment_skill_catalog(&blobs, None, &catalog)
                .await
                .unwrap()
        else {
            panic!("initial publication")
        };
        // Reads durable provenance, without requiring the process-local scan cache.
        let stale = unavailable_observation(&blobs, Some(&entry), "machine", "configured roots")
            .await
            .unwrap();
        assert_eq!(stale.availability, EnvironmentSkillAvailability::Stale);
        assert_eq!(stale.skills, catalog.skills);
        let Some(CoreAgentCommand::UpsertContext { entry: updated, .. }) =
            publish_environment_skill_catalog(&blobs, Some(&entry), &stale)
                .await
                .unwrap()
        else {
            panic!("diagnostics update")
        };
        assert_eq!(entry.content, updated.content);
        let repeated =
            unavailable_observation(&blobs, Some(&updated), "machine", "configured roots")
                .await
                .unwrap();
        assert_eq!(repeated, stale);
        assert!(
            publish_environment_skill_catalog(&blobs, Some(&updated), &repeated)
                .await
                .unwrap()
                .is_none()
        );
        for (environment, scope) in [
            ("other", "configured roots"),
            ("machine", "different roots"),
        ] {
            let unavailable = unavailable_observation(&blobs, Some(&entry), environment, scope)
                .await
                .unwrap();
            assert_eq!(
                unavailable.availability,
                EnvironmentSkillAvailability::Unavailable
            );
            assert!(unavailable.skills.is_empty());
        }
        let text = blobs.read_text(&entry.content.content_ref).await.unwrap();
        assert!(!text.contains("internal-id"));
        assert!(!text.contains("skill_dir_path"));
        assert!(text.contains("path: /skills/review/SKILL.md"));
    }
}
