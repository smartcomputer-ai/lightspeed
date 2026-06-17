use engine::{
    BlobRef, ContextEntry, ContextEntryId, ContextEntryKey, ContextEntryKind, ContextEntrySource,
    CoreAgentCommand, CoreAgentState, ENVIRONMENT_ACTIVE_CONTEXT_KEY,
    ENVIRONMENT_CATALOG_CONTEXT_KEY, SKILL_CATALOG_CONTEXT_KEY, VFS_CATALOG_CONTEXT_KEY,
};
use temporal_workflow::{SkillCatalogRefreshActivityRequest, SkillCatalogRefreshActivityResult};
use temporalio_sdk::activities::ActivityError;
use tools::skills::{
    conventional_vfs_skill_root_specs, prepare_skill_catalog_publication,
    resolve_mounted_vfs_skill_roots, skill_catalog_context_input,
};

use crate::environment::SessionEnvironmentManager;

use super::{common::activity_error, state::SkillCatalogActivityDeps};

pub(super) async fn refresh_skill_catalog(
    deps: Option<&SkillCatalogActivityDeps>,
    request: SkillCatalogRefreshActivityRequest,
) -> Result<SkillCatalogRefreshActivityResult, ActivityError> {
    let Some(deps) = deps else {
        return Ok(SkillCatalogRefreshActivityResult {
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

    let manager = SessionEnvironmentManager::new(deps.blobs.clone(), deps.mount_store.clone());
    let mut commands = manager
        .refresh_projection_for_mounts(&state, mounts.clone())
        .await
        .map(|refresh| refresh.commands)
        .map_err(activity_error)?;

    let specs = conventional_vfs_skill_root_specs(&mounts);
    if specs.is_empty() {
        return Ok(SkillCatalogRefreshActivityResult {
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
        return Ok(SkillCatalogRefreshActivityResult {
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
    Ok(SkillCatalogRefreshActivityResult { commands })
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
