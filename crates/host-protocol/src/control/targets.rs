//! Controller-plane target lifecycle payloads.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::shared::{HostCapabilities, HostPath, HostScope, HostTargetId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderBindingContext {
    pub universe_id: String,
    pub binding_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentTemplate {
    pub template_id: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub public_ingress: bool,
    #[serde(default)]
    pub deprecated: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTemplatesParams {
    pub binding: ProviderBindingContext,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTemplatesResponse {
    pub templates: Vec<EnvironmentTemplate>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTargetsParams {
    pub binding: ProviderBindingContext,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<HostTargetStatus>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTargetsResponse {
    pub targets: Vec<HostTargetSummary>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTargetParams {
    pub request_id: String,
    pub environment_id: String,
    pub incarnation_id: String,
    pub binding: ProviderBindingContext,
    pub template_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTargetResponse {
    pub target: HostTargetSummary,
}

/// Explicit ownership transfer of an existing provider target into a
/// Lightspeed-managed binding namespace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdoptTargetParams {
    pub request_id: String,
    pub environment_id: String,
    pub incarnation_id: String,
    pub binding: ProviderBindingContext,
    /// Provider-native reference to the existing target. Incus uses
    /// `<project>/<instance>` and defaults the project to `default`.
    pub source_target: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdoptTargetResponse {
    pub target: HostTargetSummary,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTargetParams {
    pub binding: ProviderBindingContext,
    pub target_id: HostTargetId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTargetResponse {
    pub target: HostTargetSummary,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseTargetParams {
    pub request_id: String,
    pub environment_id: String,
    pub incarnation_id: String,
    pub binding: ProviderBindingContext,
    pub target_id: HostTargetId,
    #[serde(default)]
    pub force: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloseTargetResponse {
    pub target_id: HostTargetId,
    pub status: HostTargetStatus,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxTargetSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<HostPath>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_options: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostTargetSummary {
    pub target_id: HostTargetId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub status: HostTargetStatus,
    pub scope: HostScope,
    pub capabilities: HostCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_cwd: Option<HostPath>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HostTargetStatus {
    Creating,
    Starting,
    Ready,
    Stopped,
    Closing,
    Closed,
    Failed,
    Unknown,
}
