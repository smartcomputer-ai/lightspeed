use engine::{ContextEntryKey, ContextEntryKind, CoreAgentCommand};
use temporal_workflow::{
    RuntimeProjectionRefreshActivityRequest, RuntimeProjectionRefreshActivityResult,
};
use temporalio_sdk::activities::ActivityError;
use tools::catalog::{
    SKILL_CATALOG_CONTEXT_KEY, SUBAGENT_CATALOG_CONTEXT_KEY, VFS_CATALOG_CONTEXT_KEY,
    clear_catalog_command,
};
use tools::subagents::{
    SubagentCatalogAgent, SubagentCatalogSnapshot, prepare_subagent_catalog_publication,
};
use tools::{
    environment::projection::{prepare_vfs_catalog_publication, vfs_catalog_from_workspace_links},
    prompts::{
        PromptAssemblyLimits, configured_vfs_prompt_root_specs,
        prepare_prompt_instructions_publication_with_warnings, resolve_linked_vfs_prompt_roots,
    },
    skills::{
        configured_vfs_skill_root_specs, prepare_skill_catalog_publication_with_warnings,
        resolve_linked_vfs_skill_roots,
    },
};

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

    let links = vfs::resolve_workspace_links(
        deps.blobs.clone(),
        deps.workspace_store.clone(),
        &request.workspace_links,
    )
    .await
    .map_err(activity_error)?;
    let current_vfs = request
        .active_catalogs
        .get(&ContextEntryKey::new(VFS_CATALOG_CONTEXT_KEY));
    let current_skills = request
        .active_catalogs
        .get(&ContextEntryKey::new(SKILL_CATALOG_CONTEXT_KEY));
    let current_subagents = request
        .active_catalogs
        .get(&ContextEntryKey::new(SUBAGENT_CATALOG_CONTEXT_KEY));
    let mut commands = Vec::new();
    let environment_sources = crate::environment_sources::refresh(
        deps.blobs.as_ref(),
        deps.environment_resolver.as_ref(),
        deps.environment_gateway.as_ref(),
        &request.session_id,
        request.environments.as_ref(),
        request.active_environment_id.as_ref(),
        request.active_catalogs.get(&ContextEntryKey::new(
            tools::skills::environment::ENVIRONMENT_SKILL_CATALOG_CONTEXT_KEY,
        )),
    )
    .await
    .map_err(activity_error)?;
    if let Some(command) = environment_sources.skill_command {
        commands.push(command);
    }

    if request.vfs_catalog_enabled {
        let catalog = vfs_catalog_from_workspace_links(&links).map_err(activity_error)?;
        if let Some(command) = prepare_vfs_catalog_publication(
            deps.blobs.as_ref(),
            deps.blob_graph.as_deref(),
            current_vfs,
            catalog,
        )
        .await
        .map_err(activity_error)?
        .command
        {
            commands.push(command);
        }
    } else if current_vfs.is_some() {
        commands.push(CoreAgentCommand::RemoveContext {
            expected_revision: None,
            key: ContextEntryKey::new(VFS_CATALOG_CONTEXT_KEY),
        });
    }

    // Sub-agent catalog: follows the grant, refreshed like the
    // skill catalog so profile description edits land at the next run.
    match request.subagents.as_ref() {
        Some(subagents) => {
            let snapshot = subagent_catalog_snapshot(deps.profiles.as_deref(), subagents).await;
            if let Some(command) = prepare_subagent_catalog_publication(
                deps.blobs.as_ref(),
                current_subagents,
                &snapshot,
            )
            .await
            .map_err(activity_error)?
            {
                commands.push(command);
            }
        }
        None => {
            if let Some(command) =
                clear_catalog_command(current_subagents, SUBAGENT_CATALOG_CONTEXT_KEY)
            {
                commands.push(command);
            }
        }
    }

    let prompt_entries = if request.vfs_prompts_enabled {
        let specs = configured_vfs_prompt_root_specs(&links, request.vfs_prompt_roots.as_deref())
            .map_err(activity_error)?;
        let resolved = resolve_linked_vfs_prompt_roots(
            deps.blobs.clone(),
            deps.workspace_store.clone(),
            links.clone(),
            specs,
        )
        .await
        .map_err(activity_error)?;
        let inputs = resolved
            .existing_directory_inputs()
            .await
            .map_err(activity_error)?;
        prepare_prompt_instructions_publication_with_warnings(
            deps.blobs.as_ref(),
            deps.blob_graph.as_deref(),
            &inputs,
            PromptAssemblyLimits::default(),
            resolved.warnings().to_vec(),
        )
        .await
        .map_err(activity_error)?
        .desired
    } else {
        Default::default()
    };
    let mut active_instructions = request.active_instruction_inputs.clone();
    active_instructions.remove(&ContextEntryKey::new(
        tools::prompts::environment::ENVIRONMENT_PROMPT_CONTEXT_KEY,
    ));
    active_instructions.extend(environment_sources.prompt_entries);
    let desired_instructions =
        replace_prompt_instruction_source(active_instructions, prompt_entries, deps.blobs.as_ref())
            .await
            .map_err(activity_error)?;
    if desired_instructions != request.active_instruction_inputs {
        commands.push(CoreAgentCommand::ReplaceContextPrefix {
            expected_revision: None,
            key_prefix: ContextEntryKey::new("instructions"),
            entries: desired_instructions,
        });
    }

    if current_skills.is_some_and(|entry| entry.origin.as_deref() != Some("runtime.vfs.skills")) {
        return Ok(RuntimeProjectionRefreshActivityResult { commands });
    }
    let Some(skills_config) = request.vfs_skills.as_ref() else {
        return Ok(RuntimeProjectionRefreshActivityResult {
            commands: append_optional(
                commands,
                clear_catalog_command(current_skills, SKILL_CATALOG_CONTEXT_KEY),
            ),
        });
    };
    let specs = configured_vfs_skill_root_specs(&links, skills_config.roots.as_deref())
        .map_err(activity_error)?;
    if specs.is_empty() {
        return Ok(RuntimeProjectionRefreshActivityResult {
            commands: append_optional(
                commands,
                clear_catalog_command(current_skills, SKILL_CATALOG_CONTEXT_KEY),
            ),
        });
    }

    let resolved = resolve_linked_vfs_skill_roots(
        deps.blobs.clone(),
        deps.workspace_store.clone(),
        links,
        specs,
    )
    .await
    .map_err(activity_error)?;
    let inputs = resolved
        .existing_directory_inputs()
        .await
        .map_err(activity_error)?;
    if inputs.is_empty() && resolved.warnings().is_empty() {
        return Ok(RuntimeProjectionRefreshActivityResult {
            commands: append_optional(
                commands,
                clear_catalog_command(current_skills, SKILL_CATALOG_CONTEXT_KEY),
            ),
        });
    }

    let publication = prepare_skill_catalog_publication_with_warnings(
        deps.blobs.as_ref(),
        deps.blob_graph.as_deref(),
        current_skills,
        &inputs,
        resolved.warnings().to_vec(),
    )
    .await
    .map_err(activity_error)?;
    if let Some(command) = publication.command {
        commands.push(command);
    }
    Ok(RuntimeProjectionRefreshActivityResult { commands })
}

async fn replace_prompt_instruction_source(
    mut active: std::collections::BTreeMap<ContextEntryKey, engine::ContextEntryInput>,
    prompts: std::collections::BTreeMap<ContextEntryKey, engine::ContextEntryInput>,
    blobs: &dyn engine::storage::BlobStore,
) -> Result<
    std::collections::BTreeMap<ContextEntryKey, engine::ContextEntryInput>,
    engine::storage::BlobStoreError,
> {
    active.retain(|key, _| {
        !(key.as_str() == tools::prompts::PROMPT_INSTRUCTIONS_CONTEXT_KEY_PREFIX
            || key.as_str().starts_with(&format!(
                "{}.",
                tools::prompts::PROMPT_INSTRUCTIONS_CONTEXT_KEY_PREFIX
            )))
    });
    active.extend(prompts);
    let default_key = ContextEntryKey::new("instructions.000.default");
    active.remove(&default_key);
    if active.is_empty() {
        let content_ref = blobs
            .put_bytes(
                temporal_workflow::default_instructions()
                    .as_bytes()
                    .to_vec(),
            )
            .await?;
        active.insert(
            default_key,
            engine::ContextEntryInput {
                kind: ContextEntryKind::Instructions,
                content: engine::ContentRef::text(content_ref),
                preview: None,
                origin: None,
                provenance_ref: None,
                token_estimate: None,
            },
        );
    }
    Ok(active)
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

/// Join the grant's allowlist with the current profile records. A missing
/// profile keeps its id in the menu with no revision, so the model learns
/// it is unavailable instead of silently losing the option.
pub async fn subagent_catalog_snapshot(
    profiles: Option<&dyn ::profiles::ProfileStore>,
    subagents: &engine::SubagentsFeature,
) -> SubagentCatalogSnapshot {
    let mut agents = Vec::with_capacity(subagents.agents.len());
    for agent in &subagents.agents {
        let record = match (profiles, api::ProfileId::try_new(agent.profile_id.clone())) {
            (Some(profiles), Ok(profile_id)) => profiles.read_agent_profile(&profile_id).await.ok(),
            _ => None,
        };
        agents.push(SubagentCatalogAgent {
            profile_id: agent.profile_id.clone(),
            display_name: record
                .as_ref()
                .and_then(|profile| profile.display_name.clone()),
            description: record
                .as_ref()
                .and_then(|profile| profile.description.clone()),
            revision: record.as_ref().map(|profile| profile.revision),
        });
    }
    SubagentCatalogSnapshot::new(agents, subagents.limits)
}
