//! Controller-plane target lifecycle payloads.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::shared::{HostCapabilities, HostConnectionSpec, HostPath, HostScope, HostTargetId};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTargetsParams {
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
    pub request: HostTargetCreateRequest,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTargetResponse {
    pub target: HostTargetSummary,
    pub connection: HostConnectionSpec,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachTargetParams {
    pub request: HostTargetAttachRequest,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachTargetResponse {
    pub target: HostTargetSummary,
    pub connection: HostConnectionSpec,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTargetParams {
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HostTargetCreateRequest {
    Sandbox {
        spec: SandboxTargetSpec,
    },
    AttachedHost {
        spec: AttachedHostSpec,
    },
    Provider {
        #[serde(rename = "providerType")]
        provider_type: String,
        spec: serde_json::Value,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HostTargetAttachRequest {
    Target {
        #[serde(rename = "targetId")]
        target_id: HostTargetId,
    },
    Provider {
        #[serde(rename = "providerType")]
        provider_type: String,
        spec: serde_json::Value,
    },
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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachedHostSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<HostPath>,
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
