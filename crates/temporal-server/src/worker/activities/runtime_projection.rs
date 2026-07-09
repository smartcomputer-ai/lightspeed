use engine::{
    BlobRef, ContextEntry, ContextEntryId, ContextEntryKey, ContextEntryKind, ContextEntrySource,
    CoreAgentCommand, CoreAgentState, ENVIRONMENT_ACTIVE_CONTEXT_KEY,
    ENVIRONMENT_CATALOG_CONTEXT_KEY, SKILL_CATALOG_CONTEXT_KEY, VFS_CATALOG_CONTEXT_KEY,
};
use temporal_workflow::{
    RuntimeProjectionRefreshActivityRequest, RuntimeProjectionRefreshActivityResult,
};
use temporalio_sdk::activities::ActivityError;
use tools::{
    environment::EnvironmentToolContext,
    skills::{
        conventional_vfs_skill_root_specs, prepare_skill_catalog_publication,
        resolve_mounted_vfs_skill_roots, skill_catalog_context_input,
    },
    targets::ENV_TARGET_NAMESPACE,
};

use crate::environment::{SessionEnvironmentManager, runtime_environment_from_binding_record};

use super::{common::activity_error, state::RuntimeProjectionActivityDeps};

pub(super) async fn refresh_runtime_projection(
    deps: Option<&RuntimeProjectionActivityDeps>,
    request: RuntimeProjectionRefreshActivityRequest,
) -> Result<RuntimeProjectionRefreshActivityResult, ActivityError> {
    let Some(deps) = deps else {
        return Ok(RuntimeProjectionRefreshActivityResult {
            commands: Vec::new(),
        });
    };

    let mounts = deps
        .mount_store
        .list_mounts(&request.session_id)
        .await
        .map_err(activity_error)?;
    let mut state = CoreAgentState::new();
    if let Some(catalog_ref) = request.active_catalog_ref.clone() {
        state
            .context
            .entries
            .push(active_catalog_entry(catalog_ref));
    }
    if let Some(catalog_ref) = request.active_vfs_catalog_ref.clone() {
        state
            .context
            .entries
            .push(active_vfs_catalog_entry(catalog_ref));
    }
    if let Some(catalog_ref) = request.active_environment_catalog_ref.clone() {
        state
            .context
            .entries
            .push(active_environment_catalog_entry(catalog_ref));
    }
    if let Some(active_ref) = request.active_environment_active_ref.clone() {
        state
            .context
            .entries
            .push(active_environment_active_entry(active_ref));
    }
    if let Some(target) = request.active_environment_target.clone() {
        state
            .tooling
            .routing
            .default_targets
            .insert(ENV_TARGET_NAMESPACE.to_owned(), target);
    }

    let bindings = ::environments::SessionEnvironmentBindingStore::list_bindings_for_session(
        deps.environment_bindings.as_ref(),
        &request.session_id,
    )
    .await
    .map_err(activity_error)?
    .into_iter();
    let mut environments = Vec::new();
    for binding in bindings {
        let instance = ::environments::EnvironmentInstanceStore::read_instance(
            deps.environment_instances.as_ref(),
            &binding.instance_id,
        )
        .await
        .map_err(activity_error)?;
        let tool_context = EnvironmentToolContext::new(None, deps.blobs.clone())
            .with_session_id(binding.session_id.as_str());
        environments.push(
            runtime_environment_from_binding_record(&binding, &instance, tool_context)
                .map_err(activity_error)?,
        );
    }
    let manager = SessionEnvironmentManager::new(deps.blobs.clone(), deps.mount_store.clone());
    let mut commands = manager
        .refresh_projection_for_runtime_environments(&state, mounts.clone(), environments)
        .await
        .map(|refresh| refresh.commands)
        .map_err(activity_error)?;

    let specs = conventional_vfs_skill_root_specs(&mounts);
    if specs.is_empty() {
        return Ok(RuntimeProjectionRefreshActivityResult {
            commands: append_optional(
                commands,
                clear_catalog_command(request.active_catalog_ref.as_ref()),
            ),
        });
    }

    let resolved = resolve_mounted_vfs_skill_roots(
        deps.blobs.clone(),
        deps.workspace_store.clone(),
        mounts,
        specs,
    )
    .await
    .map_err(activity_error)?;
    let inputs = resolved
        .existing_directory_inputs()
        .await
        .map_err(activity_error)?;
    if inputs.is_empty() {
        return Ok(RuntimeProjectionRefreshActivityResult {
            commands: append_optional(
                commands,
                clear_catalog_command(request.active_catalog_ref.as_ref()),
            ),
        });
    }

    let publication = prepare_skill_catalog_publication(deps.blobs.as_ref(), &state, None, &inputs)
        .await
        .map_err(activity_error)?;
    if let Some(command) = publication.command {
        commands.push(command);
    }
    Ok(RuntimeProjectionRefreshActivityResult { commands })
}

fn append_optional(
    mut commands: Vec<CoreAgentCommand>,
    command: Option<CoreAgentCommand>,
) -> Vec<CoreAgentCommand> {
    if let Some(command) = command {
        commands.push(command);
    }
    commands
}

fn clear_catalog_command(active_catalog_ref: Option<&BlobRef>) -> Option<CoreAgentCommand> {
    active_catalog_ref.map(|_| CoreAgentCommand::RemoveContext {
        key: ContextEntryKey::new(SKILL_CATALOG_CONTEXT_KEY),
    })
}

fn active_catalog_entry(catalog_ref: BlobRef) -> ContextEntry {
    let input = skill_catalog_context_input(catalog_ref);
    ContextEntry {
        entry_id: ContextEntryId::new(1),
        key: Some(ContextEntryKey::new(SKILL_CATALOG_CONTEXT_KEY)),
        kind: ContextEntryKind::SkillCatalog,
        source: ContextEntrySource::Runtime {
            label: "skills.catalog".to_owned(),
        },
        content_ref: input.content_ref,
        media_type: input.media_type,
        preview: input.preview,
        provider_kind: input.provider_kind,
        provider_item_id: input.provider_item_id,
        token_estimate: input.token_estimate,
    }
}

fn active_vfs_catalog_entry(catalog_ref: BlobRef) -> ContextEntry {
    let input = tools::environment::projection::vfs_catalog_context_input(catalog_ref);
    active_projection_entry(
        ContextEntryKey::new(VFS_CATALOG_CONTEXT_KEY),
        ContextEntryKind::VfsCatalog,
        input,
        "environment.vfs_catalog",
    )
}

fn active_environment_catalog_entry(catalog_ref: BlobRef) -> ContextEntry {
    let input = tools::environment::projection::environment_catalog_context_input(catalog_ref);
    active_projection_entry(
        ContextEntryKey::new(ENVIRONMENT_CATALOG_CONTEXT_KEY),
        ContextEntryKind::EnvironmentCatalog,
        input,
        "environment.catalog",
    )
}

fn active_environment_active_entry(active_ref: BlobRef) -> ContextEntry {
    let input = tools::environment::projection::environment_active_context_input(active_ref);
    active_projection_entry(
        ContextEntryKey::new(ENVIRONMENT_ACTIVE_CONTEXT_KEY),
        ContextEntryKind::EnvironmentActive,
        input,
        "environment.active",
    )
}

fn active_projection_entry(
    key: ContextEntryKey,
    kind: ContextEntryKind,
    input: engine::ContextEntryInput,
    label: &'static str,
) -> ContextEntry {
    ContextEntry {
        entry_id: ContextEntryId::new(1),
        key: Some(key),
        kind,
        source: ContextEntrySource::Runtime {
            label: label.to_owned(),
        },
        content_ref: input.content_ref,
        media_type: input.media_type,
        preview: input.preview,
        provider_kind: input.provider_kind,
        provider_item_id: input.provider_item_id,
        token_estimate: input.token_estimate,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use engine::{
        SessionId, ToolExecutionTarget,
        storage::{BlobStore, InMemoryBlobStore},
    };
    use environments::{
        EnvironmentId, EnvironmentInstanceId, EnvironmentInstanceOrigin, EnvironmentInstanceStore,
        EnvironmentProviderCapabilities, EnvironmentProviderId, EnvironmentProviderKind,
        EnvironmentProviderStore, HostControllerConnectionSpec, InMemoryEnvironmentRegistryStore,
        ObserveEnvironmentInstance, PutSessionEnvironmentBinding, RegisterEnvironmentProvider,
        SessionEnvironmentBindingStore, SessionEnvironmentFsRoute, SessionEnvironmentFsRouteAccess,
    };
    use host_protocol::shared::{
        HostCapabilities, HostConnectionSpec, HostPath, HostScope, HostTargetId, HostTransport,
        ImplementationInfo,
    };
    use tools::environment::projection::{EnvironmentActive, EnvironmentCatalogSnapshot};
    use vfs::{
        CompareAndSetVfsWorkspaceHead, CreateVfsWorkspaceRecord, VfsCatalogError, VfsMountRecord,
        VfsMountStore, VfsPath, VfsWorkspaceId, VfsWorkspaceRecord, VfsWorkspaceStore,
    };

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn runtime_projection_refresh_preserves_bound_active_environment_projection() {
        let blobs: Arc<dyn BlobStore> = Arc::new(InMemoryBlobStore::new());
        let vfs = Arc::new(EmptyVfsStore);
        let bindings = Arc::new(InMemoryEnvironmentRegistryStore::new());
        bindings
            .register_provider(RegisterEnvironmentProvider {
                provider_id: EnvironmentProviderId::new("bridge-local"),
                provider_kind: EnvironmentProviderKind::Bridge,
                display_name: None,
                controller_connection: HostControllerConnectionSpec::new(
                    "ws://127.0.0.1:9001/controller",
                    HostTransport::WebSocket,
                ),
                capabilities: EnvironmentProviderCapabilities {
                    get_target: true,
                    ..EnvironmentProviderCapabilities::default()
                },
                implementation: ImplementationInfo {
                    name: "test".to_owned(),
                    version: None,
                },
                lease_ttl_ms: 60_000,
                metadata: Default::default(),
                observed_at_ms: 1,
            })
            .await
            .expect("create provider");
        bindings
            .observe_instance(test_instance("session_1"))
            .await
            .expect("create instance");
        bindings
            .put_binding(test_binding("session_1", "devbox"))
            .await
            .expect("create binding");
        let deps = RuntimeProjectionActivityDeps {
            blobs: blobs.clone(),
            workspace_store: vfs.clone(),
            mount_store: vfs,
            environment_bindings: bindings.clone(),
            environment_instances: bindings,
        };

        let result = refresh_runtime_projection(
            Some(&deps),
            RuntimeProjectionRefreshActivityRequest {
                session_id: SessionId::new("session_1"),
                active_catalog_ref: None,
                active_vfs_catalog_ref: None,
                active_environment_catalog_ref: None,
                active_environment_active_ref: None,
                active_environment_target: Some(ToolExecutionTarget::new("env", "devbox")),
            },
        )
        .await
        .expect("refresh skill catalog");

        let catalog_ref = result
            .commands
            .iter()
            .find_map(|command| match command {
                CoreAgentCommand::UpsertContext { key, entry }
                    if key.as_str() == ENVIRONMENT_CATALOG_CONTEXT_KEY =>
                {
                    Some(entry.content_ref.clone())
                }
                _ => None,
            })
            .expect("environment catalog command");
        let catalog: EnvironmentCatalogSnapshot =
            serde_json::from_slice(&blobs.read_bytes(&catalog_ref).await.expect("catalog blob"))
                .expect("catalog json");
        assert_eq!(catalog.active_env_id.as_deref(), Some("devbox"));
        assert_eq!(catalog.environments.len(), 1);
        assert_eq!(catalog.environments[0].env_id, "devbox");
        assert!(catalog.environments[0].capabilities.process_exec);

        let active_ref = result
            .commands
            .iter()
            .find_map(|command| match command {
                CoreAgentCommand::UpsertContext { key, entry }
                    if key.as_str() == ENVIRONMENT_ACTIVE_CONTEXT_KEY =>
                {
                    Some(entry.content_ref.clone())
                }
                _ => None,
            })
            .expect("active environment command");
        let active: EnvironmentActive =
            serde_json::from_slice(&blobs.read_bytes(&active_ref).await.expect("active blob"))
                .expect("active json");
        assert_eq!(active.env_id, "devbox");
    }

    fn test_binding(session_id: &str, env_id: &str) -> PutSessionEnvironmentBinding {
        PutSessionEnvironmentBinding {
            session_id: SessionId::new(session_id),
            env_id: EnvironmentId::new(env_id),
            instance_id: EnvironmentInstanceId::new("evi-local"),
            cwd: Some(HostPath::new("/workspace").expect("cwd")),
            fs_routes: vec![SessionEnvironmentFsRoute {
                path: HostPath::root(),
                source_path: None,
                access: SessionEnvironmentFsRouteAccess::ReadWrite,
                same_state_as_active_env: Some(EnvironmentId::new(env_id)),
            }],
            updated_at_ms: 1,
        }
    }

    fn test_instance(session_id: &str) -> ObserveEnvironmentInstance {
        ObserveEnvironmentInstance {
            instance_id: EnvironmentInstanceId::new("evi-local"),
            provider_id: EnvironmentProviderId::new("bridge-local"),
            provider_target_id: HostTargetId::new("local-host"),
            origin: EnvironmentInstanceOrigin::Provided,
            display_name: None,
            status: host_protocol::control::targets::HostTargetStatus::Ready,
            scope: HostScope::Session {
                session_id: session_id.to_owned(),
            },
            capabilities: HostCapabilities::filesystem(true, true).with_process(),
            connection: HostConnectionSpec {
                target_id: HostTargetId::new("local-host"),
                endpoint: "ws://127.0.0.1:9001/data".to_owned(),
                transport: HostTransport::WebSocket,
                scope: HostScope::Session {
                    session_id: session_id.to_owned(),
                },
                default_cwd: Some(HostPath::new("/workspace").expect("cwd")),
                capabilities: HostCapabilities::filesystem(true, true).with_process(),
            },
            default_cwd: Some(HostPath::new("/workspace").expect("cwd")),
            metadata: Default::default(),
            observed_at_ms: 1,
        }
    }

    struct EmptyVfsStore;

    #[async_trait]
    impl VfsMountStore for EmptyVfsStore {
        async fn put_mount(&self, _record: VfsMountRecord) -> Result<(), VfsCatalogError> {
            Ok(())
        }

        async fn list_mounts(
            &self,
            _session_id: &SessionId,
        ) -> Result<Vec<VfsMountRecord>, VfsCatalogError> {
            Ok(Vec::new())
        }

        async fn remove_mount(
            &self,
            _session_id: &SessionId,
            _mount_path: &VfsPath,
        ) -> Result<(), VfsCatalogError> {
            Ok(())
        }
    }

    #[async_trait]
    impl VfsWorkspaceStore for EmptyVfsStore {
        async fn create_workspace(
            &self,
            record: CreateVfsWorkspaceRecord,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            Ok(VfsWorkspaceRecord {
                workspace_id: record.workspace_id,
                display_name: record.display_name,
                base_snapshot_ref: record.base_snapshot_ref,
                head_snapshot_ref: record.head_snapshot_ref,
                head_totals: record.head_totals,
                revision: 0,
                created_at_ms: record.created_at_ms,
                updated_at_ms: record.created_at_ms,
            })
        }

        async fn read_workspace(
            &self,
            workspace_id: &VfsWorkspaceId,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            Err(VfsCatalogError::NotFound {
                kind: "workspace",
                id: workspace_id.to_string(),
            })
        }

        async fn list_workspaces(&self) -> Result<Vec<VfsWorkspaceRecord>, VfsCatalogError> {
            Ok(Vec::new())
        }

        async fn compare_and_set_head(
            &self,
            request: CompareAndSetVfsWorkspaceHead,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            Err(VfsCatalogError::NotFound {
                kind: "workspace",
                id: request.workspace_id.to_string(),
            })
        }

        async fn delete_workspace(
            &self,
            workspace_id: &VfsWorkspaceId,
        ) -> Result<VfsWorkspaceRecord, VfsCatalogError> {
            Err(VfsCatalogError::NotFound {
                kind: "workspace",
                id: workspace_id.to_string(),
            })
        }
    }
}
