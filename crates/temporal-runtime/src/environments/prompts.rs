//! Idle-boundary environment prompt refresh, independent of VFS instruction ownership.
use crate::environments::sources::{Discovery, PhaseTimer};
use harness::{
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
            connection.cwd.as_str(),
            connection
                .initialized
                .home_directory
                .as_ref()
                .map(|home| home.as_str()),
            "prompts",
        )?;
        let scan = {
            let _timer = PhaseTimer::new("prompts_scan");
            connection.scan(query).await?
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
    use harness::storage::{BlobStore, InMemoryBlobStore};
    use harness::{EnvironmentId, EnvironmentsFeature, SessionId};
    use tools::prompts::environment::{
        ENVIRONMENT_PROMPT_CONTEXT_KEY, EnvironmentPromptReport, assemble, publication,
    };

    fn daemon(root: &std::path::Path, cwd: &std::path::Path) -> environment_daemon::DaemonRuntime {
        environment_daemon::DaemonRuntime::new(environment_daemon::config::DaemonConfig {
            listen: None,
            cwd: cwd.to_owned(),
            fs_root: root.to_owned(),
            state_dir: root.parent().unwrap().join("state"),
            read_only_fs: true,
            registration: None,
            scrubbed_env: Vec::new(),
        })
        .unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn local_environment_prompts_scan_in_order_and_replace_on_failure() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap().join("root");
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
        let runtime = daemon(&root, &cwd);
        let query = tools::environment::sources::scan_query(
            None,
            cwd.to_str().unwrap(),
            home.to_str(),
            "prompts",
        )
        .unwrap();
        let scan = runtime
            .filesystem()
            .scan(
                &environment_daemon::dispatch::RequestContext::detached(),
                query,
            )
            .await
            .unwrap();
        let observation = assemble(&scan).unwrap();
        assert_eq!(
            observation.text,
            "First instructions\n\nLast instructions\n\nBase instructions"
        );
        assert_eq!(observation.paths.len(), 3);
        assert!(observation.warnings.is_empty());
        let blobs = InMemoryBlobStore::new();
        let entries = publication(&blobs, "machine", Ok(observation.clone()))
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
        // A root that cannot be scanned is a warning naming it; the other
        // roots' prompts still load.
        let mut partial = scan.clone();
        let failed_root = environment_protocol::paths::EnvironmentPath::new(
            home.join(".agents/prompts").to_str().unwrap(),
        )
        .unwrap();
        partial
            .roots
            .push(environment_protocol::data::scan::ScanRootResult::Failed {
                path: failed_root.clone(),
                error: environment_protocol::error::ProtocolError::deadline_exceeded("slow"),
            });
        let degraded = assemble(&partial).unwrap();
        assert_eq!(degraded.text, observation.text);
        assert_eq!(degraded.warnings.len(), 1);
        assert!(degraded.warnings[0].starts_with(failed_root.as_str()));
        // Prompts need a full observation of every root.
        let mut unchanged = scan.clone();
        unchanged.roots.push(
            environment_protocol::data::scan::ScanRootResult::Unchanged {
                path: failed_root,
                fingerprint: environment_protocol::data::scan::ScanFingerprint::new(format!(
                    "scan:v1:{}",
                    "0".repeat(64)
                ))
                .unwrap(),
            },
        );
        let failed = publication(&blobs, "other-machine", assemble(&unchanged))
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
            publication(
                &blobs,
                "machine",
                Ok(tools::prompts::environment::PromptObservation::default())
            )
            .await
            .unwrap()
            .is_empty()
        );
        let feature = EnvironmentsFeature {
            skills: Some(Default::default()),
            ..Default::default()
        };
        assert!(
            crate::environments::sources::refresh(
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
