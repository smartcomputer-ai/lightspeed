use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PromptsActiveParams {
    pub session_id: SessionId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PromptsActiveResponse {
    #[serde(default)]
    pub instructions: Vec<PromptInstructionView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PromptInstructionView {
    pub key: String,
    pub instructions_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}
