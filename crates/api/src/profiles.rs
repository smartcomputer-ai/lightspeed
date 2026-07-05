use super::*;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProfileId(String);

impl ProfileId {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        Self::try_new(value).unwrap_or_else(|error| panic!("invalid ProfileId: {error}"))
    }

    pub fn try_new(value: impl Into<String>) -> Result<Self, ProfileIdError> {
        let value = value.into();
        validate_profile_id(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ProfileId {
    type Error = ProfileIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl TryFrom<&str> for ProfileId {
    type Error = ProfileIdError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl FromStr for ProfileId {
    type Err = ProfileIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_new(value)
    }
}

impl fmt::Display for ProfileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for ProfileId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ProfileId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_new(value).map_err(de::Error::custom)
    }
}

impl JsonSchema for ProfileId {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ProfileId".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        String::json_schema(generator)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProfileIdError {
    #[error("profile id must not be empty")]
    Empty,
    #[error("profile id must start with an ASCII alphanumeric character")]
    InvalidStart,
    #[error("profile id contains invalid character {ch:?} at byte {index}")]
    InvalidCharacter { index: usize, ch: char },
    #[error("profile id must be at most 128 bytes")]
    TooLong,
}

fn validate_profile_id(value: &str) -> Result<(), ProfileIdError> {
    if value.is_empty() {
        return Err(ProfileIdError::Empty);
    }
    if value.len() > 128 {
        return Err(ProfileIdError::TooLong);
    }
    let Some(first) = value.chars().next() else {
        return Err(ProfileIdError::Empty);
    };
    if !first.is_ascii_alphanumeric() {
        return Err(ProfileIdError::InvalidStart);
    }
    for (index, ch) in value.char_indices() {
        if !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':')) {
            return Err(ProfileIdError::InvalidCharacter { index, ch });
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentProfileInput {
    pub profile_id: ProfileId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(flatten)]
    pub document: ProfileDocument,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InlineAgentProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(flatten)]
    pub document: ProfileDocument,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentProfile {
    pub profile_id: ProfileId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub revision: u64,
    #[serde(flatten)]
    pub document: ProfileDocument,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl AgentProfile {
    pub fn summary(&self) -> AgentProfileSummary {
        AgentProfileSummary {
            profile_id: self.profile_id.clone(),
            display_name: self.display_name.clone(),
            description: self.description.clone(),
            revision: self.revision,
            updated_at_ms: self.updated_at_ms,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentProfileSummary {
    pub profile_id: ProfileId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub revision: u64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<SessionConfigInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<ProfileInstructions>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<ProfileMount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp: Vec<ProfileMcpLink>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environments: Vec<ProfileEnvironment>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ProfileInstructions {
    Text { text: String },
    TextRef { blob_ref: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileMount {
    pub mount_path: String,
    pub source: VfsMountSourceInput,
    pub access: VfsMountAccess,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileMcpLink {
    pub server_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<RemoteMcpApprovalPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defer_loading: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_grant_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileEnvironment {
    pub env_id: EnvironmentId,
    pub provider_id: EnvironmentProviderId,
    pub target_id: EnvironmentTargetId,
    #[serde(default)]
    pub activate: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ProfileSource {
    Named {
        #[serde(alias = "profile_id")]
        profile_id: ProfileId,
    },
    Inline {
        profile: InlineAgentProfile,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentProfileUpdatePatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<FieldPatch<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<FieldPatch<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<FieldPatch<SessionConfigInput>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<FieldPatch<ProfileInstructions>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mounts: Option<Vec<ProfileMount>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<Vec<ProfileMcpLink>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environments: Option<Vec<ProfileEnvironment>>,
}

impl AgentProfileUpdatePatch {
    pub fn is_empty(&self) -> bool {
        self.display_name.is_none()
            && self.description.is_none()
            && self.config.is_none()
            && self.instructions.is_none()
            && self.mounts.is_none()
            && self.mcp.is_none()
            && self.environments.is_none()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileCreateParams {
    pub profile: AgentProfileInput,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileCreateResponse {
    pub profile: AgentProfile,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileReadParams {
    pub profile_id: ProfileId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileReadResponse {
    pub profile: AgentProfile,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileListParams {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileListResponse {
    #[serde(default)]
    pub profiles: Vec<AgentProfileSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfilePutParams {
    pub profile: AgentProfileInput,
    /// Checked only when the profile already exists; absent replaces (or
    /// creates) unconditionally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfilePutResponse {
    pub profile: AgentProfile,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileUpdateParams {
    pub profile_id: ProfileId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    #[serde(default)]
    pub patch: AgentProfileUpdatePatch,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileUpdateResponse {
    pub profile: AgentProfile,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDeleteParams {
    pub profile_id: ProfileId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileDeleteResponse {
    pub profile: AgentProfile,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileApplyParams {
    pub session_id: SessionId,
    pub profile: ProfileSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_config_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_tools_revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileApplyResponse {
    pub session: SessionView,
    #[serde(default)]
    pub applied: ProfileApplySummary,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileApplySummary {
    pub config_changed: bool,
    pub instructions_changed: bool,
    pub mounts_changed: u32,
    pub mcp_changed: u32,
    pub environments_changed: u32,
}
