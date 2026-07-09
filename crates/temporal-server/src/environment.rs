//! Hosted session environment projection and runtime target owner.

use std::{collections::BTreeMap, sync::Arc};

use engine::{CoreAgentState, SessionId, ToolExecutionTarget, storage::BlobStore};
use environments::{
    EnvironmentInstanceRecord, SessionEnvironmentBindingRecord, SessionEnvironmentBindingState,
    SessionEnvironmentFsRoute, SessionEnvironmentFsRouteAccess,
};
use host_protocol::{control::targets::HostTargetStatus, shared::HostCapabilities};
use thiserror::Error;
use tools::{
    environment::EnvironmentToolContext,
    environment::projection::{
        EnvironmentCapabilities, EnvironmentKind, EnvironmentProjectionError,
        EnvironmentProjectionInput, EnvironmentProjectionRefresh, EnvironmentRecord,
        EnvironmentStatus, FsRoute, FsRouteAccess, FsRouteSource,
        prepare_environment_projection_refresh,
    },
    fs::{
        FileSystem, FsError, FsPath, FsToolContext, ScopedFileSystem, SessionFileSystem,
        SessionFileSystemRoute, SessionFileSystemRouteSource,
    },
    targets::{ENV_TARGET_NAMESPACE, SESSION_FS_TARGET_ID, ToolTargets},
};
use vfs::{VfsCatalogError, VfsMountRecord, VfsMountStore};

#[derive(Clone)]
pub struct RuntimeEnvironment {
    record: EnvironmentRecord,
    instance_id: Option<String>,
    binding_state: SessionEnvironmentBindingState,
    tool_context: EnvironmentToolContext,
    fs_context: Option<FsToolContext>,
    fs_routes: Vec<FsRoute>,
}

impl RuntimeEnvironment {
    pub fn new(record: EnvironmentRecord, tool_context: EnvironmentToolContext) -> Self {
        Self {
            record,
            instance_id: None,
            binding_state: SessionEnvironmentBindingState::Attached,
            tool_context,
            fs_context: None,
            fs_routes: Vec::new(),
        }
    }

    pub fn with_fs_context(mut self, fs_context: FsToolContext) -> Self {
        self.fs_context = Some(fs_context);
        self
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

    pub fn instance_id(&self) -> Option<&str> {
        self.instance_id.as_deref()
    }

    pub fn binding_state(&self) -> SessionEnvironmentBindingState {
        self.binding_state
    }

    pub fn tool_context(&self) -> &EnvironmentToolContext {
        &self.tool_context
    }

    pub fn fs_context(&self) -> Option<&FsToolContext> {
        self.fs_context.as_ref()
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

    pub fn active_environment_id_for<'a>(
        &self,
        environments: &'a [RuntimeEnvironment],
        state: &CoreAgentState,
    ) -> Option<&'a str> {
        active_environment_for_slice(environments, state).map(RuntimeEnvironment::env_id)
    }

    pub fn has_environments(&self) -> bool {
        !self.environments.is_empty()
    }

    pub fn has_process_environment(&self) -> bool {
        self.environments
            .values()
            .any(|environment| environment.record.capabilities.process_exec)
    }

    pub fn has_job_environment(&self) -> bool {
        self.environments.values().any(|environment| {
            let capabilities = &environment.record.capabilities;
            capabilities.job_start
                || capabilities.job_list
                || capabilities.job_read
                || capabilities.job_cancel
        })
    }

    pub fn tool_targets(
        &self,
        session_fs: Option<FsToolContext>,
        vfs_mounts: &[VfsMountRecord],
        active_env_target: Option<&ToolExecutionTarget>,
    ) -> Result<ToolTargets, SessionEnvironmentManagerError> {
        let mut targets = ToolTargets::new();
        if let Some(session_fs) =
            self.composed_session_fs(session_fs, vfs_mounts, active_env_target)?
        {
            targets.insert_fs_context(SESSION_FS_TARGET_ID, session_fs);
        }
        for environment in self.environments.values() {
            targets
                .insert_environment_context(environment.env_id(), environment.tool_context.clone());
        }
        Ok(targets)
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

    pub async fn refresh_projection_for_runtime_environments(
        &self,
        state: &CoreAgentState,
        mounts: Vec<VfsMountRecord>,
        environments: Vec<RuntimeEnvironment>,
    ) -> Result<EnvironmentProjectionRefresh, SessionEnvironmentManagerError> {
        let active_environment = active_environment_for_slice(&environments, state);
        let mut input = EnvironmentProjectionInput::from_mounts(mounts).with_environments(
            environments
                .iter()
                .map(|environment| environment.record.clone())
                .collect(),
        );
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

    fn composed_session_fs(
        &self,
        session_fs: Option<FsToolContext>,
        vfs_mounts: &[VfsMountRecord],
        active_env_target: Option<&ToolExecutionTarget>,
    ) -> Result<Option<FsToolContext>, SessionEnvironmentManagerError> {
        let active_environment = active_env_target
            .and_then(|target| active_environment_for_target(&self.environments, target))
            .filter(|environment| environment.fs_context().is_some())
            .filter(|environment| !environment.fs_routes().is_empty());
        let Some(active_environment) = active_environment else {
            return Ok(session_fs);
        };
        let active_fs = active_environment
            .fs_context()
            .expect("active environment fs context checked above");

        let mut routes = Vec::new();
        if let Some(session_fs) = session_fs.as_ref() {
            for mount in vfs_mounts {
                routes.push(vfs_session_route(mount, session_fs.fs.clone())?);
            }
        }
        for route in active_environment.fs_routes() {
            let FsRouteSource::HostFilesystem { .. } = &route.source else {
                continue;
            };
            let route_fs = scoped_route_fs(route, active_fs.fs.clone())?;
            routes.push(SessionFileSystemRoute::new(
                route.path.clone(),
                route_fs,
                SessionFileSystemRouteSource::EnvironmentFilesystem {
                    environment_id: active_environment.env_id().to_owned(),
                },
                route.same_state_as_active_env.as_deref() == Some(active_environment.env_id()),
            )?);
        }

        if routes.is_empty() {
            return Ok(session_fs);
        }

        let mut fs_context = FsToolContext::new(
            Arc::new(SessionFileSystem::new(routes)?),
            self.blobs.clone(),
        );
        if let Some(cwd) = active_fs
            .fs_cwd
            .clone()
            .or_else(|| session_fs.as_ref().and_then(|ctx| ctx.fs_cwd.clone()))
        {
            fs_context = fs_context.with_cwd(cwd);
        }
        Ok(Some(fs_context))
    }
}

fn vfs_session_route(
    mount: &VfsMountRecord,
    fs: Arc<dyn FileSystem>,
) -> Result<SessionFileSystemRoute, FsError> {
    let mount_path = FsPath::new(mount.mount_path.as_str())?;
    let route_fs = match mount.access {
        vfs::VfsMountAccess::ReadOnly => {
            ScopedFileSystem::read_only_from_arc(mount_path.clone(), fs)?
        }
        vfs::VfsMountAccess::ReadWrite => {
            ScopedFileSystem::read_write_from_arc(mount_path.clone(), fs)?
        }
    };
    Ok(SessionFileSystemRoute::new(
        mount_path,
        Arc::new(route_fs),
        match &mount.source {
            vfs::VfsMountSource::Snapshot { .. } => SessionFileSystemRouteSource::VfsSnapshot,
            vfs::VfsMountSource::Workspace { .. } => SessionFileSystemRouteSource::VfsWorkspace,
        },
        false,
    )?)
}

fn scoped_route_fs(
    route: &FsRoute,
    fs: Arc<dyn FileSystem>,
) -> Result<Arc<dyn FileSystem>, FsError> {
    let source_path = route.source_path.as_ref().unwrap_or(&route.path);
    let scoped = match route.access {
        FsRouteAccess::ReadOnly => ScopedFileSystem::read_only_from_arc(source_path.clone(), fs)?,
        FsRouteAccess::ReadWrite => ScopedFileSystem::read_write_from_arc(source_path.clone(), fs)?,
    };
    Ok(Arc::new(scoped))
}

fn active_environment_for_slice<'a>(
    environments: &'a [RuntimeEnvironment],
    state: &CoreAgentState,
) -> Option<&'a RuntimeEnvironment> {
    let target = state
        .tooling
        .routing
        .default_targets
        .get(ENV_TARGET_NAMESPACE)?;
    environments
        .iter()
        .find(|environment| environment.record.exec_target.as_ref() == Some(target))
        .or_else(|| {
            environments.iter().find(|environment| {
                target.namespace == ENV_TARGET_NAMESPACE && environment.record.env_id == target.id
            })
        })
}

#[derive(Debug, Error)]
pub enum SessionEnvironmentManagerError {
    #[error(transparent)]
    Projection(#[from] EnvironmentProjectionError),

    #[error(transparent)]
    VfsCatalog(#[from] VfsCatalogError),

    #[error(transparent)]
    Fs(#[from] FsError),
}

#[derive(Debug, Error)]
pub enum RuntimeEnvironmentBindingError {
    #[error("invalid environment binding cwd: {0}")]
    InvalidCwd(String),

    #[error("invalid environment binding fs route: {0}")]
    InvalidFsRoute(String),
}

pub fn runtime_environment_from_binding_record(
    binding: &SessionEnvironmentBindingRecord,
    instance: &EnvironmentInstanceRecord,
    tool_context: EnvironmentToolContext,
) -> Result<RuntimeEnvironment, RuntimeEnvironmentBindingError> {
    let record = environment_record_from_binding(binding, instance)?;
    let fs_routes = fs_routes_from_binding(binding)?;
    Ok(RuntimeEnvironment::new(record, tool_context)
        .with_instance_binding(instance.instance_id.as_str(), binding.state)
        .with_fs_routes(fs_routes))
}

impl RuntimeEnvironment {
    fn with_instance_binding(
        mut self,
        instance_id: &str,
        state: SessionEnvironmentBindingState,
    ) -> Self {
        self.instance_id = Some(instance_id.to_owned());
        self.binding_state = state;
        self
    }
}

pub fn environment_record_from_binding(
    binding: &SessionEnvironmentBindingRecord,
    instance: &EnvironmentInstanceRecord,
) -> Result<EnvironmentRecord, RuntimeEnvironmentBindingError> {
    Ok(EnvironmentRecord {
        env_id: binding.env_id.as_str().to_owned(),
        kind: EnvironmentKind::AttachedHost,
        capabilities: environment_capabilities_from_host(&instance.capabilities),
        exec_target: Some(binding.exec_target()),
        cwd: binding
            .cwd
            .as_ref()
            .map(|cwd| FsPath::new(cwd.as_str()))
            .transpose()
            .map_err(|error| RuntimeEnvironmentBindingError::InvalidCwd(error.to_string()))?,
        status: environment_status_from_binding(binding.state, instance.status),
    })
}

pub fn fs_routes_from_binding(
    binding: &SessionEnvironmentBindingRecord,
) -> Result<Vec<FsRoute>, RuntimeEnvironmentBindingError> {
    binding
        .fs_routes
        .iter()
        .map(|route| fs_route_from_binding(route, &binding.exec_target()))
        .collect()
}

fn fs_route_from_binding(
    route: &SessionEnvironmentFsRoute,
    target: &ToolExecutionTarget,
) -> Result<FsRoute, RuntimeEnvironmentBindingError> {
    Ok(FsRoute {
        path: FsPath::new(route.path.as_str())
            .map_err(|error| RuntimeEnvironmentBindingError::InvalidFsRoute(error.to_string()))?,
        source_path: route
            .source_path
            .as_ref()
            .map(|path| {
                FsPath::new(path.as_str()).map_err(|error| {
                    RuntimeEnvironmentBindingError::InvalidFsRoute(error.to_string())
                })
            })
            .transpose()?,
        access: match route.access {
            SessionEnvironmentFsRouteAccess::ReadOnly => FsRouteAccess::ReadOnly,
            SessionEnvironmentFsRouteAccess::ReadWrite => FsRouteAccess::ReadWrite,
        },
        source: FsRouteSource::HostFilesystem {
            target: target.clone(),
        },
        same_state_as_active_env: route
            .same_state_as_active_env
            .as_ref()
            .map(|env_id| env_id.as_str().to_owned()),
    })
}

fn environment_status_from_binding(
    state: SessionEnvironmentBindingState,
    status: HostTargetStatus,
) -> EnvironmentStatus {
    if state == SessionEnvironmentBindingState::Detached {
        return EnvironmentStatus::Detached;
    }
    match status {
        HostTargetStatus::Ready => EnvironmentStatus::Ready,
        HostTargetStatus::Creating | HostTargetStatus::Starting => EnvironmentStatus::Attaching,
        HostTargetStatus::Stopped
        | HostTargetStatus::Closing
        | HostTargetStatus::Closed
        | HostTargetStatus::Failed
        | HostTargetStatus::Unknown => EnvironmentStatus::Degraded,
    }
}

fn environment_capabilities_from_host(capabilities: &HostCapabilities) -> EnvironmentCapabilities {
    EnvironmentCapabilities {
        fs_read: capabilities.filesystem_read,
        fs_write: capabilities.filesystem_write,
        process_exec: capabilities.process_start,
        process_stdin: capabilities.process_stdin,
        job_start: capabilities.job_start,
        job_list: capabilities.job_list,
        job_read: capabilities.job_read,
        job_cancel: capabilities.job_cancel,
        job_wait_hint: capabilities.job_wait_hint,
        job_dependencies: capabilities.job_dependencies,
        job_queue_keys: capabilities.job_queue_keys,
        network: capabilities.network,
        persistent: true,
    }
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
    use engine::{
        BlobRef, ContextEntryKey, CoreAgentCommand, SessionId, storage::InMemoryBlobStore,
    };
    use tools::{
        environment::EnvironmentToolContext,
        environment::projection::{
            EnvironmentCapabilities, EnvironmentKind, EnvironmentStatus, FsRoute, FsRouteAccess,
            FsRouteSource,
        },
        fs::{CreateDirectoryOptions, FileSystem, FsPath, FsToolContext, InMemoryFileSystem},
        targets::environment_target,
    };
    use vfs::{VfsCatalogError, VfsMountAccess, VfsMountRecord, VfsMountSource, VfsPath};

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
                            ..EnvironmentCapabilities::default()
                        },
                        exec_target: Some(environment_target("local")),
                        cwd: Some(FsPath::new("/workspace").expect("cwd")),
                        status: EnvironmentStatus::Ready,
                    },
                    EnvironmentToolContext::new(None, blobs),
                )
                .with_fs_routes(vec![FsRoute {
                    path: FsPath::new("/workspace").expect("route path"),
                    source_path: None,
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

    #[tokio::test(flavor = "current_thread")]
    async fn manager_composes_active_environment_filesystem_into_session_target() {
        let blobs = Arc::new(InMemoryBlobStore::new());
        let fs = InMemoryFileSystem::full_access();
        fs.create_directory(
            &FsPath::new("/workspace").expect("workspace path"),
            CreateDirectoryOptions::single(),
        )
        .await
        .expect("create workspace");
        fs.write_file(
            &FsPath::new("/workspace/README.md").expect("file path"),
            b"from environment".to_vec(),
        )
        .await
        .expect("write file");
        let fs_context = FsToolContext::new(Arc::new(fs), blobs.clone())
            .with_cwd(FsPath::new("/workspace").expect("cwd"));
        let manager = SessionEnvironmentManager::new(blobs.clone(), Arc::new(EmptyMountStore))
            .with_environment(
                RuntimeEnvironment::new(
                    EnvironmentRecord {
                        env_id: "local".to_owned(),
                        kind: EnvironmentKind::AttachedHost,
                        capabilities: EnvironmentCapabilities {
                            fs_read: true,
                            fs_write: true,
                            process_exec: false,
                            process_stdin: false,
                            network: false,
                            persistent: true,
                            ..EnvironmentCapabilities::default()
                        },
                        exec_target: Some(environment_target("local")),
                        cwd: Some(FsPath::new("/workspace").expect("cwd")),
                        status: EnvironmentStatus::Ready,
                    },
                    EnvironmentToolContext::new(None, blobs.clone()),
                )
                .with_fs_context(fs_context)
                .with_fs_routes(vec![FsRoute {
                    path: FsPath::new("/workspace").expect("route path"),
                    source_path: None,
                    access: FsRouteAccess::ReadWrite,
                    source: FsRouteSource::HostFilesystem {
                        target: environment_target("local"),
                    },
                    same_state_as_active_env: Some("local".to_owned()),
                }]),
            );

        let targets = manager
            .tool_targets(None, &[], Some(&environment_target("local")))
            .expect("tool targets");
        let ctx = targets
            .resolve(&tools::targets::session_fs_target())
            .expect("session fs target")
            .filesystem()
            .expect("filesystem context");
        let contents = ctx
            .fs
            .read_file(&FsPath::new("/workspace/README.md").expect("file path"))
            .await
            .expect("read file");

        assert_eq!(contents, b"from environment");
        assert_eq!(ctx.fs_cwd.as_ref().map(FsPath::as_str), Some("/workspace"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manager_composes_vfs_routes_before_active_environment_root() {
        let blobs = Arc::new(InMemoryBlobStore::new());

        let session_vfs = InMemoryFileSystem::full_access();
        session_vfs
            .create_directory(
                &FsPath::new("/skills").expect("skills path"),
                CreateDirectoryOptions::single(),
            )
            .await
            .expect("create skills");
        session_vfs
            .write_file(
                &FsPath::new("/skills/SKILL.md").expect("skill path"),
                b"from vfs".to_vec(),
            )
            .await
            .expect("write vfs skill");
        let session_fs = FsToolContext::new(Arc::new(session_vfs), blobs.clone())
            .with_cwd(FsPath::new("/skills").expect("vfs cwd"));
        let mounts = vec![VfsMountRecord {
            session_id: SessionId::new("session-vfs-first"),
            mount_path: VfsPath::parse("/skills").expect("mount path"),
            source: VfsMountSource::Snapshot {
                snapshot_ref: BlobRef::from_bytes(b"skills"),
            },
            access: VfsMountAccess::ReadOnly,
        }];

        let env_fs = InMemoryFileSystem::full_access();
        env_fs
            .create_directory(
                &FsPath::new("/host/workspace/skills").expect("env skills path"),
                CreateDirectoryOptions::recursive(),
            )
            .await
            .expect("create env skills");
        env_fs
            .write_file(
                &FsPath::new("/host/workspace/skills/SKILL.md").expect("env skill path"),
                b"from environment".to_vec(),
            )
            .await
            .expect("write env skill");
        env_fs
            .create_directory(
                &FsPath::new("/host/workspace/repo").expect("repo path"),
                CreateDirectoryOptions::single(),
            )
            .await
            .expect("create repo");
        env_fs
            .write_file(
                &FsPath::new("/host/workspace/repo/Cargo.toml").expect("repo file"),
                b"from environment repo".to_vec(),
            )
            .await
            .expect("write repo file");
        let env_fs_context = FsToolContext::new(Arc::new(env_fs), blobs.clone())
            .with_cwd(FsPath::new("/repo").expect("env cwd"));
        let manager = SessionEnvironmentManager::new(blobs.clone(), Arc::new(EmptyMountStore))
            .with_environment(
                RuntimeEnvironment::new(
                    EnvironmentRecord {
                        env_id: "local".to_owned(),
                        kind: EnvironmentKind::AttachedHost,
                        capabilities: EnvironmentCapabilities {
                            fs_read: true,
                            fs_write: true,
                            process_exec: false,
                            process_stdin: false,
                            network: false,
                            persistent: true,
                            ..EnvironmentCapabilities::default()
                        },
                        exec_target: Some(environment_target("local")),
                        cwd: Some(FsPath::new("/repo").expect("cwd")),
                        status: EnvironmentStatus::Ready,
                    },
                    EnvironmentToolContext::new(None, blobs.clone()),
                )
                .with_fs_context(env_fs_context)
                .with_fs_routes(vec![FsRoute {
                    path: FsPath::root(),
                    source_path: Some(FsPath::new("/host/workspace").expect("source path")),
                    access: FsRouteAccess::ReadWrite,
                    source: FsRouteSource::HostFilesystem {
                        target: environment_target("local"),
                    },
                    same_state_as_active_env: Some("local".to_owned()),
                }]),
            );

        let targets = manager
            .tool_targets(
                Some(session_fs),
                &mounts,
                Some(&environment_target("local")),
            )
            .expect("tool targets");
        let ctx = targets
            .resolve(&tools::targets::session_fs_target())
            .expect("session fs target")
            .filesystem()
            .expect("filesystem context");

        assert_eq!(
            ctx.fs
                .read_file_text(&FsPath::new("/skills/SKILL.md").expect("skill path"))
                .await
                .expect("read skill"),
            "from vfs"
        );
        assert_eq!(
            ctx.fs
                .read_file_text(&FsPath::new("/repo/Cargo.toml").expect("repo file"))
                .await
                .expect("read repo"),
            "from environment repo"
        );
        let root_entries = ctx
            .fs
            .read_directory(&FsPath::root())
            .await
            .expect("read root directory")
            .into_iter()
            .map(|entry| entry.file_name)
            .collect::<Vec<_>>();
        assert!(root_entries.contains(&"repo".to_owned()));
        assert!(root_entries.contains(&"skills".to_owned()));
        assert_eq!(ctx.fs_cwd.as_ref().map(FsPath::as_str), Some("/repo"));
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
