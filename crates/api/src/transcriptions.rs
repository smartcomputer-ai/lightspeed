use super::*;

/// Immutable audio input in this universe's content store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptionAudio {
    pub blob_ref: String,
    pub mime: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptionStartParams {
    /// Scoped to the requester. Matching retries rejoin the original job,
    /// including after defaults change; changed requests conflict. Identity is
    /// retained for the Temporal namespace's workflow-history retention period.
    pub idempotency_key: String,
    pub audio: TranscriptionAudio,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

impl TranscriptionStartParams {
    pub fn validate(&self) -> Result<(), AgentApiError> {
        for (name, value, limit) in [
            ("idempotencyKey", self.idempotency_key.as_str(), 200),
            ("audio.mime", self.audio.mime.as_str(), 128),
            ("audio.name", self.audio.name.as_str(), 256),
        ] {
            if value.trim().is_empty() || value.len() > limit {
                return Err(AgentApiError::invalid_request(format!(
                    "{name} must contain 1–{limit} bytes"
                )));
            }
        }
        if self
            .language
            .as_ref()
            .is_some_and(|v| v.is_empty() || v.len() > 32)
            || self.prompt.as_ref().is_some_and(|v| v.len() > 8192)
        {
            return Err(AgentApiError::invalid_request(
                "language or prompt exceeds the transcription limit",
            ));
        }
        if let Some(model) = &self.model {
            ModelDefaultSlot::SpeechToText.validate_model(model)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptionReadParams {
    pub transcription_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TranscriptionCancelParams {
    pub transcription_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TranscriptionStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Expired,
}
impl TranscriptionStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending | Self::Running)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TranscriptionFailureKind {
    InvalidAudio,
    Configuration,
    Provider,
    Timeout,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionFailure {
    pub kind: TranscriptionFailureKind,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionView {
    pub transcription_id: String,
    pub created_by: Attribution,
    pub audio: TranscriptionAudio,
    pub model: ModelConfig,
    pub status: TranscriptionStatus,
    pub created_at_ms: u64,
    /// Plain UTF-8 transcript blob, usable as ordinary textRef input.
    /// Unsubmitted content can be swept after the ordinary CAS grace period.
    pub transcript_ref: Option<String>,
    pub text: Option<String>,
    pub failure: Option<TranscriptionFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionResponse {
    pub transcription: TranscriptionView,
}
