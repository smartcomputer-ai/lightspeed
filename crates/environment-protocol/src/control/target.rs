//! `target/*`: create, adopt, observe, power and close provider targets.

use serde::{Deserialize, Serialize};

use crate::{
    ids::{
        BindingId, EnvironmentId, IncarnationId, ProviderNativeRef, ProviderTargetId, TemplateId,
        UniverseId,
    },
    metadata::{DescriptiveText, DisplayName, Metadata},
    routes::PublicUrl,
    serde_util::is_false,
};

/// The Lightspeed incarnation a target belongs to. A provider owns at most
/// one target per owner.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TargetOwner {
    pub universe_id: UniverseId,
    pub binding_id: BindingId,
    pub environment_id: EnvironmentId,
    pub incarnation_id: IncarnationId,
}

/// The provider binding a call is scoped to, for calls that concern no
/// particular target.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindingRef {
    pub universe_id: UniverseId,
    pub binding_id: BindingId,
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

/// What the provider observes about a target. Every target method answers
/// with one.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<DisplayName>,
    pub state: TargetState,
    /// Power states `target/setPower` can move this target to. Empty when
    /// the provider offers no power control beyond create and close.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_power_states: Vec<PowerState>,
    /// Public ingress as realized; absent when disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingress: Option<IngressObservation>,
    /// Provider-specific display facts.
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    pub metadata: Metadata,
}

/// Realized public ingress of a target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IngressObservation {
    /// Public URL that reaches the target's application.
    pub endpoint: PublicUrl,
}

/// A target's state on two axes: lifecycle `phase`, and `power` while
/// active.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TargetState {
    pub phase: TargetPhase,
    /// Present exactly when `phase` is `active`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power: Option<PowerState>,
    /// envd answers on the target's data route (running targets only).
    #[serde(default, skip_serializing_if = "is_false")]
    pub daemon_ready: bool,
    /// Why the target is `failed` or `unknown`, or what a transition waits
    /// for. Providers build it with
    /// [`DescriptiveText::lossy`] from whatever their backend reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<DescriptiveText>,
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
/// creation policy (template deprecation, binding or image checks).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateTargetParams {
    pub owner: TargetOwner,
    pub template_id: TemplateId,
}

/// `target/adopt` params: take ownership of an existing provider target.
/// Idempotent per owner. Requires `capabilities.adoptTarget`. A source that
/// belongs to another owner, or an owner that already holds a different
/// target, is `failedPrecondition` (`ownerMismatch`); a source the provider
/// cannot take over is `failedPrecondition` (`notAdoptable`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdoptTargetParams {
    pub owner: TargetOwner,
    /// Provider-native reference to the existing target; the provider
    /// defines its format.
    pub source_target: ProviderNativeRef,
}

/// `target/get` params. A pure read: it never creates or changes anything.
/// A target that no longer exists is `notFound`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GetTargetParams {
    pub target: TargetRef,
}

/// `target/setPower` params: converge the target toward `power`. A target
/// already in, or moving toward, that state is observed without further
/// action. Resuming may replace the underlying host process. A target whose
/// `phase` is not `active` is `failedPrecondition` (`targetNotActive`); a
/// state outside `supportedPowerStates` is `invalidParams`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetTargetPowerParams {
    pub target: TargetRef,
    /// One of the target's `supportedPowerStates`.
    pub power: PowerState,
}

/// `target/close` params: destroy the owner's target and every resource it
/// holds, public ingress included. Graceful within the provider's own bound,
/// then forced. Answers `closing` until the target is gone, then `closed`;
/// `closed` also when the provider holds no target for the owner.
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
