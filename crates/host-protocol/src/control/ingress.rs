//! Provider-owned public ingress lifecycle payloads.

use serde::{Deserialize, Serialize};

use crate::{control::targets::ProviderBindingContext, shared::HostTargetId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnsureIngressParams {
    pub request_id: String,
    pub environment_id: String,
    pub incarnation_id: String,
    pub binding: ProviderBindingContext,
    pub target_id: HostTargetId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveIngressParams {
    pub request_id: String,
    pub environment_id: String,
    pub incarnation_id: String,
    pub binding: ProviderBindingContext,
    pub target_id: HostTargetId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderIngressStatus {
    Ready,
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngressResponse {
    pub status: ProviderIngressStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_endpoint: Option<String>,
}
