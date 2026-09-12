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
    pub config: Option<SessionConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<ProfileInstructions>,
    /// How the session obtains its active environment when this profile is
    /// applied: select an existing environment or inherit the parent's
    /// selection. Absence leaves the session's current
    /// active environment unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<ProfileEnvironment>,
    /// Descriptive metadata defaults copied to a session when it is created
    /// from this profile. Explicit `session/start` metadata wins key by key.
    /// Applying the profile to an existing session does not change metadata.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    /// Creation-time session retention default. Absence keeps the tree until
    /// manual deletion. Applying a profile to an existing session does not
    /// change retention.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<ProfileSessionRetention>,
}

/// Root-session retention policy supplied by a profile at session creation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileSessionRetention {
    /// Positive close-relative automatic-deletion duration.
    #[schemars(range(min = 1, max = 3153600000000_u64))]
    pub delete_after_close_ms: u64,
}

/// Environment intent carried by a profile document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ProfileEnvironment {
    /// Activate an existing universe environment. The profile never closes
    /// it.
    Existing { environment_id: EnvironmentId },
    /// Activate the delegating parent's active environment. Resolved at
    /// sub-agent spawn, shared not copied, never closed by the
    /// child; rejected on a session without a delegation origin or whose
    /// parent has no active environment.
    Inherit {},
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ProfileInstructions {
    Text {
        text: String,
    },
    /// Borrowed CAS content: saving a profile does not retain the blob. Use
    /// inline text, or keep this ref retained by another durable resource.
    TextRef {
        blob_ref: String,
    },
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
        profile: Box<InlineAgentProfile>,
    },
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
    pub active_environment_changed: bool,
}
