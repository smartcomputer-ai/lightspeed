//! Agent profile registry contracts and validation helpers.
//!
//! Profile wire DTOs live in `api` so clients and gateways share one contract.
//! This crate owns the runtime registry/store boundary around those DTOs.

use api::{
    AgentProfile, AgentProfileInput, AgentProfileSummary, InlineAgentProfile, ProfileDocument,
    ProfileEnvironment, ProfileId, ProfileInstructions, ProfileSource,
};
use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProfileError {
    #[error("agent profile already exists: {profile_id}")]
    AlreadyExists { profile_id: ProfileId },

    #[error("agent profile not found: {profile_id}")]
    NotFound { profile_id: ProfileId },

    #[error("agent profile revision conflict for {profile_id}: expected {expected}, got {actual}")]
    RevisionConflict {
        profile_id: ProfileId,
        expected: u64,
        actual: u64,
    },

    #[error("invalid agent profile: {message}")]
    InvalidInput { message: String },

    #[error("agent profile store failure: {message}")]
    Store { message: String },
}

#[async_trait]
pub trait ProfileStore: Send + Sync {
    async fn create_agent_profile(
        &self,
        profile: AgentProfileInput,
        created_at_ms: i64,
    ) -> Result<AgentProfile, ProfileError>;

    /// Create the profile when absent, otherwise replace the whole document
    /// and bump the revision. `expected_revision` is checked only when the
    /// profile already exists; `None` replaces unconditionally.
    async fn put_agent_profile(
        &self,
        profile: AgentProfileInput,
        expected_revision: Option<u64>,
        now_ms: i64,
    ) -> Result<AgentProfile, ProfileError>;

    async fn read_agent_profile(
        &self,
        profile_id: &ProfileId,
    ) -> Result<AgentProfile, ProfileError>;

    async fn list_agent_profiles(&self) -> Result<Vec<AgentProfileSummary>, ProfileError>;

    async fn delete_agent_profile(
        &self,
        profile_id: &ProfileId,
    ) -> Result<AgentProfile, ProfileError>;
}

pub trait AgentProfileInputExt {
    fn into_record(self, created_at_ms: i64) -> AgentProfile;

    /// Whole-document replacement of `current`: identity and `created_at_ms`
    /// are preserved, the revision bumps, and everything else comes from the
    /// input.
    fn into_replacement(
        self,
        current: &AgentProfile,
        updated_at_ms: i64,
    ) -> Result<AgentProfile, ProfileError>;
}

impl AgentProfileInputExt for AgentProfileInput {
    fn into_record(self, created_at_ms: i64) -> AgentProfile {
        AgentProfile {
            profile_id: self.profile_id,
            display_name: self.display_name,
            description: self.description,
            revision: 1,
            document: self.document,
            created_at_ms,
            updated_at_ms: created_at_ms,
        }
    }

    fn into_replacement(
        self,
        current: &AgentProfile,
        updated_at_ms: i64,
    ) -> Result<AgentProfile, ProfileError> {
        if self.profile_id != current.profile_id {
            return Err(ProfileError::InvalidInput {
                message: format!(
                    "replacement profile id {} does not match current {}",
                    self.profile_id, current.profile_id
                ),
            });
        }
        let revision =
            current
                .revision
                .checked_add(1)
                .ok_or_else(|| ProfileError::InvalidInput {
                    message: "profile revision exhausted".to_owned(),
                })?;
        let profile = AgentProfile {
            profile_id: self.profile_id,
            display_name: self.display_name,
            description: self.description,
            revision,
            document: self.document,
            created_at_ms: current.created_at_ms,
            updated_at_ms,
        };
        profile.validate()?;
        Ok(profile)
    }
}

pub trait AgentProfileExt {
    fn validate(&self) -> Result<(), ProfileError>;
}

impl AgentProfileExt for AgentProfile {
    fn validate(&self) -> Result<(), ProfileError> {
        validate_nonempty_optional("displayName", self.display_name.as_deref())?;
        validate_nonempty_optional("description", self.description.as_deref())?;
        if self.revision == 0 {
            return Err(ProfileError::InvalidInput {
                message: "revision must be greater than zero".to_owned(),
            });
        }
        validate_nonnegative_i64("createdAtMs", self.created_at_ms)?;
        validate_nonnegative_i64("updatedAtMs", self.updated_at_ms)?;
        if self.updated_at_ms < self.created_at_ms {
            return Err(ProfileError::InvalidInput {
                message: "updatedAtMs must be >= createdAtMs".to_owned(),
            });
        }
        validate_profile_document(&self.document)
    }
}

pub trait ProfileSourceExt {
    fn validate(&self) -> Result<(), ProfileError>;
}

impl ProfileSourceExt for ProfileSource {
    fn validate(&self) -> Result<(), ProfileError> {
        match self {
            ProfileSource::Named { .. } => Ok(()),
            ProfileSource::Inline { profile } => validate_inline_profile(profile),
        }
    }
}

pub fn validate_profile_document(document: &ProfileDocument) -> Result<(), ProfileError> {
    environment_protocol::registration::validate_registration_metadata(None, &document.metadata)
        .map_err(|message| ProfileError::InvalidInput {
            message: format!("invalid metadata: {message}"),
        })?;
    if let Some(value) = document
        .retention
        .as_ref()
        .map(|retention| retention.delete_after_close_ms)
        && !(1..=api::MAX_SESSION_DELETE_AFTER_CLOSE_MS).contains(&value)
    {
        return Err(ProfileError::InvalidInput {
            message: format!(
                "deleteAfterCloseMs must be 1..={}",
                api::MAX_SESSION_DELETE_AFTER_CLOSE_MS
            ),
        });
    }
    if let Some(instructions) = &document.instructions {
        validate_profile_instructions(instructions)?;
    }
    if let Some(environment) = &document.environment {
        validate_profile_environment(environment)?;
    }
    Ok(())
}

fn validate_profile_environment(environment: &ProfileEnvironment) -> Result<(), ProfileError> {
    match environment {
        ProfileEnvironment::Existing { environment_id } => {
            validate_nonempty_string("environment.environmentId", environment_id)
        }
        ProfileEnvironment::Inherit {} => Ok(()),
    }
}

fn validate_inline_profile(profile: &InlineAgentProfile) -> Result<(), ProfileError> {
    validate_nonempty_optional("displayName", profile.display_name.as_deref())?;
    validate_nonempty_optional("description", profile.description.as_deref())?;
    validate_profile_document(&profile.document)
}

fn validate_profile_instructions(instructions: &ProfileInstructions) -> Result<(), ProfileError> {
    match instructions {
        ProfileInstructions::Text { text } => validate_nonempty_string("instructions.text", text),
        ProfileInstructions::TextRef { blob_ref } => {
            validate_nonempty_string("instructions.blobRef", blob_ref)
        }
    }
}

fn validate_nonempty_optional(name: &str, value: Option<&str>) -> Result<(), ProfileError> {
    if let Some(value) = value {
        validate_nonempty_string(name, value)?;
    }
    Ok(())
}

fn validate_nonempty_string(name: &str, value: &str) -> Result<(), ProfileError> {
    if value.trim().is_empty() {
        return Err(ProfileError::InvalidInput {
            message: format!("{name} must not be empty"),
        });
    }
    Ok(())
}

fn validate_nonnegative_i64(name: &str, value: i64) -> Result<(), ProfileError> {
    if value < 0 {
        return Err(ProfileError::InvalidInput {
            message: format!("{name} must be nonnegative"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn input_into_record_stamps_registry_metadata() {
        let record = AgentProfileInput {
            profile_id: ProfileId::new("support"),
            display_name: Some("Support".to_owned()),
            description: None,
            document: ProfileDocument::default(),
        }
        .into_record(42);

        assert_eq!(record.profile_id.as_str(), "support");
        assert_eq!(record.revision, 1);
        assert_eq!(record.created_at_ms, 42);
        assert_eq!(record.updated_at_ms, 42);
    }

    #[test]
    fn document_validation_rejects_empty_existing_environment_id() {
        let empty_environment = ProfileDocument {
            environment: Some(ProfileEnvironment::Existing {
                environment_id: String::new(),
            }),
            ..ProfileDocument::default()
        };
        assert!(matches!(
            validate_profile_document(&empty_environment),
            Err(ProfileError::InvalidInput { message }) if message.contains("environment.environmentId")
        ));
    }

    #[test]
    fn document_validation_applies_session_metadata_bounds() {
        let valid = ProfileDocument {
            metadata: BTreeMap::from([("campaign".to_owned(), "release-42".to_owned())]),
            ..ProfileDocument::default()
        };
        assert!(validate_profile_document(&valid).is_ok());

        let reserved = ProfileDocument {
            metadata: BTreeMap::from([("lightspeed.internal".to_owned(), "value".to_owned())]),
            ..ProfileDocument::default()
        };
        assert!(matches!(
            validate_profile_document(&reserved),
            Err(ProfileError::InvalidInput { message })
                if message.contains("invalid metadata") && message.contains("lightspeed.")
        ));
    }

    #[test]
    fn document_validation_checks_session_retention_default() {
        let valid = ProfileDocument {
            retention: Some(api::ProfileSessionRetention {
                delete_after_close_ms: 86_400_000,
            }),
            ..ProfileDocument::default()
        };
        assert!(validate_profile_document(&valid).is_ok());

        let zero = ProfileDocument {
            retention: Some(api::ProfileSessionRetention {
                delete_after_close_ms: 0,
            }),
            ..ProfileDocument::default()
        };
        assert!(matches!(
            validate_profile_document(&zero),
            Err(ProfileError::InvalidInput { message }) if message.contains("deleteAfterCloseMs")
        ));
    }

    #[test]
    fn profile_provision_intents_are_rejected() {
        assert!(
            serde_json::from_value::<ProfileEnvironment>(serde_json::json!({
                "type": "provision", "providerId": "incus", "templateId": "dev"
            }))
            .is_err()
        );
    }

    #[test]
    fn inline_source_validation_rejects_empty_instruction_text() {
        let source = ProfileSource::Inline {
            profile: Box::new(InlineAgentProfile {
                display_name: None,
                description: None,
                document: ProfileDocument {
                    instructions: Some(ProfileInstructions::Text {
                        text: " ".to_owned(),
                    }),
                    ..ProfileDocument::default()
                },
            }),
        };

        assert!(matches!(
            source.validate(),
            Err(ProfileError::InvalidInput { message })
                if message.contains("instructions.text")
        ));
    }

    #[test]
    fn replacement_preserves_identity_and_bumps_revision() {
        let current = AgentProfile {
            profile_id: ProfileId::new("support"),
            display_name: Some("Support".to_owned()),
            description: Some("Old description".to_owned()),
            revision: 7,
            document: ProfileDocument {
                instructions: Some(ProfileInstructions::Text {
                    text: "Old instructions".to_owned(),
                }),
                ..ProfileDocument::default()
            },
            created_at_ms: 10,
            updated_at_ms: 15,
        };

        let replaced = AgentProfileInput {
            profile_id: ProfileId::new("support"),
            display_name: Some("Support v2".to_owned()),
            description: None,
            document: ProfileDocument::default(),
        }
        .into_replacement(&current, 20)
        .expect("replacement should apply");

        assert_eq!(replaced.revision, 8);
        assert_eq!(replaced.created_at_ms, 10);
        assert_eq!(replaced.updated_at_ms, 20);
        assert_eq!(replaced.display_name.as_deref(), Some("Support v2"));
        assert_eq!(replaced.description, None);
        assert_eq!(replaced.document, ProfileDocument::default());

        let mismatched = AgentProfileInput {
            profile_id: ProfileId::new("other"),
            display_name: None,
            description: None,
            document: ProfileDocument::default(),
        }
        .into_replacement(&current, 20);
        assert!(matches!(
            mismatched,
            Err(ProfileError::InvalidInput { message }) if message.contains("does not match")
        ));
    }
}
