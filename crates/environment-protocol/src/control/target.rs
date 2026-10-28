//! `target/*`: create, observe, power and close provider targets.

use serde::{Deserialize, Serialize};

use crate::{
    idle::IdlePolicy,
    ids::{EnvironmentId, ProviderTargetId, TemplateId, UniverseId},
    metadata::{DescriptiveText, DisplayName},
    routes::PublicUrl,
};

/// The Lightspeed environment a target belongs to. A provider owns at most
/// one target per owner.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TargetOwner {
    pub universe_id: UniverseId,
    pub environment_id: EnvironmentId,
}

/// A reference to an existing target: its owner and, once known, its id.
/// Without `targetId` the provider resolves the owner's target itself.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TargetRef {
    pub owner: TargetOwner,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<ProviderTargetId>,
}

/// What the provider observes about a target. Every target and ingress
/// method answers with one; Lightspeed records it for display only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TargetObservation {
    /// The provider's id for the target. Absent in two cases only: while
    /// `phase` is `provisioning` and the provider has not assigned an id
    /// yet (an asynchronous create answers before it has one; the runtime
    /// records the id from the first observation that carries it), and in a
    /// `closed` observation of an owner the provider holds no target for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<ProviderTargetId>,
    /// Echoed from the request.
    pub owner: TargetOwner,
    pub phase: TargetPhase,
    /// Present exactly when `phase` is `active`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power: Option<PowerState>,
    /// Why the target is `failed` or `unknown`, or what a transition waits
    /// for. Providers build it with [`DescriptiveText::lossy`] from
    /// whatever their backend reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<DescriptiveText>,
    /// Public ingress as realized; absent when disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingress: Option<IngressObservation>,
}

/// Realized public ingress of a target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IngressObservation {
    /// Public URL that reaches the target's application.
    pub endpoint: PublicUrl,
}

/// Lifecycle phase of a target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum TargetPhase {
    /// Being created; not yet usable.
    Provisioning,
    /// Exists; see `power`.
    Active,
    /// Being destroyed.
    Closing,
    /// Gone, and nothing of it remains.
    Closed,
    /// Broken beyond the provider's own recovery; see `message`.
    Failed,
    /// The provider cannot tell; see `message`.
    Unknown,
}

/// Steady power state of an active target, in provider-neutral terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum PowerState {
    Running,
    /// Execution halted, memory retained; resuming is fast.
    Paused,
    /// Execution state persisted, compute released; resuming restores it.
    Suspended,
    /// Powered off; disks retained.
    Stopped,
}

impl PowerState {
    pub const ALL: [PowerState; 4] = [
        PowerState::Running,
        PowerState::Paused,
        PowerState::Suspended,
        PowerState::Stopped,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Suspended => "suspended",
            Self::Stopped => "stopped",
        }
    }
}

impl std::fmt::Display for PowerState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// `target/create` params. Idempotent per owner: when the owner already
/// has a target, the provider returns its observation and applies no
/// creation policy (template deprecation, image checks). Handing out a
/// target from a warm pool counts as a create.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateTargetParams {
    pub owner: TargetOwner,
    pub template_id: TemplateId,
    /// Label for the target, where the provider shows one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<DisplayName>,
    /// Overrides the template's `idle` for this target. A policy that does
    /// not validate ([`IdlePolicy::validate`]) is `invalidParams`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle: Option<IdlePolicy>,
}

/// `target/get` params. A pure read: it never creates or changes anything.
/// A target that no longer exists is `notFound`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GetTargetParams {
    pub target: TargetRef,
}

/// `target/setPower` params: move the target to `power`. Requires
/// `capabilities.power`. A target already in, or moving toward, that state
/// is observed without further action. Resuming may replace the underlying
/// host process. A state outside the template's `powerStates` is
/// `invalidParams`; a target whose `phase` is not `active` is
/// `failedPrecondition` (`targetNotActive`). The provider stays the owner
/// of power afterwards: its own idle policy may move the target again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetTargetPowerParams {
    pub target: TargetRef,
    /// One of the template's `powerStates`.
    pub power: PowerState,
}

/// `target/close` params: destroy the owner's target and every resource it
/// holds, public ingress included. Idempotent. Graceful within the
/// provider's own bound, then forced. Answers `closing` until the target is
/// gone, then `closed`; `closed` also when the provider holds no target for
/// the owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloseTargetParams {
    pub target: TargetRef,
}

/// Response of every target method and both ingress methods.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TargetResponse {
    pub target: TargetObservation,
}
