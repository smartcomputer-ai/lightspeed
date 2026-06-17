//! Hosted session environment projection owner.

use std::sync::Arc;

use engine::{CoreAgentState, SessionId, ToolExecutionTarget, storage::BlobStore};
use thiserror::Error;
use tools::{
    environment::projection::{
        EnvironmentProjectionError, EnvironmentProjectionInput, EnvironmentProjectionRefresh,
        EnvironmentRecord, FsRoute, prepare_environment_projection_refresh,
    },
    targets::ENV_TARGET_NAMESPACE,
};
use vfs::{VfsCatalogError, VfsMountRecord, VfsMountStore};

#[derive(Clone)]
pub struct SessionEnvironmentManager {
    blobs: Arc<dyn BlobStore>,
    mount_store: Arc<dyn VfsMountStore>,
    environments: Vec<EnvironmentRecord>,
    active_fs_routes: Vec<FsRoute>,
}

impl SessionEnvironmentManager {
    pub fn new(blobs: Arc<dyn BlobStore>, mount_store: Arc<dyn VfsMountStore>) -> Self {
        Self {
            blobs,
            mount_store,
            environments: Vec::new(),
            active_fs_routes: Vec::new(),
        }
    }

    pub fn with_environments(mut self, environments: Vec<EnvironmentRecord>) -> Self {
        self.environments = environments;
        self
    }

    pub fn with_active_fs_routes(mut self, fs_routes: Vec<FsRoute>) -> Self {
        self.active_fs_routes = fs_routes;
        self
    }

    pub async fn refresh_projection(
        &self,
        session_id: &SessionId,
        state: &CoreAgentState,
    ) -> Result<EnvironmentProjectionRefresh, SessionEnvironmentManagerError> {
        let mounts = self.mount_store.list_mounts(session_id).await?;
        self.refresh_projection_for_mounts(state, mounts).await
    }

    pub async fn refresh_projection_for_mounts(
        &self,
        state: &CoreAgentState,
        mounts: Vec<VfsMountRecord>,
    ) -> Result<EnvironmentProjectionRefresh, SessionEnvironmentManagerError> {
        let active_env_id = self.active_environment_id(state);
        let active_fs_routes = active_env_id
            .as_ref()
            .map(|_| self.active_fs_routes.clone())
            .unwrap_or_default();
        let mut input = EnvironmentProjectionInput::from_mounts(mounts)
            .with_environments(self.environments.clone());
        if let Some(env_id) = active_env_id {
            input = input.with_active_environment(env_id, active_fs_routes);
        }
        Ok(prepare_environment_projection_refresh(self.blobs.as_ref(), state, input).await?)
    }

    fn active_environment_id(&self, state: &CoreAgentState) -> Option<String> {
        let target = state
            .tooling
            .routing
            .default_targets
            .get(ENV_TARGET_NAMESPACE)?;
        active_environment_id_for_target(&self.environments, target)
    }
}

#[derive(Debug, Error)]
pub enum SessionEnvironmentManagerError {
    #[error(transparent)]
    Projection(#[from] EnvironmentProjectionError),

    #[error(transparent)]
    VfsCatalog(#[from] VfsCatalogError),
}

fn active_environment_id_for_target(
    environments: &[EnvironmentRecord],
    target: &ToolExecutionTarget,
) -> Option<String> {
    environments
        .iter()
        .find(|record| record.exec_target.as_ref() == Some(target))
        .or_else(|| {
            environments.iter().find(|record| {
                target.namespace == ENV_TARGET_NAMESPACE && record.env_id == target.id
            })
        })
        .map(|record| record.env_id.clone())
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use engine::{ContextEntryKey, CoreAgentCommand, storage::InMemoryBlobStore};
    use tools::{
        environment::projection::{EnvironmentCapabilities, EnvironmentKind, EnvironmentStatus},
        fs::FsPath,
        targets::environment_target,
    };
    use vfs::{VfsCatalogError, VfsMountRecord};

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn manager_projects_active_environment_from_default_env_target() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let manager = SessionEnvironmentManager::new(blobs, Arc::new(EmptyMountStore))
            .with_environments(vec![EnvironmentRecord {
                env_id: "local".to_owned(),
                kind: EnvironmentKind::AttachedHost,
                capabilities: EnvironmentCapabilities {
                    fs_read: true,
                    fs_write: true,
                    process_exec: true,
                    process_stdin: true,
                    network: false,
                    persistent: true,
                },
                exec_target: Some(environment_target("local")),
                cwd: Some(FsPath::new("/workspace").expect("cwd")),
                status: EnvironmentStatus::Ready,
            }]);
        let mut state = CoreAgentState::new();
        state.tooling.routing.default_targets.insert(
            tools::targets::ENV_TARGET_NAMESPACE.to_owned(),
            environment_target("local"),
        );

        let refresh = manager
            .refresh_projection_for_mounts(&state, Vec::new())
            .await
            .expect("refresh projection");

        assert_eq!(
            refresh
                .environment_active
                .as_ref()
                .map(|active| active.env_id.as_str()),
            Some("local")
        );
        assert!(refresh.commands.iter().any(|command| matches!(
            command,
            CoreAgentCommand::UpsertContext { key, .. }
                if key == &ContextEntryKey::new(engine::ENVIRONMENT_ACTIVE_CONTEXT_KEY)
        )));
    }

    struct EmptyMountStore;

    #[async_trait]
    impl VfsMountStore for EmptyMountStore {
        async fn list_mounts(
            &self,
            _session_id: &SessionId,
        ) -> Result<Vec<VfsMountRecord>, VfsCatalogError> {
            Ok(Vec::new())
        }

        async fn put_mount(&self, _record: VfsMountRecord) -> Result<(), VfsCatalogError> {
            Ok(())
        }

        async fn remove_mount(
            &self,
            _session_id: &SessionId,
            _mount_path: &vfs::VfsPath,
        ) -> Result<(), VfsCatalogError> {
            Ok(())
        }
    }
}
