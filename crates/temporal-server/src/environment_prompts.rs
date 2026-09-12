//! Idle-boundary environment prompt refresh, independent of VFS instruction ownership.
use crate::environment_sources::{Discovery, PhaseTimer};
use engine::{
    ContextEntryInput, ContextEntryKey,
    storage::{BlobStore, BlobStoreError},
};
use std::{collections::BTreeMap, time::Duration};

pub(crate) async fn refresh(
    blobs: &dyn BlobStore,
    discovery: &mut Discovery<'_>,
) -> Result<BTreeMap<ContextEntryKey, ContextEntryInput>, BlobStoreError> {
    let Some((source, id)) = discovery
        .feature
        .and_then(|feature| Some((feature.prompts.as_ref()?, discovery.environment_id?)))
    else {
        return Ok(BTreeMap::new());
    };
    let attempt = async {
        let connection = discovery.connection().await?;
        let query = tools::environment::sources::scan_query(
            source.roots.as_deref(),
            &connection.cwd,
            connection.initialized.home_directory.as_deref(),
            "prompts",
        )?;
        let scan = {
            let _timer = PhaseTimer::new("prompts_scan");
            connection
                .client
                .scan(&query)
                .await
                .map_err(|e| e.to_string())?
        };
        tools::prompts::environment::assemble(&scan)
    };
    let observation = tokio::time::timeout(Duration::from_secs(4), attempt)
        .await
        .unwrap_or_else(|_| Err("environment prompt discovery timed out".into()));
    if observation.is_err() {
        discovery.discard_connection();
    }
    let _timer = PhaseTimer::new("prompts_publication");
    tools::prompts::environment::publication(blobs, id.as_str(), observation).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::storage::{BlobStore, InMemoryBlobStore};
    use engine::{EnvironmentId, EnvironmentsFeature, SessionId};
    use tools::prompts::environment::{
        ENVIRONMENT_PROMPT_CONTEXT_KEY, EnvironmentPromptReport, assemble, publication,
    };

    #[tokio::test(flavor = "current_thread")]
    async fn local_environment_prompts_scan_in_order_and_replace_on_failure() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        let cwd = root.join("project");
        let home = root.join("home");
        std::fs::create_dir_all(cwd.join(".agents/prompts")).unwrap();
        std::fs::create_dir_all(home.join(".lightspeed/prompts")).unwrap();
        std::fs::create_dir_all(home.join(".codex/prompts")).unwrap();
        std::fs::write(
            cwd.join(".agents/prompts/instructions.md"),
            "Base instructions",
        )
        .unwrap();
        std::fs::write(
            cwd.join(".agents/prompts/020-last.txt"),
            "Last instructions",
        )
        .unwrap();
        std::fs::write(
            cwd.join(".agents/prompts/010-first.md"),
            "First instructions",
        )
        .unwrap();
        std::fs::write(home.join(".codex/prompts/instructions.md"), "Must not load").unwrap();
        std::fs::create_dir_all(cwd.join(".agents/prompts/nested/deep")).unwrap();
        std::fs::write(
            cwd.join(".agents/prompts/nested/deep/hidden.md"),
            "Must not load",
        )
        .unwrap();
        std::fs::write(cwd.join(".agents/prompts/notes.json"), "Must not load").unwrap();
        let fs =
            environment_daemon::filesystem::LocalFileSystem::new(root.clone(), cwd.clone(), true);
        let query = tools::environment::sources::scan_query(
            None,
            cwd.to_str().unwrap(),
            home.to_str(),
            "prompts",
        )
        .unwrap();
        let scan = fs.scan(query).await.unwrap();
        let (text, paths) = assemble(&scan).unwrap();
        assert_eq!(
            text,
            "First instructions\n\nLast instructions\n\nBase instructions"
        );
        assert_eq!(paths.len(), 3);
        let blobs = InMemoryBlobStore::new();
        let entries = publication(&blobs, "machine", Ok((text, paths)))
            .await
            .unwrap();
        let entry = &entries[&ContextEntryKey::new(ENVIRONMENT_PROMPT_CONTEXT_KEY)];
        let report: EnvironmentPromptReport = serde_json::from_slice(
            &blobs
                .read_bytes(entry.provenance_ref.as_ref().unwrap())
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(report.available);
        let mut incomplete = scan.clone();
        incomplete.complete = false;
        assert!(assemble(&incomplete).is_err());
        let failed = publication(&blobs, "other-machine", assemble(&incomplete))
            .await
            .unwrap();
        let failed = &failed[&ContextEntryKey::new(ENVIRONMENT_PROMPT_CONTEXT_KEY)];
        assert_eq!(
            failed.origin.as_deref(),
            Some("runtime.environment:other-machine")
        );
        let text = String::from_utf8(blobs.read_bytes(&failed.content.content_ref).await.unwrap())
            .unwrap();
        assert!(text.contains("unavailable"));
        assert!(!text.contains("Base instructions"));
        assert!(
            publication(&blobs, "machine", Ok((String::new(), vec![])))
                .await
                .unwrap()
                .is_empty()
        );
        let feature = EnvironmentsFeature {
            skills: Some(Default::default()),
            ..Default::default()
        };
        assert!(
            crate::environment_sources::refresh(
                &blobs,
                None,
                None,
                &SessionId::new("session"),
                Some(&feature),
                Some(&EnvironmentId::new("machine")),
                None,
            )
            .await
            .unwrap()
            .prompt_entries
            .is_empty()
        );
    }
}
