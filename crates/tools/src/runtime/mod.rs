//! Runtime-neutral tool catalog and profile assembly types.

use std::collections::BTreeMap;

use async_trait::async_trait;
use engine::{ToolEffect, ToolName};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ToolError, ToolResult};

pub mod inline;
pub mod target;

pub use inline::InlineToolRuntime;
pub use target::ToolTarget;

/// Runtime function definition. Code-owned tools render directly into this
/// value; externally authored definitions are loaded from CAS by the adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FunctionDefinition {
    pub name: ToolName,
    pub description: Option<String>,
    pub input_schema: Value,
    pub strict: Option<bool>,
    pub provider_options: Option<Value>,
}

impl FunctionDefinition {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        Self {
            name: ToolName::new(name),
            description: Some(description.into()),
            input_schema,
            strict: Some(false),
            provider_options: None,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ToolCatalog {
    bindings: BTreeMap<ToolName, ToolBinding>,
}

impl ToolCatalog {
    pub fn from_registrations(
        tools: &BTreeMap<ToolName, engine::ToolSpec>,
        target: &ToolTarget,
    ) -> ToolResult<Self> {
        let mut catalog = Self::new();
        for tool in tools.values() {
            if let engine::ToolKind::Builtin(spec) = &tool.kind {
                for resolved in crate::definitions::resolve(&tool.name, spec, target)? {
                    if let Some(binding) = resolved.binding {
                        if catalog.get(&binding.tool_name).is_some() {
                            return Err(ToolError::InvalidRequest {
                                message: format!(
                                    "duplicate exposed tool name {}",
                                    binding.tool_name
                                ),
                            });
                        }
                        catalog.insert(binding);
                    }
                }
            }
        }
        Ok(catalog)
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, binding: ToolBinding) {
        self.bindings.insert(binding.tool_name.clone(), binding);
    }

    pub fn get(&self, tool_name: &ToolName) -> Option<&ToolBinding> {
        self.bindings.get(tool_name)
    }

    pub fn bindings(&self) -> impl Iterator<Item = &ToolBinding> {
        self.bindings.values()
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolBinding {
    pub tool_name: ToolName,
    pub logical_id: String,
    /// Runtime-only schema adapter identity. The logical id remains stable
    /// across provider-native presentations.
    pub adapter_id: Option<String>,
}

impl ToolBinding {
    pub fn new(tool_name: ToolName, logical_id: impl Into<String>) -> Self {
        Self {
            tool_name,
            logical_id: logical_id.into(),
            adapter_id: None,
        }
    }

    pub fn with_adapter_id(mut self, adapter_id: impl Into<String>) -> Self {
        self.adapter_id = Some(adapter_id.into());
        self
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolInvocationOutput {
    pub output_json: Value,
    pub model_visible_text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<ToolEffect>,
    /// Admitted media the tool hands the model, in the order the visible
    /// text announces it. Each item becomes a user-role media entry after the
    /// tool result; the bytes are already in CAS.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<ToolMediaOutput>,
}

impl ToolInvocationOutput {
    pub fn with_media(mut self, media: Vec<ToolMediaOutput>) -> Self {
        self.media = media;
        self
    }
}

/// One admitted media asset a tool produced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolMediaOutput {
    pub content_ref: engine::BlobRef,
    pub media_type: String,
    pub kind: engine::media::MediaKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ToolMediaOutput {
    pub fn context_entry(&self) -> engine::ContextEntryInput {
        engine::media::media_context_entry(
            self.content_ref.clone(),
            &self.media_type,
            self.kind,
            self.name.as_deref(),
        )
    }
}

#[async_trait]
pub trait ToolRuntime: Send + Sync {
    async fn invoke_json(
        &self,
        tool_name: &ToolName,
        arguments: Value,
    ) -> ToolResult<ToolInvocationOutput>;
}

pub(crate) fn decode_args<T>(arguments: Value) -> ToolResult<T>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(arguments).map_err(|error| ToolError::InvalidRequest {
        message: format!("invalid tool arguments: {error}"),
    })
}

pub(crate) fn encode_output<T>(
    result: &T,
    model_visible_text: impl Into<String>,
) -> ToolResult<ToolInvocationOutput>
where
    T: Serialize,
{
    let output_json = serde_json::to_value(result).map_err(|error| ToolError::InvalidRequest {
        message: format!("failed to encode tool output: {error}"),
    })?;
    Ok(ToolInvocationOutput {
        output_json,
        model_visible_text: model_visible_text.into(),
        effects: Vec::new(),
        media: Vec::new(),
    })
}
