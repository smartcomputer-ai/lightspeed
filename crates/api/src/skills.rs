use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillListParams {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillListResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_ref: Option<String>,
    #[serde(default)]
    pub skills: Vec<SkillListItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillListItem {
    pub skill_id: SkillId,
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
    pub enabled: bool,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillActiveParams {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillActiveResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_ref: Option<String>,
    #[serde(default)]
    pub activations: Vec<SkillActivationView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillActivationView {
    pub skill_id: SkillId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
    pub catalog_ref: String,
    pub scope: SkillActivationScope,
    pub source: SkillActivationSource,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SkillActivationScope {
    #[default]
    Run,
    Session,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SkillActivationSource {
    ToolResult { call_id: String },
    DirectContext { context_ref: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillActivateParams {
    pub session_id: SessionId,
    pub skill_id: SkillId,
    #[serde(default)]
    pub scope: SkillActivationScope,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillActivateResponse {
    pub activation: SkillActivationView,
    #[serde(default)]
    pub active: Vec<SkillActivationView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillDeactivateParams {
    pub session_id: SessionId,
    pub skill_id: SkillId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillDeactivateResponse {
    pub skill_id: SkillId,
    #[serde(default)]
    pub active: Vec<SkillActivationView>,
}
