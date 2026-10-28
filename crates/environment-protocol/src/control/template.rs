//! `template/list`: the templates a universe may create targets from.

use serde::{Deserialize, Serialize};

use crate::{
    control::target::PowerState,
    idle::IdlePolicy,
    ids::{TemplateId, UniverseId},
    metadata::{DescriptiveText, DisplayName, Metadata},
    serde_util::is_false,
};

/// `template/list` params: the universe whose targets the templates would
/// create. A provider may offer each universe a different set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListTemplatesParams {
    pub universe_id: UniverseId,
}

/// `template/list` response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListTemplatesResponse {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub templates: Vec<EnvironmentTemplate>,
}

/// A template targets are created from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentTemplate {
    pub template_id: TemplateId,
    pub display_name: DisplayName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<DescriptiveText>,
    /// Power states `target/setPower` can move this template's targets to.
    /// Empty when the provider offers no power control beyond create and
    /// close.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub power_states: Vec<PowerState>,
    /// Targets of this template can serve public ingress.
    #[serde(default, skip_serializing_if = "is_false")]
    pub public_ingress: bool,
    /// Idle policy targets get when `target/create` names none; absent when
    /// the provider applies none by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle: Option<IdlePolicy>,
    /// Creates no new targets; existing targets keep working.
    #[serde(default, skip_serializing_if = "is_false")]
    pub deprecated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<TemplateResources>,
    /// Provider-specific display facts, such as an image fingerprint.
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    pub metadata: Metadata,
}

/// Compute a template's targets get, in provider-neutral units. Each
/// field is absent when the provider does not fix it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplateResources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcpus: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_bytes: Option<u64>,
}
