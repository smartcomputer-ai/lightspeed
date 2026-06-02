use engine::{CoreAgentCommand, CoreAgentState, SkillCatalogContext};
use temporalio_sdk::activities::ActivityError;
use tools::skills::{
    conventional_vfs_skill_root_specs, prepare_skill_catalog_publication,
    resolve_mounted_vfs_skill_roots,
};
use workflow::{SkillCatalogRefreshActivityRequest, SkillCatalogRefreshActivityResult};

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
            command: clear_catalog_command(request.active_catalog.as_ref()),
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
            command: clear_catalog_command(request.active_catalog.as_ref()),
        });
    }

    let mut state = CoreAgentState::new();
    state.skills.catalog = request.active_catalog;
    let publication = prepare_skill_catalog_publication(deps.blobs.as_ref(), &state, None, &inputs)
        .await
        .map_err(activity_error)?;
    Ok(SkillCatalogRefreshActivityResult {
        command: publication.command,
    })
}

fn clear_catalog_command(active_catalog: Option<&SkillCatalogContext>) -> Option<CoreAgentCommand> {
    active_catalog.map(|_| CoreAgentCommand::SetSkillCatalog { catalog: None })
}
