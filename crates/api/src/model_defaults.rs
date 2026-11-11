use super::*;

/// A universe's model selection for a particular use. Protocol and purpose
/// are separate: several purposes may use the same provider API.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ModelDefaultSlot {
    AgentRun,
    SpeechToText,
}

impl ModelDefaultSlot {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentRun => "agentRun",
            Self::SpeechToText => "speechToText",
        }
    }

    pub fn validate_model(self, model: &ModelConfig) -> Result<(), AgentApiError> {
        for (field, value) in [("providerId", &model.provider_id), ("model", &model.model)] {
            if value.trim().is_empty() || value.trim() != value || value.len() > 512 {
                return Err(AgentApiError::invalid_request(format!(
                    "{field} must contain 1..=512 bytes without surrounding whitespace"
                )));
            }
        }
        let supported = match self {
            Self::AgentRun => matches!(
                model.api_kind.as_str(),
                "openai:responses" | "openai:completions" | "anthropic:messages"
            ),
            Self::SpeechToText => model.api_kind == "openai:audio-transcriptions",
        };
        if !supported {
            return Err(AgentApiError::invalid_request(format!(
                "API kind {} cannot be used for {}",
                model.api_kind,
                self.as_str()
            )));
        }
        Ok(())
    }
}

/// Persisted universe defaults. Revision zero means no update has been made.
/// Clearing a slot still advances the revision, so setup cannot undo a clear.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelDefaults {
    pub revision: u64,
    pub agent_run: Option<ModelConfig>,
    pub speech_to_text: Option<ModelConfig>,
}

impl ModelDefaults {
    pub fn model(&self, slot: ModelDefaultSlot) -> Option<&ModelConfig> {
        match slot {
            ModelDefaultSlot::AgentRun => self.agent_run.as_ref(),
            ModelDefaultSlot::SpeechToText => self.speech_to_text.as_ref(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelDefaultsReadParams {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelDefaultsPutParams {
    pub slot: ModelDefaultSlot,
    /// Complete selection, or explicit null to clear this slot. Required.
    #[serde(deserialize_with = "Option::deserialize")]
    #[schemars(required, schema_with = "required_nullable_model_schema")]
    pub model: Option<ModelConfig>,
    /// Write only if the defaults are still at this revision, as returned by
    /// read or put; zero for a universe with no updates. Omit it to write
    /// whatever the current revision is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelDefaultsResponse {
    pub defaults: ModelDefaults,
}

fn required_nullable_model_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    Option::<ModelConfig>::json_schema(generator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_validate_protocols_without_requiring_a_catalog_model() {
        for (kind, slot) in [
            ("openai:responses", ModelDefaultSlot::AgentRun),
            ("anthropic:messages", ModelDefaultSlot::AgentRun),
            ("openai:completions", ModelDefaultSlot::AgentRun),
            (
                "openai:audio-transcriptions",
                ModelDefaultSlot::SpeechToText,
            ),
        ] {
            let model = ModelConfig {
                provider_id: "custom".into(),
                api_kind: kind.into(),
                model: "private-model".into(),
            };
            slot.validate_model(&model).unwrap();
            let other = match slot {
                ModelDefaultSlot::AgentRun => ModelDefaultSlot::SpeechToText,
                ModelDefaultSlot::SpeechToText => ModelDefaultSlot::AgentRun,
            };
            assert_eq!(
                other.validate_model(&model).unwrap_err().kind,
                AgentApiErrorKind::InvalidRequest
            );
        }
    }

    #[test]
    fn clearing_requires_an_explicit_null_and_the_revision_is_optional() {
        let request = serde_json::json!({"slot":"agentRun", "model":null, "expectedRevision":4});
        let parsed = serde_json::from_value::<ModelDefaultsPutParams>(request.clone()).unwrap();
        assert_eq!(parsed.model, None);
        assert_eq!(parsed.expected_revision, Some(4));
        let mut without_model = request.clone();
        without_model.as_object_mut().unwrap().remove("model");
        assert!(serde_json::from_value::<ModelDefaultsPutParams>(without_model).is_err());
        let mut unconditional = request;
        unconditional
            .as_object_mut()
            .unwrap()
            .remove("expectedRevision");
        assert_eq!(
            serde_json::from_value::<ModelDefaultsPutParams>(unconditional)
                .unwrap()
                .expected_revision,
            None
        );
    }

    #[test]
    fn missing_default_error_identifies_its_slot() {
        let error = AgentApiError::model_default_unset(ModelDefaultSlot::AgentRun);
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["kind"], "modelDefaultUnset");
        assert_eq!(json["modelDefaultSlot"], "agentRun");
        assert_eq!(
            serde_json::from_value::<AgentApiError>(json).unwrap(),
            error
        );
    }
}
