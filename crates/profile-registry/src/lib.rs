//! Agent profile registry contracts and validation helpers.
//!
//! Profile wire DTOs live in `api` so clients and gateways share one contract.
//! This crate owns the runtime registry/store boundary around those DTOs.

use std::collections::BTreeSet;

use api::{
    AgentProfile, AgentProfileInput, AgentProfileSummary, AgentProfileUpdatePatch,
    InlineAgentProfile, ProfileDocument, ProfileEnvironment, ProfileId, ProfileInstructions,
    ProfileMcpLink, ProfileMount, ProfileSource,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateAgentProfile {
    pub profile_id: ProfileId,
    pub expected_revision: Option<u64>,
    pub patch: AgentProfileUpdatePatch,
    pub updated_at_ms: i64,
}

#[async_trait]
pub trait ProfileStore: Send + Sync {
    async fn create_agent_profile(
        &self,
        profile: AgentProfileInput,
        created_at_ms: i64,
    ) -> Result<AgentProfile, ProfileError>;

    async fn read_agent_profile(
        &self,
        profile_id: &ProfileId,
    ) -> Result<AgentProfile, ProfileError>;

    async fn list_agent_profiles(&self) -> Result<Vec<AgentProfileSummary>, ProfileError>;

    async fn update_agent_profile(
        &self,
        update: UpdateAgentProfile,
    ) -> Result<AgentProfile, ProfileError>;

    async fn delete_agent_profile(
        &self,
        profile_id: &ProfileId,
    ) -> Result<AgentProfile, ProfileError>;
}

pub trait AgentProfileInputExt {
    fn into_record(self, created_at_ms: i64) -> AgentProfile;
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

pub trait AgentProfileUpdatePatchExt {
    fn apply_to(
        self,
        profile: AgentProfile,
        updated_at_ms: i64,
    ) -> Result<AgentProfile, ProfileError>;
}

impl AgentProfileUpdatePatchExt for AgentProfileUpdatePatch {
    fn apply_to(
        self,
        mut profile: AgentProfile,
        updated_at_ms: i64,
    ) -> Result<AgentProfile, ProfileError> {
        if let Some(patch) = self.display_name {
            profile.display_name = patch.into_option();
        }
        if let Some(patch) = self.description {
            profile.description = patch.into_option();
        }
        if let Some(patch) = self.config {
            profile.document.config = patch.into_option();
        }
        if let Some(patch) = self.instructions {
            profile.document.instructions = patch.into_option();
        }
        if let Some(mounts) = self.mounts {
            profile.document.mounts = mounts;
        }
        if let Some(mcp) = self.mcp {
            profile.document.mcp = mcp;
        }
        if let Some(environments) = self.environments {
            profile.document.environments = environments;
        }
        profile.revision =
            profile
                .revision
                .checked_add(1)
                .ok_or_else(|| ProfileError::InvalidInput {
                    message: "profile revision exhausted".to_owned(),
                })?;
        profile.updated_at_ms = updated_at_ms;
        profile.validate()?;
        Ok(profile)
    }
}

pub fn validate_profile_document(document: &ProfileDocument) -> Result<(), ProfileError> {
    if let Some(instructions) = &document.instructions {
        validate_profile_instructions(instructions)?;
    }
    let mut mount_paths = BTreeSet::new();
    for mount in &document.mounts {
        validate_profile_mount(mount)?;
        if !mount_paths.insert(mount.mount_path.clone()) {
            return Err(ProfileError::InvalidInput {
                message: format!("duplicate mountPath {}", mount.mount_path),
            });
        }
    }
    let mut server_ids = BTreeSet::new();
    for link in &document.mcp {
        validate_profile_mcp_link(link)?;
        if !server_ids.insert(link.server_id.clone()) {
            return Err(ProfileError::InvalidInput {
                message: format!("duplicate mcp serverId {}", link.server_id),
            });
        }
    }
    let mut env_ids = BTreeSet::new();
    let mut active_count = 0usize;
    for environment in &document.environments {
        validate_profile_environment(environment)?;
        if !env_ids.insert(environment.env_id.clone()) {
            return Err(ProfileError::InvalidInput {
                message: format!("duplicate environment envId {}", environment.env_id),
            });
        }
        if environment.activate {
            active_count += 1;
        }
    }
    if active_count > 1 {
        return Err(ProfileError::InvalidInput {
            message: "at most one environment may activate".to_owned(),
        });
    }
    Ok(())
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

fn validate_profile_mount(mount: &ProfileMount) -> Result<(), ProfileError> {
    validate_absolute_path("mountPath", &mount.mount_path)
}

fn validate_profile_mcp_link(link: &ProfileMcpLink) -> Result<(), ProfileError> {
    validate_nonempty_string("mcp.serverId", &link.server_id)?;
    validate_nonempty_optional("mcp.toolId", link.tool_id.as_deref())?;
    validate_nonempty_optional("mcp.serverLabel", link.server_label.as_deref())?;
    validate_nonempty_optional("mcp.authGrantId", link.auth_grant_id.as_deref())?;
    if let Some(allowed_tools) = &link.allowed_tools {
        if allowed_tools.is_empty() {
            return Err(ProfileError::InvalidInput {
                message: "mcp.allowedTools must not be empty when present".to_owned(),
            });
        }
        for tool in allowed_tools {
            validate_nonempty_string("mcp.allowedTools[]", tool)?;
        }
    }
    Ok(())
}

fn validate_profile_environment(environment: &ProfileEnvironment) -> Result<(), ProfileError> {
    validate_nonempty_string("environment.envId", &environment.env_id)?;
    validate_nonempty_string("environment.providerId", &environment.provider_id)?;
    validate_nonempty_string("environment.targetId", &environment.target_id)
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

fn validate_absolute_path(name: &str, value: &str) -> Result<(), ProfileError> {
    validate_nonempty_string(name, value)?;
    if !value.starts_with('/') {
        return Err(ProfileError::InvalidInput {
            message: format!("{name} must be an absolute VFS path"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::{
        FieldPatch, SessionConfigInput, ToolConfigInput, VfsMountAccess, VfsMountSourceInput,
    };

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
    fn document_validation_rejects_duplicate_keys_and_multiple_active_environments() {
        let duplicate_mounts = ProfileDocument {
            mounts: vec![
                ProfileMount {
                    mount_path: "/repo".to_owned(),
                    source: VfsMountSourceInput::Workspace {
                        workspace_id: "ws_1".to_owned(),
                    },
                    access: VfsMountAccess::ReadOnly,
                },
                ProfileMount {
                    mount_path: "/repo".to_owned(),
                    source: VfsMountSourceInput::Workspace {
                        workspace_id: "ws_2".to_owned(),
                    },
                    access: VfsMountAccess::ReadOnly,
                },
            ],
            ..ProfileDocument::default()
        };
        assert!(matches!(
            validate_profile_document(&duplicate_mounts),
            Err(ProfileError::InvalidInput { message }) if message.contains("duplicate mountPath")
        ));

        let multiple_active = ProfileDocument {
            environments: vec![
                ProfileEnvironment {
                    env_id: "dev_a".to_owned(),
                    provider_id: "host".to_owned(),
                    target_id: "local".to_owned(),
                    activate: true,
                },
                ProfileEnvironment {
                    env_id: "dev_b".to_owned(),
                    provider_id: "host".to_owned(),
                    target_id: "local".to_owned(),
                    activate: true,
                },
            ],
            ..ProfileDocument::default()
        };
        assert!(matches!(
            validate_profile_document(&multiple_active),
            Err(ProfileError::InvalidInput { message }) if message.contains("at most one")
        ));
    }

    #[test]
    fn inline_source_validation_rejects_empty_instruction_text() {
        let source = ProfileSource::Inline {
            profile: InlineAgentProfile {
                display_name: None,
                description: None,
                document: ProfileDocument {
                    instructions: Some(ProfileInstructions::Text {
                        text: " ".to_owned(),
                    }),
                    ..ProfileDocument::default()
                },
            },
        };

        assert!(matches!(
            source.validate(),
            Err(ProfileError::InvalidInput { message })
                if message.contains("instructions.text")
        ));
    }

    #[test]
    fn update_patch_applies_and_increments_revision() {
        let profile = AgentProfile {
            profile_id: ProfileId::new("support"),
            display_name: Some("Support".to_owned()),
            description: None,
            revision: 7,
            document: ProfileDocument::default(),
            created_at_ms: 10,
            updated_at_ms: 10,
        };
        let updated = AgentProfileUpdatePatch {
            display_name: Some(FieldPatch::Set("Support v2".to_owned())),
            config: Some(FieldPatch::Set(SessionConfigInput {
                tools: Some(ToolConfigInput {
                    messaging: Some(true),
                    ..ToolConfigInput::default()
                }),
                ..SessionConfigInput::default()
            })),
            ..AgentProfileUpdatePatch::default()
        }
        .apply_to(profile, 20)
        .expect("patch should apply");

        assert_eq!(updated.display_name.as_deref(), Some("Support v2"));
        assert_eq!(updated.revision, 8);
        assert_eq!(updated.updated_at_ms, 20);
        assert_eq!(
            updated.document.config.and_then(|config| config.tools),
            Some(ToolConfigInput {
                messaging: Some(true),
                ..ToolConfigInput::default()
            })
        );
    }
}
