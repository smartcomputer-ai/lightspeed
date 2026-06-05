use engine::{
    BlobRef, ContextEntry, ContextEntryId, ContextEntryKey, ContextEntryKind, ContextEntrySource,
    CoreAgentCommand, CoreAgentState, SKILL_CATALOG_CONTEXT_KEY,
};
use temporal_workflow::{SkillCatalogRefreshActivityRequest, SkillCatalogRefreshActivityResult};
use temporalio_sdk::activities::ActivityError;
use tools::skills::{
    conventional_vfs_skill_root_specs, prepare_skill_catalog_publication,
    resolve_mounted_vfs_skill_roots, skill_catalog_context_input,
};

use super::{common::activity_error, state::SkillCatalogActivityDeps};

pub(super) async fn refresh_skill_catalog(
    deps: Option<&SkillCatalogActivityDeps>,
    request: SkillCatalogRefreshActivityRequest,
) -> Result<SkillCatalogRefreshActivityResult, ActivityError> {
    let Some(deps) = deps else {
        return Ok(SkillCatalogRefreshActivityResult { command: None });
    };

    let mounts = deps
        .mount_store
        .list_mounts(&request.session_id)
        .await
        .map_err(activity_error)?;
    let specs = conventional_vfs_skill_root_specs(&mounts);
    if specs.is_empty() {
        return Ok(SkillCatalogRefreshActivityResult {
            command: clear_catalog_command(request.active_catalog_ref.as_ref()),
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
            command: clear_catalog_command(request.active_catalog_ref.as_ref()),
        });
    }

    let mut state = CoreAgentState::new();
    if let Some(catalog_ref) = request.active_catalog_ref.clone() {
        state.context.entries = vec![active_catalog_entry(catalog_ref)];
    }
    let publication = prepare_skill_catalog_publication(deps.blobs.as_ref(), &state, None, &inputs)
        .await
        .map_err(activity_error)?;
    Ok(SkillCatalogRefreshActivityResult {
        command: publication.command,
    })
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
