//! `ingress/*`: public ingress of one target. Requires
//! `capabilities.ingress`. `target/close` removes ingress by contract, so
//! `ingress/remove` exists only to disable ingress on a target that stays.
//! Both answer [`TargetResponse`](crate::control::target::TargetResponse)
//! with the realized ingress in the observation. The runtime calls them
//! synchronously on a user's request and caches the observed endpoint; it
//! records no ingress intent and retries nothing.

use serde::{Deserialize, Serialize};

use crate::control::target::TargetRef;

/// `ingress/ensure` params: route the template's public ingress to the
/// target. Idempotent. A target whose template has no `publicIngress` is
/// `failedPrecondition` (`ingressUnsupported`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnsureIngressParams {
    pub target: TargetRef,
}

/// `ingress/remove` params: stop routing public ingress to the target.
/// Idempotent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoveIngressParams {
    pub target: TargetRef,
}
