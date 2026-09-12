//! Shared materialization service used by workflow activities and API preflight.
use super::*;
use ::profiles::ProfileStore;
use temporal_workflow::{SessionToolsetPreparation, SessionToolsetSource};

#[derive(Clone)]
pub(crate) struct SessionPreparationService {
    pub(crate) store: Arc<PgStore>,
    pub(crate) task_queue: String,
}

impl SessionPreparationService {
    pub(crate) fn session_toolset_config(
        session_config: &SessionConfig,
        include_environment_tools: bool,
        include_job_read_tool: bool,
    ) -> ToolsetConfig {
        let features = &session_config.features;
        let mut config = ToolsetConfig::empty();
        config.environment_read = features.environments.is_some();
        config.environment_selection = features
            .environments
            .as_ref()
            .is_some_and(|environments| environments.selection_tools);
        config.builtin = match features.vfs.as_ref().and_then(|vfs| vfs.tools) {
            None => tools::toolset::BuiltinToolsetConfig::disabled(),
            Some(engine::VfsToolSurface::ReadOnly) => tools::toolset::BuiltinToolsetConfig {
                vfs: tools::toolset::FilesystemToolsetConfig::read_only(),
                ..tools::toolset::BuiltinToolsetConfig::disabled()
            },
            Some(engine::VfsToolSurface::Edit) => tools::toolset::BuiltinToolsetConfig::workspace(),
        };
        if let Some(web) = features.web.as_ref() {
            if let Some(search) = &web.search {
                config.web.search = Some(WebSearchToolConfig::new(
                    search.allowed_domains.clone().unwrap_or_default(),
                    search.blocked_domains.clone(),
                ));
            }
            if web.fetch.is_some() {
                config.web.fetch = true;
            }
        }
        if features.timers.is_some() || features.subagents.is_some() {
            // Joining spawned sub-agents depends on the base concurrency
            // tools, so the subagents grant implies them; the timers grant
            // adds nothing extra today beyond the same surface.
            config.concurrency = tools::concurrency::ConcurrencyToolsetConfig::timer();
        }
        if include_environment_tools && let Some(environment) = &features.environments {
            config.builtin.environment.filesystem = match environment.tools {
                None => tools::toolset::FilesystemToolsetConfig::disabled(),
                Some(engine::EnvironmentToolSurface::ReadOnly) => {
                    tools::toolset::FilesystemToolsetConfig::read_only()
                }
                Some(engine::EnvironmentToolSurface::Edit) => {
                    tools::toolset::FilesystemToolsetConfig::workspace_edit()
                }
            };
            config.builtin.environment.run_process = environment.commands;
            config.builtin.environment.continue_process = environment.commands;
        }
        if include_job_read_tool {
            config.builtin.environment.job_read = true;
        }
        config
    }

    pub(crate) async fn core_environment_job_workflow_tool_declarations(
        &self,
    ) -> Result<Vec<WorkflowToolDeclaration>, AgentApiError> {
        let recipe_bytes = serde_json::to_vec(&temporal_workflow::WorkflowToolRecipeV1 {
            workflow_type: "EnvironmentJobWorkflow".to_owned(),
            task_queue: self.task_queue.clone(),
        })
        .map_err(|error| {
            AgentApiError::internal(format!(
                "encode core environment-job workflow recipe: {error}"
            ))
        })?;
        let recipe_fingerprint = temporal_workflow::workflow_tool_recipe_fingerprint(&recipe_bytes);
        let recipe_ref = self
            .store
            .put_bytes(recipe_bytes)
            .await
            .map_err(map_blob_store_error)?;

        let definitions = [
            (
                BuiltinToolOperation::JobSubmit,
                JOB_SUBMIT_WORKFLOW_TOOL_ID,
                JOB_SUBMIT_WORKFLOW_SEMANTIC_TYPE,
                WorkflowToolCompletion::Promises {
                    reply_schema_ref: None,
                    deadline_after_ms: None,
                    max_promises: engine::MAX_COMPLETION_PROMISES,
                    key_source: WorkflowToolCompletionKeySource::ArrayItemField {
                        pointer: "/jobs".to_owned(),
                        field: "job_id".to_owned(),
                    },
                },
            ),
            (
                BuiltinToolOperation::JobRun,
                JOB_RUN_WORKFLOW_TOOL_ID,
                JOB_RUN_WORKFLOW_SEMANTIC_TYPE,
                WorkflowToolCompletion::Joined {
                    reply_schema_ref: None,
                    deadline_after_ms: JOB_RUN_DEADLINE_AFTER_MS,
                },
            ),
        ];
        let mut declarations = Vec::with_capacity(definitions.len());
        for (operation, tool_id, semantic_type, completion) in definitions {
            let builtin = BuiltinTool::environment_canonical(operation);
            let tool = tools::definitions::register(
                builtin.logical_id(),
                tools::definitions::BuiltinSettings {
                    presentation: tools::toolset::BuiltinToolPresentation::Canonical,
                    unscoped_paths: true,
                    ..Default::default()
                },
                builtin.parallelism(),
                builtin.execution_spec(),
            );
            declarations.push(WorkflowToolDeclaration::new(
                WorkflowToolDefinition {
                    tool_id: WorkflowToolId::new(tool_id),
                    revision: 1,
                    semantic_type: semantic_type.to_owned(),
                    tool,
                },
                WorkflowToolTarget::Start {
                    start: WorkflowStartRef {
                        recipe_format: temporal_workflow::WORKFLOW_TOOL_RECIPE_FORMAT_V1,
                        revision: 1,
                        recipe_ref: recipe_ref.clone(),
                        recipe_fingerprint: recipe_fingerprint.clone(),
                    },
                },
                completion,
            ));
        }
        Ok(declarations)
    }

    pub(crate) async fn core_subagent_workflow_tool_declarations(
        &self,
    ) -> Result<Vec<WorkflowToolDeclaration>, AgentApiError> {
        let recipe_bytes = serde_json::to_vec(&temporal_workflow::WorkflowToolRecipeV1 {
            workflow_type: tools::subagents::SUBAGENT_WORKFLOW_TYPE.to_owned(),
            task_queue: self.task_queue.clone(),
        })
        .map_err(|error| {
            AgentApiError::internal(format!("encode core subagent workflow recipe: {error}"))
        })?;
        let recipe_fingerprint = temporal_workflow::workflow_tool_recipe_fingerprint(&recipe_bytes);
        let recipe_ref = self
            .store
            .put_bytes(recipe_bytes)
            .await
            .map_err(map_blob_store_error)?;
        // The binding carries the hard ceiling; the grant's `deadlineMs` is
        // pinned per call and enforced inside the execution, so the
        // immutable binding never has to change with the grant.
        let definitions = [
            (
                tools::subagents::SubagentToolKind::Run,
                WorkflowToolCompletion::Joined {
                    reply_schema_ref: None,
                    deadline_after_ms: engine::SUBAGENT_DEADLINE_CEILING_MS,
                },
            ),
            (
                tools::subagents::SubagentToolKind::Spawn,
                WorkflowToolCompletion::Promises {
                    reply_schema_ref: None,
                    deadline_after_ms: Some(engine::SUBAGENT_DEADLINE_CEILING_MS),
                    max_promises: 1,
                    key_source: WorkflowToolCompletionKeySource::Reply,
                },
            ),
        ];
        let mut declarations = Vec::with_capacity(definitions.len());
        for (kind, completion) in definitions {
            let tool = tools::definitions::register(
                match kind {
                    tools::subagents::SubagentToolKind::Run => "subagent.run",
                    tools::subagents::SubagentToolKind::Spawn => "subagent.spawn",
                },
                Default::default(),
                engine::ToolParallelism::ParallelSafe,
                Default::default(),
            );
            declarations.push(WorkflowToolDeclaration::new(
                WorkflowToolDefinition {
                    tool_id: WorkflowToolId::new(kind.workflow_tool_id()),
                    revision: 1,
                    semantic_type: kind.semantic_type().to_owned(),
                    tool,
                },
                WorkflowToolTarget::Start {
                    start: WorkflowStartRef {
                        recipe_format: temporal_workflow::WORKFLOW_TOOL_RECIPE_FORMAT_V1,
                        revision: 1,
                        recipe_ref: recipe_ref.clone(),
                        recipe_fingerprint: recipe_fingerprint.clone(),
                    },
                },
                completion,
            ));
        }
        Ok(declarations)
    }

    pub(crate) async fn prepare_toolset(
        &self,
        source: SessionToolsetSource,
    ) -> Result<SessionToolsetPreparation, AgentApiError> {
        let session_config = &source.config;
        let jobs = session_config
            .features
            .environments
            .as_ref()
            .is_some_and(|e| e.jobs);
        let subagents = session_config.features.subagents.is_some();
        let mut declarations = Vec::new();
        if jobs {
            declarations.extend(
                self.core_environment_job_workflow_tool_declarations()
                    .await?,
            );
        }
        if subagents {
            declarations.extend(self.core_subagent_workflow_tool_declarations().await?);
        }
        for declaration in &declarations {
            let id = &declaration.definition.tool_id;
            if let Some(existing) = source.bindings.get(id) {
                if !source.system_binding_ids.contains(id)
                    || existing.session_universe_id != self.store.config().universe_id
                    || existing.definition.semantic_type != declaration.definition.semantic_type
                    || existing.definition.tool.name != declaration.definition.tool.name
                {
                    return Err(AgentApiError::invalid_request(format!(
                        "system workflow tool {id} conflicts with an existing immutable binding"
                    )));
                }
            }
        }
        let mut validation_state = engine::CoreAgentState::new();
        validation_state.workflow_tools.bindings = source.bindings.clone();
        validate_subagent_deadline_for_existing_bindings(
            &validation_state,
            &session_config.features,
        )?;
        declarations.retain(|declaration| {
            !source
                .bindings
                .contains_key(&declaration.definition.tool_id)
        });
        let mut bindings = source.bindings.clone();
        for declaration in &declarations {
            let binding = engine::WorkflowToolBinding::admit(
                self.store.config().universe_id,
                declaration.definition.clone(),
                declaration.target.clone(),
                declaration.completion.clone(),
            )
            .map_err(|error| AgentApiError::invalid_request(error.to_string()))?;
            bindings.insert(binding.definition.tool_id.clone(), binding);
        }
        let materialized = bindings
            .values()
            .filter(|binding| {
                (jobs || !is_core_environment_job_binding(binding))
                    && (subagents || !is_core_subagent_binding(binding))
            })
            .collect::<Vec<_>>();
        let mut config = Self::session_toolset_config(
            session_config,
            session_config.features.environments.is_some(),
            jobs,
        );
        enable_concurrency_for_workflow_tools(&mut config, materialized.iter().copied());
        let mut toolset = register_toolset(&config)
            .map_err(|e| AgentApiError::internal(format!("build session tools: {e}")))?;
        register_workflow_tools(&mut toolset, materialized.iter().copied()).map_err(|e| {
            AgentApiError::invalid_request(format!("materialize workflow tools: {e}"))
        })?;
        let desired_mcp = self.desired_mcp_tools(&session_config.features).await?;
        if let Some(name) = toolset
            .tools
            .keys()
            .find(|name| desired_mcp.contains_key(*name))
        {
            return Err(AgentApiError::invalid_request(format!(
                "tool name {name} collides with a remote MCP tool"
            )));
        }
        let mut tools = toolset.tools;
        tools.extend(desired_mcp);
        Ok(SessionToolsetPreparation {
            source,
            declarations,
            tools,
        })
    }
}

impl SessionPreparationService {
    pub(crate) async fn validate_configuration(
        &self,
        config: &SessionConfig,
    ) -> Result<(), AgentApiError> {
        config
            .validate()
            .map_err(|e| AgentApiError::invalid_request(e.to_string()))?;
        if let Some(vfs) = &config.features.vfs {
            let links = vfs::resolve_workspace_links(
                self.store.clone(),
                self.store.clone(),
                &vfs.workspace_links,
            )
            .await
            .map_err(map_vfs_catalog_error)?;
            if let Some(link) = links.iter().find(|link| !link.is_available()) {
                return Err(AgentApiError::invalid_request(format!(
                    "workspace link target at {} is unavailable: {}",
                    link.path,
                    link.unavailable_reason().unwrap_or("unknown reason")
                )));
            }
        }
        if let Some(subagents) = &config.features.subagents {
            for agent in &subagents.agents {
                let id = api::ProfileId::try_new(agent.profile_id.clone())
                    .map_err(|e| AgentApiError::invalid_request(e.to_string()))?;
                self.store
                    .read_agent_profile(&id)
                    .await
                    .map_err(profiles::map_profile_error)?;
            }
        }
        Ok(())
    }

    pub(crate) async fn prepare_profile(
        &self,
        request: temporal_workflow::SessionProfilePreparationRequest,
    ) -> Result<temporal_workflow::SessionProfilePreparation, AgentApiError> {
        self.validate_configuration(&request.source.config).await?;
        let mut instructions = BTreeMap::new();
        if let Some(input) = request.instructions {
            let reference = match input {
                ProfileInstructions::Text { text } => self
                    .store
                    .put_bytes(text.into_bytes())
                    .await
                    .map_err(map_blob_store_error)?,
                ProfileInstructions::TextRef { blob_ref } => {
                    let reference = parse_blob_ref(&blob_ref)?;
                    if !self
                        .store
                        .has_blob(&reference)
                        .await
                        .map_err(map_blob_store_error)?
                    {
                        return Err(AgentApiError::not_found(format!(
                            "profile instructions blob not found: {reference}"
                        )));
                    }
                    reference
                }
            };
            instructions.insert(
                ContextEntryKey::new("instructions.050.profile"),
                ContextEntryInput {
                    kind: ContextEntryKind::Instructions,
                    content: engine::ContentRef::text(reference),
                    preview: Some("Profile instructions".to_owned()),
                    origin: None,
                    provenance_ref: None,
                    token_estimate: None,
                },
            );
        }
        let environment_id = match request.environment {
            None => None,
            Some(environment) => {
                let feature = request
                    .source
                    .config
                    .features
                    .environments
                    .as_ref()
                    .ok_or_else(|| {
                        AgentApiError::rejected(
                            "profile environment requires features.environments",
                        )
                    })?;
                let policy = ::environments::EnvironmentAccessPolicy::new(
                    feature.providers.clone(),
                    feature.registration_keys.clone(),
                );
                let id = match environment {
                    ProfileEnvironment::Existing { environment_id } => {
                        engine::EnvironmentId::try_new(environment_id)
                            .map_err(|e| AgentApiError::invalid_request(e.to_string()))?
                    }
                    ProfileEnvironment::Inherit {} => {
                        let child = self
                            .store
                            .load_session(&request.session_id)
                            .await
                            .map_err(map_session_store_error)?
                            .ok_or_else(|| AgentApiError::not_found("child session not found"))?;
                        let origin = child.origin.ok_or_else(|| {
                            AgentApiError::rejected(
                                "profile environment inherit requires a delegation origin",
                            )
                        })?;
                        let parent = self
                            .store
                            .load_session(&origin.parent_session_id)
                            .await
                            .map_err(map_session_store_error)?
                            .ok_or_else(|| AgentApiError::not_found("parent session not found"))?;
                        let reduced = crate::checkpoint::load_reduction(
                            self.store.as_ref(),
                            self.store.as_ref(),
                            &parent,
                        )
                        .await
                        .map_err(|e| AgentApiError::internal(e.to_string()))?;
                        let id = reduced
                            .reduced
                            .core_state
                            .environment
                            .active_environment_id
                            .ok_or_else(|| {
                                AgentApiError::rejected("parent session has no active environment")
                            })?;
                        id
                    }
                };
                crate::environment_resolver::EnvironmentResolver::from_pg_store(self.store.clone())
                    .selectable(&id, &policy)
                    .await
                    .map_err(super::environments::map_environment_resolve_error)?;
                Some(id)
            }
        };
        let toolset = self.prepare_toolset(request.source).await?;
        Ok(temporal_workflow::SessionProfilePreparation {
            toolset,
            instructions,
            environment_id,
        })
    }
}
