use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{BlobRef, CoreAgentState, DomainError, RunId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AwaitOutcome {
    Terminal,
    Timeout,
    Cancelled,
}

/// Stable identifier for a promise: a session-scoped counter rendered as
/// `promise_<n>`, the same convention as `run_<n>`, so the model copies a
/// short handle rather than a digest. The harness hands every tool batch a
/// base one past the session cursor (`ToolInvocationBatchRequest::
/// promise_id_base`); the executor numbers the promises it creates from
/// that base, and the reducer accepts a creation only at or above the
/// batch's base and never twice. Producer correlation lives on
/// `PromiseSource`, not in the id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "contract", derive(schemars::JsonSchema))]
pub struct PromiseId(String);

pub const PROMISE_ID_PREFIX: &str = "promise_";

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("promise id must use promise_<number> form: {value:?}")]
pub struct PromiseIdError {
    pub value: String,
}

impl PromiseId {
    pub fn from_number(number: u64) -> Self {
        Self(format!("{PROMISE_ID_PREFIX}{number}"))
    }

    pub fn try_new(value: impl Into<String>) -> Result<Self, PromiseIdError> {
        let value = value.into();
        match parse_promise_number(&value) {
            Some(_) => Ok(Self(value)),
            None => Err(PromiseIdError { value }),
        }
    }

    /// Trusted constructor for ids the harness minted itself; panics on a
    /// malformed value. Untrusted input (model arguments, emissions) goes
    /// through `try_new`.
    pub fn new(value: impl Into<String>) -> Self {
        Self::try_new(value).unwrap_or_else(|error| panic!("{error}"))
    }

    pub fn number(&self) -> u64 {
        parse_promise_number(&self.0).expect("promise id was validated at construction")
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn parse_promise_number(value: &str) -> Option<u64> {
    let digits = value.strip_prefix(PROMISE_ID_PREFIX)?;
    if digits.is_empty()
        || digits.len() > 20
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
        || (digits.len() > 1 && digits.starts_with('0'))
    {
        return None;
    }
    digits.parse::<u64>().ok()
}

impl PartialOrd for PromiseId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PromiseId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.number().cmp(&other.number())
    }
}

impl std::fmt::Display for PromiseId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for PromiseId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for PromiseId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_new(value).map_err(serde::de::Error::custom)
    }
}

/// Hands out promise ids to the executors of one tool batch dispatch,
/// counting up from the batch's base. Executors of parallel calls share
/// one allocator; the order they draw in is not replayed, only the
/// recorded result is.
#[derive(Debug)]
pub struct PromiseIdAllocator {
    next: AtomicU64,
}

impl PromiseIdAllocator {
    pub fn new(base: u64) -> Self {
        Self {
            next: AtomicU64::new(base),
        }
    }

    pub fn allocate(&self) -> PromiseId {
        PromiseId::from_number(self.next.fetch_add(1, Ordering::Relaxed))
    }
}

/// What produces the resolution of a promise. Provider-native detail stays
/// opaque; the harness keeps only the facts needed for deterministic
/// branching and outward cancellation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PromiseSource {
    /// A durable timer owned by the session workflow.
    Timer { fire_at_ms: u64 },
    /// One keyed completion promise of a workflow-tool invocation.
    /// `producer` is the only workflow authorized to resolve it: the
    /// admitted bound receiver, or the system-derived started execution.
    /// Whether cancellation targets a shared receiver or an owned execution
    /// is derived from the durable binding's target lifecycle, not from a
    /// second source variant.
    Workflow {
        producer_workflow_id: String,
        producer_workflow_kind: String,
        invocation_id: String,
        completion_key: String,
    },
}

/// Ownership scope (structured concurrency): run-scoped promises auto-cancel
/// when their run reaches a terminal state; session-scoped (detached)
/// promises count as active work until the session closes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PromiseScope {
    Run { run_id: RunId },
    Session,
}

/// Who may control a Promise through model-facing concurrency tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromiseOwnership {
    /// The Promise ID may be exposed to and controlled by the model.
    Model,
    /// The runtime owns completion; the model may not await, cancel, or detach it.
    Runtime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromiseStatus {
    Pending,
    Resolved,
    Failed,
    Cancelled,
}

impl PromiseStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, PromiseStatus::Pending)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Promise {
    pub promise_id: PromiseId,
    pub source: PromiseSource,
    pub scope: PromiseScope,
    pub ownership: PromiseOwnership,
    pub status: PromiseStatus,
    /// Resolution payload (CAS ref); set only when `status == Resolved`.
    pub payload_ref: Option<BlobRef>,
    /// Failure detail (CAS ref); set only when `status == Failed`.
    pub error_ref: Option<BlobRef>,
    /// Harness-owned hard deadline, independent of any await.
    pub deadline_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromiseComponentState {
    pub promises: BTreeMap<PromiseId, Promise>,
}

impl PromiseComponentState {
    pub fn pending(&self) -> impl Iterator<Item = &Promise> {
        self.promises
            .values()
            .filter(|promise| promise.status == PromiseStatus::Pending)
    }

    pub fn pending_for_run(&self, run_id: RunId) -> impl Iterator<Item = &Promise> {
        self.pending()
            .filter(move |promise| promise.scope == PromiseScope::Run { run_id })
    }
}

/// How a promise reached a terminal state. Used by `ResolvePromise`
/// admission; all transports (push notifications, poll results, timers,
/// cancellation) converge on this one funnel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
#[cfg_attr(feature = "contract", derive(schemars::JsonSchema))]
pub enum PromiseResolution {
    Resolved { payload_ref: Option<BlobRef> },
    Failed { error_ref: Option<BlobRef> },
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    Created {
        promise: Promise,
    },
    Resolved {
        promise_id: PromiseId,
        payload_ref: Option<BlobRef>,
    },
    Failed {
        promise_id: PromiseId,
        error_ref: Option<BlobRef>,
    },
    Cancelled {
        promise_id: PromiseId,
    },
    Detached {
        promise_id: PromiseId,
    },
}

pub type PromiseEvent = Event;

pub(crate) fn apply_event(state: &mut CoreAgentState, event: &Event) -> Result<(), DomainError> {
    match event {
        Event::Created { promise } => {
            if promise.status != PromiseStatus::Pending {
                return Err(DomainError::InvariantViolation(
                    "promises are created pending".into(),
                ));
            }
            if state.promises.promises.contains_key(&promise.promise_id) {
                return Err(DomainError::InvariantViolation(format!(
                    "duplicate promise id {}",
                    promise.promise_id
                )));
            }
            if promise.ownership == PromiseOwnership::Runtime
                && (!matches!(promise.source, PromiseSource::Workflow { .. })
                    || !matches!(promise.scope, PromiseScope::Run { .. }))
            {
                return Err(DomainError::InvariantViolation(format!(
                    "runtime-owned promise {} must be a run-scoped workflow completion",
                    promise.promise_id
                )));
            }
            if let PromiseScope::Run { run_id } = promise.scope {
                let owned_by_active = state
                    .runs
                    .active
                    .as_ref()
                    .is_some_and(|active| active.run_id == run_id);
                if !owned_by_active {
                    return Err(DomainError::InvariantViolation(format!(
                        "promise {} is scoped to run {} which is not active",
                        promise.promise_id, run_id
                    )));
                }
            }
            state.id_cursors.last_promise_id = state
                .id_cursors
                .last_promise_id
                .max(promise.promise_id.number());
            state
                .promises
                .promises
                .insert(promise.promise_id.clone(), promise.clone());
            Ok(())
        }
        Event::Resolved {
            promise_id,
            payload_ref,
        } => {
            let promise = pending_promise_mut(state, promise_id)?;
            promise.status = PromiseStatus::Resolved;
            promise.payload_ref = payload_ref.clone();
            Ok(())
        }
        Event::Failed {
            promise_id,
            error_ref,
        } => {
            let promise = pending_promise_mut(state, promise_id)?;
            promise.status = PromiseStatus::Failed;
            promise.error_ref = error_ref.clone();
            Ok(())
        }
        Event::Cancelled { promise_id } => {
            let promise = pending_promise_mut(state, promise_id)?;
            promise.status = PromiseStatus::Cancelled;
            Ok(())
        }
        Event::Detached { promise_id } => {
            let promise = pending_promise_mut(state, promise_id)?;
            if promise.ownership != PromiseOwnership::Model {
                return Err(DomainError::InvariantViolation(format!(
                    "runtime-owned promise {} cannot be detached",
                    promise_id
                )));
            }
            promise.scope = PromiseScope::Session;
            Ok(())
        }
    }
}

fn pending_promise_mut<'state>(
    state: &'state mut CoreAgentState,
    promise_id: &PromiseId,
) -> Result<&'state mut Promise, DomainError> {
    let promise = state.promises.promises.get_mut(promise_id).ok_or_else(|| {
        DomainError::InvariantViolation(format!("unknown promise {}", promise_id))
    })?;
    if promise.status.is_terminal() {
        return Err(DomainError::InvariantViolation(format!(
            "promise {} is already terminal",
            promise_id
        )));
    }
    Ok(promise)
}

/// Tool effect vocabulary: tool executions create promises by attaching a
/// `lightspeed.core.promise.create` effect to their call result. The drive
/// turns each effect into an explicit `Promise(Created)` event in the same
/// append as the call completion, so promise creation is log-backed and
/// replay-deterministic.
pub const PROMISE_CREATE_EFFECT_KIND: &str = "lightspeed.core.promise.create";
pub const PROMISE_CANCEL_EFFECT_KIND: &str = "lightspeed.core.promise.cancel";
pub const PROMISE_DETACH_EFFECT_KIND: &str = "lightspeed.core.promise.detach";

pub const PROMISE_EFFECT_ID: &str = "promise_id";
pub const PROMISE_EFFECT_SOURCE: &str = "source";
pub const PROMISE_EFFECT_FIRE_AT_MS: &str = "fire_at_ms";
pub const PROMISE_EFFECT_DEADLINE_MS: &str = "deadline_ms";
pub const PROMISE_EFFECT_SOURCE_TIMER: &str = "timer";
pub const PROMISE_EFFECT_SOURCE_WORKFLOW: &str = "workflow";
pub const PROMISE_EFFECT_PRODUCER_WORKFLOW_ID: &str = "producer_workflow_id";
pub const PROMISE_EFFECT_PRODUCER_WORKFLOW_KIND: &str = "producer_workflow_kind";
pub const PROMISE_EFFECT_INVOCATION_ID: &str = "invocation_id";
pub const PROMISE_EFFECT_COMPLETION_KEY: &str = "completion_key";

/// Build the creation effect a tool executor attaches to its call result.
pub fn promise_create_effect(
    promise_id: &PromiseId,
    source: &PromiseSource,
    deadline_ms: Option<u64>,
) -> crate::ToolEffect {
    let mut data = BTreeMap::new();
    data.insert(PROMISE_EFFECT_ID.to_owned(), promise_id.as_str().to_owned());
    match source {
        PromiseSource::Timer { fire_at_ms } => {
            data.insert(
                PROMISE_EFFECT_SOURCE.to_owned(),
                PROMISE_EFFECT_SOURCE_TIMER.to_owned(),
            );
            data.insert(PROMISE_EFFECT_FIRE_AT_MS.to_owned(), fire_at_ms.to_string());
        }
        PromiseSource::Workflow {
            producer_workflow_id,
            producer_workflow_kind,
            invocation_id,
            completion_key,
        } => {
            data.insert(
                PROMISE_EFFECT_SOURCE.to_owned(),
                PROMISE_EFFECT_SOURCE_WORKFLOW.to_owned(),
            );
            data.insert(
                PROMISE_EFFECT_PRODUCER_WORKFLOW_ID.to_owned(),
                producer_workflow_id.clone(),
            );
            data.insert(
                PROMISE_EFFECT_PRODUCER_WORKFLOW_KIND.to_owned(),
                producer_workflow_kind.clone(),
            );
            data.insert(
                PROMISE_EFFECT_INVOCATION_ID.to_owned(),
                invocation_id.clone(),
            );
            data.insert(
                PROMISE_EFFECT_COMPLETION_KEY.to_owned(),
                completion_key.clone(),
            );
        }
    }
    if let Some(deadline_ms) = deadline_ms {
        data.insert(
            PROMISE_EFFECT_DEADLINE_MS.to_owned(),
            deadline_ms.to_string(),
        );
    }
    crate::ToolEffect {
        kind: PROMISE_CREATE_EFFECT_KIND.to_owned(),
        data,
    }
}

/// Build the effect a revocation tool attaches after it has best-effort
/// cancelled the native source. The drive turns this into
/// `Promise(Cancelled)` in the same append as the tool call completion.
pub fn promise_cancel_effect(promise_id: &PromiseId) -> crate::ToolEffect {
    let mut data = BTreeMap::new();
    data.insert(PROMISE_EFFECT_ID.to_owned(), promise_id.as_str().to_owned());
    crate::ToolEffect {
        kind: PROMISE_CANCEL_EFFECT_KIND.to_owned(),
        data,
    }
}

/// Build the effect a detach tool attaches after validating ownership. The
/// drive turns this into `Promise(Detached)` in the same append as the tool
/// call completion.
pub fn promise_detach_effect(promise_id: &PromiseId) -> crate::ToolEffect {
    let mut data = BTreeMap::new();
    data.insert(PROMISE_EFFECT_ID.to_owned(), promise_id.as_str().to_owned());
    crate::ToolEffect {
        kind: PROMISE_DETACH_EFFECT_KIND.to_owned(),
        data,
    }
}

/// Decode a creation effect back into a pending promise owned by `run_id`.
/// Returns `None` for effects of other kinds; malformed promise effects are
/// invariant violations (they came from our own executors).
pub(crate) fn promise_from_create_effect(
    effect: &crate::ToolEffect,
    run_id: RunId,
) -> Result<Option<Promise>, DomainError> {
    if effect.kind != PROMISE_CREATE_EFFECT_KIND {
        return Ok(None);
    }
    let field = |key: &str| {
        effect.data.get(key).cloned().ok_or_else(|| {
            DomainError::InvariantViolation(format!("promise create effect is missing `{key}`"))
        })
    };
    let promise_id = PromiseId::try_new(field(PROMISE_EFFECT_ID)?).map_err(|error| {
        DomainError::InvariantViolation(format!("promise create effect has an invalid id: {error}"))
    })?;
    let source_kind = field(PROMISE_EFFECT_SOURCE)?;
    let parse_u64 = |key: &str, value: String| {
        value.parse::<u64>().map_err(|_| {
            DomainError::InvariantViolation(format!("promise create effect `{key}` is not a u64"))
        })
    };
    let source = match source_kind.as_str() {
        PROMISE_EFFECT_SOURCE_TIMER => PromiseSource::Timer {
            fire_at_ms: parse_u64(PROMISE_EFFECT_FIRE_AT_MS, field(PROMISE_EFFECT_FIRE_AT_MS)?)?,
        },
        PROMISE_EFFECT_SOURCE_WORKFLOW => {
            // Workflow-source promises are created only through the trusted
            // workflow-tool invocation effect, which validates them against
            // the durable binding; a generic promise-create effect must not
            // mint one.
            return Err(DomainError::InvariantViolation(
                "workflow-source promises are created by workflow-tool invocation effects, not generic promise-create effects"
                    .to_owned(),
            ));
        }
        other => {
            return Err(DomainError::InvariantViolation(format!(
                "unknown promise source kind `{other}`"
            )));
        }
    };
    let deadline_ms = effect
        .data
        .get(PROMISE_EFFECT_DEADLINE_MS)
        .map(|value| parse_u64(PROMISE_EFFECT_DEADLINE_MS, value.clone()))
        .transpose()?;
    Ok(Some(Promise {
        promise_id,
        source,
        scope: PromiseScope::Run { run_id },
        ownership: PromiseOwnership::Model,
        status: PromiseStatus::Pending,
        payload_ref: None,
        error_ref: None,
        deadline_ms,
    }))
}

pub(crate) fn promise_id_from_cancel_effect(
    effect: &crate::ToolEffect,
) -> Result<Option<PromiseId>, DomainError> {
    if effect.kind != PROMISE_CANCEL_EFFECT_KIND {
        return Ok(None);
    }
    let Some(promise_id) = effect.data.get(PROMISE_EFFECT_ID) else {
        return Err(DomainError::InvariantViolation(
            "promise cancel effect is missing `promise_id`".into(),
        ));
    };
    PromiseId::try_new(promise_id.clone())
        .map(Some)
        .map_err(|error| {
            DomainError::InvariantViolation(format!(
                "promise cancel effect has an invalid id: {error}"
            ))
        })
}

pub(crate) fn promise_id_from_detach_effect(
    effect: &crate::ToolEffect,
) -> Result<Option<PromiseId>, DomainError> {
    if effect.kind != PROMISE_DETACH_EFFECT_KIND {
        return Ok(None);
    }
    let Some(promise_id) = effect.data.get(PROMISE_EFFECT_ID) else {
        return Err(DomainError::InvariantViolation(
            "promise detach effect is missing `promise_id`".into(),
        ));
    };
    PromiseId::try_new(promise_id.clone())
        .map(Some)
        .map_err(|error| {
            DomainError::InvariantViolation(format!(
                "promise detach effect has an invalid id: {error}"
            ))
        })
}

#[cfg(test)]
mod id_tests {
    use super::*;

    #[test]
    fn promise_ids_are_canonical_counters() {
        assert_eq!(PromiseId::from_number(7).as_str(), "promise_7");
        assert_eq!(PromiseId::from_number(0).number(), 0);
        assert_eq!(
            PromiseId::try_new("promise_12")
                .expect("canonical")
                .number(),
            12
        );
        for malformed in [
            "",
            "promise_",
            "promise_07",
            "promise_a",
            "promise-7",
            "7",
            "wtp:sha256:abc",
            "promise_7 ",
            "promise_123456789012345678901",
        ] {
            assert!(PromiseId::try_new(malformed).is_err(), "{malformed:?}");
        }
    }

    #[test]
    fn promise_ids_order_numerically_and_round_trip_as_strings() {
        assert!(PromiseId::from_number(9) < PromiseId::from_number(10));
        let encoded = serde_json::to_string(&PromiseId::from_number(10)).expect("encode");
        assert_eq!(encoded, "\"promise_10\"");
        let decoded: PromiseId = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded, PromiseId::from_number(10));
        assert!(serde_json::from_str::<PromiseId>("\"promise_x\"").is_err());
    }

    #[test]
    fn allocator_counts_up_from_the_base() {
        let allocator = PromiseIdAllocator::new(4);
        assert_eq!(allocator.allocate(), PromiseId::from_number(4));
        assert_eq!(allocator.allocate(), PromiseId::from_number(5));
    }
}
