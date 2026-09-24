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
            .is_some_and(|environments| environments.selection);
        // Tool surfaces are the union of attachment grants: any attachment
        // installs the read tools, any editing attachment the write tools.
        config.builtin = match features.vfs.as_ref().and_then(|vfs| vfs.tool_access()) {
            None => tools::toolset::BuiltinToolsetConfig::disabled(),
            Some(engine::WorkspaceAccess::Read) => tools::toolset::BuiltinToolsetConfig {
                vfs: tools::toolset::FilesystemToolsetConfig::read_only(),
                ..tools::toolset::BuiltinToolsetConfig::disabled()
            },
            Some(engine::WorkspaceAccess::Edit) => {
                tools::toolset::BuiltinToolsetConfig::workspace()
            }
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
            let access = environment.tool_access();
            config.builtin.environment.filesystem = match access {
                None => tools::toolset::FilesystemToolsetConfig::disabled(),
                Some(access) if access.allows_edit() => {
                    tools::toolset::FilesystemToolsetConfig::workspace_edit()
                }
                Some(_) => tools::toolset::FilesystemToolsetConfig::read_only(),
            };
            let exec = access.is_some_and(|access| access.allows_exec());
            config.builtin.environment.run_process = exec;
            config.builtin.environment.continue_process = exec;
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
            .and_then(|environments| environments.tool_access())
            .is_some_and(|access| access.allows_jobs());
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
            if let Some(existing) = source.bindings.get(id)
                && (!source.system_binding_ids.contains(id)
                    || existing.session_universe_id != self.store.config().universe_id
                    || existing.definition.semantic_type != declaration.definition.semantic_type
                    || existing.definition.tool.name != declaration.definition.tool.name)
            {
                return Err(AgentApiError::invalid_request(format!(
                    "system workflow tool {id} conflicts with an existing immutable binding"
                )));
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
    pub(crate) async fn validate_workspace_attachment_targets(
        &self,
        features: &engine::FeaturesConfig,
    ) -> Result<(), AgentApiError> {
        let Some(vfs) = features.vfs.as_ref() else {
            return Ok(());
        };
        if vfs.workspaces.is_empty() {
            return Ok(());
        }
        let blobs: Arc<dyn BlobStore> = self.store.clone();
        let workspace_store: Arc<dyn VfsWorkspaceStore> = self.store.clone();
        let resolved = vfs::resolve_workspace_attachments(blobs, workspace_store, &vfs.workspaces)
            .await
            .map_err(map_vfs_catalog_error)?;
        if let Some(link) = resolved.iter().find(|link| !link.is_available()) {
            return Err(AgentApiError::invalid_request(format!(
                "workspace attachment target at {} is unavailable: {}",
                link.path,
                link.unavailable_reason().unwrap_or("unknown reason")
            )));
        }
        Ok(())
    }

    /// Every workspace, environment and MCP server `features` attaches must
    /// be usable by the session's execution identity. This is the check of
    /// the workflow's own preparation, which profile application, bot
    /// sessions and delegated children go through; it has no caller, so a
    /// resource hidden from the identity is reported missing.
    async fn admit_session_resources(
        &self,
        session: &SessionId,
        features: &engine::FeaturesConfig,
    ) -> Result<(), AgentApiError> {
        let root = ResourceRef::Session(session.as_str().to_owned());
        match store_pg::PgAccessStore::new(self.store.pool().clone())
            .execution_use(
                self.store.config().universe_id,
                &root,
                &temporal_workflow::attached_resources(features),
                store_pg::UseCheck::Admission,
            )
            .await
            .map_err(|error| AgentApiError::internal(error.to_string()))?
        {
            None => Ok(()),
            Some(refusal) => {
                let visible = refusal.visible_to_identity();
                Err(authorization::use_refusal(&root, refusal, visible))
            }
        }
    }

    /// The configuration the workflow is about to install on `session`:
    /// well-formed, its attachments admitted for the session's execution
    /// identity, and its catalog references resolvable. Admission comes
    /// first, so nothing below describes a resource the identity may not
    /// use.
    pub(crate) async fn validate_configuration(
        &self,
        session: &SessionId,
        config: &SessionConfig,
    ) -> Result<(), AgentApiError> {
        config
            .validate()
            .map_err(|e| AgentApiError::invalid_request(e.to_string()))?;
        self.admit_session_resources(session, &config.features)
            .await?;
        self.validate_workspace_attachment_targets(&config.features)
            .await?;
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
        self.validate_configuration(&request.session_id, &request.source.config)
            .await?;
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
        // The fill candidate must be an attachment of the configuration being
        // applied and selectable in the registry; whether it is applied is
        // decided by the workflow against the live pointer.
        let environment_id = match request.environment {
            None => None,
            Some(environment_id) => {
                let attached = request
                    .source
                    .config
                    .features
                    .environments
                    .as_ref()
                    .is_some_and(|environments| environments.is_attached(environment_id.as_str()));
                if !attached {
                    return Err(AgentApiError::rejected(format!(
                        "environment {environment_id} is not attached in the session configuration"
                    )));
                }
                crate::environments::resolver::EnvironmentResolver::from_pg_store(
                    self.store.clone(),
                )
                .selectable(&environment_id)
                .await
                .map_err(super::environments::map_environment_resolve_error)?;
                Some(environment_id)
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

impl GatewayAgentApi {
    pub(super) fn preparation_service(&self) -> SessionPreparationService {
        SessionPreparationService {
            store: self.store.clone(),
            task_queue: self.task_queue.clone(),
        }
    }
}
