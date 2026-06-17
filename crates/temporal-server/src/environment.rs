//! Hosted session environment projection and runtime target owner.

use std::{collections::BTreeMap, sync::Arc};

use engine::{CoreAgentState, SessionId, ToolExecutionTarget, storage::BlobStore};
use thiserror::Error;
use tools::{
    environment::EnvironmentToolContext,
    environment::projection::{
        EnvironmentProjectionError, EnvironmentProjectionInput, EnvironmentProjectionRefresh,
        EnvironmentRecord, FsRoute, prepare_environment_projection_refresh,
    },
    fs::FsToolContext,
    targets::{ENV_TARGET_NAMESPACE, SESSION_FS_TARGET_ID, ToolTargets},
};
use vfs::{VfsCatalogError, VfsMountRecord, VfsMountStore};

#[derive(Clone)]
pub struct RuntimeEnvironment {
    record: EnvironmentRecord,
    tool_context: EnvironmentToolContext,
    fs_routes: Vec<FsRoute>,
}

impl RuntimeEnvironment {
    pub fn new(record: EnvironmentRecord, tool_context: EnvironmentToolContext) -> Self {
        Self {
            record,
            tool_context,
            fs_routes: Vec::new(),
        }
    }

    pub fn with_fs_routes(mut self, fs_routes: Vec<FsRoute>) -> Self {
        self.fs_routes = fs_routes;
        self
    }

    pub fn env_id(&self) -> &str {
        self.record.env_id.as_str()
    }

    pub fn record(&self) -> &EnvironmentRecord {
        &self.record
    }

    pub fn tool_context(&self) -> &EnvironmentToolContext {
        &self.tool_context
    }

    pub fn fs_routes(&self) -> &[FsRoute] {
        &self.fs_routes
    }
}

#[derive(Clone)]
pub struct SessionEnvironmentManager {
    blobs: Arc<dyn BlobStore>,
    mount_store: Arc<dyn VfsMountStore>,
    environments: BTreeMap<String, RuntimeEnvironment>,
}

impl SessionEnvironmentManager {
    pub fn new(blobs: Arc<dyn BlobStore>, mount_store: Arc<dyn VfsMountStore>) -> Self {
        Self {
            blobs,
            mount_store,
            environments: BTreeMap::new(),
        }
    }

    pub fn with_environment(mut self, environment: RuntimeEnvironment) -> Self {
        self.insert_environment(environment);
        self
    }

    pub fn insert_environment(&mut self, environment: RuntimeEnvironment) {
        self.environments
            .insert(environment.env_id().to_owned(), environment);
    }

    pub fn environment_records(&self) -> Vec<EnvironmentRecord> {
        self.environments
            .values()
            .map(|environment| environment.record.clone())
            .collect()
    }

    pub fn environments(&self) -> impl Iterator<Item = &RuntimeEnvironment> {
        self.environments.values()
    }

    pub fn environment(&self, env_id: &str) -> Option<&RuntimeEnvironment> {
        self.environments.get(env_id)
    }

    pub fn active_environment_id(&self, state: &CoreAgentState) -> Option<&str> {
        self.active_environment(state)
            .map(RuntimeEnvironment::env_id)
    }

    pub fn has_environments(&self) -> bool {
        !self.environments.is_empty()
    }

    pub fn tool_targets(&self, session_fs: Option<FsToolContext>) -> ToolTargets {
        let mut targets = ToolTargets::new();
        if let Some(session_fs) = session_fs {
            targets.insert_fs_context(SESSION_FS_TARGET_ID, session_fs);
        }
        for environment in self.environments.values() {
            targets
                .insert_environment_context(environment.env_id(), environment.tool_context.clone());
        }
        targets
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
        let active_environment = self.active_environment(state);
        let mut input = EnvironmentProjectionInput::from_mounts(mounts)
            .with_environments(self.environment_records());
        if let Some(environment) = active_environment {
            input = input
                .with_active_environment(environment.env_id(), environment.fs_routes().to_vec());
        }
        Ok(prepare_environment_projection_refresh(self.blobs.as_ref(), state, input).await?)
    }

    fn active_environment(&self, state: &CoreAgentState) -> Option<&RuntimeEnvironment> {
        let target = state
            .tooling
            .routing
            .default_targets
            .get(ENV_TARGET_NAMESPACE)?;
        active_environment_for_target(&self.environments, target)
    }
}

#[derive(Debug, Error)]
pub enum SessionEnvironmentManagerError {
    #[error(transparent)]
    Projection(#[from] EnvironmentProjectionError),

    #[error(transparent)]
    VfsCatalog(#[from] VfsCatalogError),
}

fn active_environment_for_target<'a>(
    environments: &'a BTreeMap<String, RuntimeEnvironment>,
    target: &ToolExecutionTarget,
) -> Option<&'a RuntimeEnvironment> {
    environments
        .values()
        .find(|environment| environment.record.exec_target.as_ref() == Some(target))
        .or_else(|| {
            environments.values().find(|environment| {
                target.namespace == ENV_TARGET_NAMESPACE && environment.record.env_id == target.id
            })
        })
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use engine::{ContextEntryKey, CoreAgentCommand, storage::InMemoryBlobStore};
    use tools::{
        environment::EnvironmentToolContext,
        environment::projection::{
            EnvironmentCapabilities, EnvironmentKind, EnvironmentStatus, FsRoute, FsRouteAccess,
            FsRouteSource,
        },
        fs::FsPath,
        targets::environment_target,
    };
    use vfs::{VfsCatalogError, VfsMountRecord};

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn manager_projects_active_environment_from_default_env_target() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let manager = SessionEnvironmentManager::new(blobs.clone(), Arc::new(EmptyMountStore))
            .with_environment(
                RuntimeEnvironment::new(
                    EnvironmentRecord {
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
                    },
                    EnvironmentToolContext::new(None, blobs),
                )
                .with_fs_routes(vec![FsRoute {
                    path: FsPath::new("/workspace").expect("route path"),
                    access: FsRouteAccess::ReadWrite,
                    source: FsRouteSource::HostFilesystem {
                        target: environment_target("local"),
                    },
                    same_state_as_active_env: Some("local".to_owned()),
                }]),
            );
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
        assert_eq!(
            refresh.environment_catalog.active_env_id.as_deref(),
            Some("local")
        );
        assert_eq!(refresh.environment_catalog.environments.len(), 1);
        assert_eq!(
            refresh
                .environment_active
                .as_ref()
                .map(|active| active.fs_routes.len()),
            Some(1)
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
