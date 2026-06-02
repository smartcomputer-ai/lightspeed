use engine::{CoreAgentCommand, CoreAgentState, SkillCatalogContext, storage::BlobStore};
use temporalio_sdk::activities::ActivityError;
use tools::skills::{
    SkillCatalogSnapshot, SkillToolResultActivationInput, conventional_vfs_skill_root_specs,
    prepare_skill_catalog_publication, resolve_mounted_vfs_skill_roots,
    skill_activation_from_tool_result,
};
use workflow::{
    SkillActivationRefreshActivityRequest, SkillActivationRefreshActivityResult,
    SkillCatalogRefreshActivityRequest, SkillCatalogRefreshActivityResult,
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

pub(super) async fn refresh_skill_activations(
    deps: &SkillCatalogActivityDeps,
    request: SkillActivationRefreshActivityRequest,
) -> Result<SkillActivationRefreshActivityResult, ActivityError> {
    let state = request.state;
    let Some(command) = skill_activation_command_for_active_tool_batch(deps, &state).await? else {
        return Ok(SkillActivationRefreshActivityResult { command: None });
    };
    Ok(SkillActivationRefreshActivityResult {
        command: Some(command),
    })
}

async fn skill_activation_command_for_active_tool_batch(
    deps: &SkillCatalogActivityDeps,
    state: &CoreAgentState,
) -> Result<Option<CoreAgentCommand>, ActivityError> {
    let Some(catalog_context) = state.skills.catalog.as_ref() else {
        return Ok(None);
    };
    let Some(active_run) = state.runs.active.as_ref() else {
        return Ok(None);
    };
    let Some(batch_id) = active_run.active_tool_batch_id else {
        return Ok(None);
    };
    let Some(batch) = active_run.tool_batches.get(&batch_id) else {
        return Ok(None);
    };

    let catalog_bytes = deps
        .blobs
        .read_bytes(&catalog_context.catalog_ref)
        .await
        .map_err(activity_error)?;
    let catalog =
        serde_json::from_slice::<SkillCatalogSnapshot>(&catalog_bytes).map_err(activity_error)?;

    let mut activations = state.skills.activations.clone();
    for call_state in &batch.calls {
        let Some(result) = call_state.result.as_ref() else {
            continue;
        };
        let Some(output_ref) = result.output_ref.as_ref() else {
            continue;
        };
        let output_bytes = deps
            .blobs
            .read_bytes(output_ref)
            .await
            .map_err(activity_error)?;
        let output_json = serde_json::from_slice(&output_bytes).map_err(activity_error)?;
        let Some(activation) = skill_activation_from_tool_result(SkillToolResultActivationInput {
            catalog_ref: &catalog_context.catalog_ref,
            catalog: &catalog,
            current_activations: &activations,
            call_id: &result.call_id,
            tool_name: &call_state.call.tool_name,
            status: result.status,
            execution_target: call_state.execution_target.as_ref(),
            output_json: &output_json,
        }) else {
            continue;
        };
        activations.push(activation);
    }

    if activations == state.skills.activations {
        Ok(None)
    } else {
        Ok(Some(CoreAgentCommand::SetSkillActivations { activations }))
    }
}
