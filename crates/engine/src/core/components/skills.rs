use serde::{Deserialize, Serialize};

use crate::{BlobRef, SkillId, ToolCallId};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillState {
    pub catalog: Option<SkillCatalogContext>,
    /// Skill bodies currently eligible for request planning.
    ///
    /// Historical skill injections are represented by context items in the log;
    /// once an activation stops being active, remove it from this list.
    pub activations: Vec<SkillActivation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillCatalogContext {
    pub catalog_ref: BlobRef,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillActivation {
    pub skill_id: SkillId,
    pub catalog_ref: BlobRef,
    pub context_ref: BlobRef,
    pub source: SkillActivationSource,
    pub scope: SkillActivationScope,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillActivationSource {
    ToolCall { call_id: ToolCallId },
    Direct,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillActivationScope {
    Run,
    Session,
}
