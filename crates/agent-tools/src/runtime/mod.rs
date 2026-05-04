//! Runtime-neutral tool catalog and profile assembly types.

use std::collections::BTreeMap;

use agent_core::{
    BlobRef, ToolName, ToolParallelism, ToolProfileId, ToolRegistry, storage::BlobWrite,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ToolError, ToolResult};

pub mod target;

pub use target::ToolTarget;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolDocument {
    pub blob_ref: BlobRef,
    pub media_type: &'static str,
    pub bytes: Vec<u8>,
}

impl ToolDocument {
    pub fn text(media_type: &'static str, text: impl Into<String>) -> Self {
        let bytes = text.into().into_bytes();
        Self {
            blob_ref: BlobRef::from_bytes(&bytes),
            media_type,
            bytes,
        }
    }

    pub fn blob_write(&self) -> BlobWrite {
        BlobWrite {
            bytes: self.bytes.clone(),
            child_refs: Vec::new(),
        }
    }

    pub fn text_lossy(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolSpecBundle {
    pub spec: agent_core::ToolSpec,
    pub documents: Vec<ToolDocument>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedToolProfile {
    pub profile_id: ToolProfileId,
    pub registry: ToolRegistry,
    pub documents: Vec<ToolDocument>,
    pub catalog: ToolCatalog,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ToolCatalog {
    bindings: BTreeMap<ToolName, ToolBinding>,
}

impl ToolCatalog {
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
    pub activity_type: String,
    pub execution: ToolExecutionMode,
    pub parallelism: ToolParallelism,
}

impl ToolBinding {
    pub fn new(
        tool_name: ToolName,
        logical_id: impl Into<String>,
        activity_type: impl Into<String>,
        execution: ToolExecutionMode,
        parallelism: ToolParallelism,
    ) -> Self {
        Self {
            tool_name,
            logical_id: logical_id.into(),
            activity_type: activity_type.into(),
            execution,
            parallelism,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolExecutionMode {
    Inline,
    Activity,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolInvocationOutput {
    pub output_json: Value,
    pub model_visible_text: String,
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
    })
}
