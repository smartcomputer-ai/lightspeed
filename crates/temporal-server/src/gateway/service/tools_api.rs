use super::*;

#[derive(Debug)]
pub(super) enum CoreToolUpdate {
    Replace(BTreeMap<ToolName, engine::ToolSpec>),
    Patch(engine::ToolPatch),
}

impl CoreToolUpdate {
    pub(super) fn is_empty(&self) -> bool {
        match self {
            Self::Replace(_) => false,
            Self::Patch(patch) => patch.is_empty(),
        }
    }

    pub(super) fn into_command(self, expected_revision: Option<u64>) -> CoreAgentCommand {
        match self {
            Self::Replace(tools) => CoreAgentCommand::ReplaceTools {
                expected_revision,
                tools,
            },
            Self::Patch(patch) => CoreAgentCommand::PatchTools {
                expected_revision,
                patch,
            },
        }
    }

    pub(super) fn validate_for(
        &self,
        tools: &BTreeMap<ToolName, engine::ToolSpec>,
    ) -> Result<(), AgentApiError> {
        match self {
            Self::Replace(next) => engine::validate_tool_map(next).map_err(map_tool_api_error),
            Self::Patch(patch) => patch.validate_for(tools).map_err(map_tool_api_error),
        }
    }
}

pub(super) fn core_tool_update_from_api(
    update: api::SessionToolsUpdateInput,
) -> Result<CoreToolUpdate, AgentApiError> {
    match update {
        api::SessionToolsUpdateInput::Replace { tools } => {
            Ok(CoreToolUpdate::Replace(core_tool_set_from_api(tools)?))
        }
        api::SessionToolsUpdateInput::Patch { upsert, remove } => {
            let upsert = upsert
                .into_iter()
                .map(core_tool_from_api)
                .collect::<Result<Vec<_>, _>>()?;
            let remove = remove
                .into_iter()
                .map(parse_api_tool_name)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CoreToolUpdate::Patch(engine::ToolPatch { upsert, remove }))
        }
    }
}

fn core_tool_set_from_api(
    tools: Vec<api::ToolView>,
) -> Result<BTreeMap<ToolName, engine::ToolSpec>, AgentApiError> {
    let mut by_name = BTreeMap::new();
    for tool in tools {
        let tool = core_tool_from_api(tool)?;
        if by_name.insert(tool.name.clone(), tool.clone()).is_some() {
            return Err(AgentApiError::invalid_request(format!(
                "tool set contains duplicate tool id {}",
                tool.name
            )));
        }
    }
    engine::validate_tool_map(&by_name).map_err(map_tool_api_error)?;
    Ok(by_name)
}

fn core_tool_from_api(tool: api::ToolView) -> Result<engine::ToolSpec, AgentApiError> {
    let spec = engine::ToolSpec {
        name: parse_api_tool_name(tool.tool_id)?,
        kind: core_tool_kind_from_api(tool.kind)?,
        parallelism: core_tool_parallelism_from_api(tool.parallelism),
        target_requirement: core_tool_target_requirement_from_api(tool.target_requirement),
    };
    spec.validate().map_err(map_tool_api_error)?;
    Ok(spec)
}

fn core_tool_kind_from_api(kind: api::ToolKindView) -> Result<engine::ToolKind, AgentApiError> {
    Ok(match kind {
        api::ToolKindView::Function {
            model_name,
            description_ref,
            input_schema_ref,
            output_schema_ref,
            strict,
            provider_options_ref,
        } => engine::ToolKind::Function(engine::FunctionToolSpec {
            model_name: model_name.map(parse_api_tool_name).transpose()?,
            description_ref: parse_optional_blob_ref(description_ref)?,
            input_schema_ref: parse_blob_ref(&input_schema_ref)?,
            output_schema_ref: parse_optional_blob_ref(output_schema_ref)?,
            strict,
            provider_options_ref: parse_optional_blob_ref(provider_options_ref)?,
        }),
        api::ToolKindView::ProviderNative {
            api_kind,
            native_tool_ref,
            execution,
        } => engine::ToolKind::ProviderNative(engine::ProviderNativeToolSpec {
            api_kind: api_kind_from_str(&api_kind)?,
            native_tool_ref: parse_blob_ref(&native_tool_ref)?,
            execution: match execution {
                api::ProviderNativeToolExecutionView::ProviderHosted => {
                    engine::ProviderNativeToolExecution::ProviderHosted
                }
                api::ProviderNativeToolExecutionView::ClientEffect => {
                    engine::ProviderNativeToolExecution::ClientEffect
                }
            },
        }),
        api::ToolKindView::RemoteMcp {
            server_label,
            server_url,
            description_ref,
            allowed_tools,
            approval,
            defer_loading,
            auth_ref,
        } => engine::ToolKind::RemoteMcp(engine::RemoteMcpToolSpec {
            server_label,
            server_url,
            description_ref: parse_optional_blob_ref(description_ref)?,
            allowed_tools,
            approval: match approval {
                api::RemoteMcpApprovalPolicy::ProviderDefault => {
                    engine::RemoteMcpApprovalPolicy::ProviderDefault
                }
                api::RemoteMcpApprovalPolicy::Always => engine::RemoteMcpApprovalPolicy::Always,
                api::RemoteMcpApprovalPolicy::Never => engine::RemoteMcpApprovalPolicy::Never,
            },
            defer_loading,
            auth_ref: auth_ref.map(core_secret_ref_from_api),
        }),
    })
}

fn core_tool_parallelism_from_api(
    parallelism: api::ToolParallelismView,
) -> engine::ToolParallelism {
    match parallelism {
        api::ToolParallelismView::Exclusive => engine::ToolParallelism::Exclusive,
        api::ToolParallelismView::ParallelSafe => engine::ToolParallelism::ParallelSafe,
    }
}

fn core_tool_target_requirement_from_api(
    requirement: api::ToolTargetRequirementView,
) -> engine::ToolTargetRequirement {
    match requirement {
        api::ToolTargetRequirementView::None => engine::ToolTargetRequirement::None,
        api::ToolTargetRequirementView::Optional { namespace } => {
            engine::ToolTargetRequirement::Optional { namespace }
        }
        api::ToolTargetRequirementView::Required { namespace } => {
            engine::ToolTargetRequirement::Required { namespace }
        }
    }
}

fn core_secret_ref_from_api(auth_ref: api::SecretRefView) -> engine::SecretRef {
    engine::SecretRef {
        namespace: auth_ref.namespace,
        id: auth_ref.id,
    }
}

fn parse_api_tool_name(tool_id: String) -> Result<ToolName, AgentApiError> {
    ToolName::try_new(tool_id)
        .map_err(|error| AgentApiError::invalid_request(format!("invalid tool id: {error}")))
}

fn parse_optional_blob_ref(value: Option<String>) -> Result<Option<BlobRef>, AgentApiError> {
    value.as_deref().map(parse_blob_ref).transpose()
}

fn map_tool_api_error(error: engine::DomainError) -> AgentApiError {
    AgentApiError::invalid_request(error.to_string())
}
