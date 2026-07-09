use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{BlobRef, CoreAgentState, DomainError, RunId};

/// Stable identifier for a promise. Minted by the tool executor that creates
/// the promise (a deterministic digest of the creating call context) and
/// carried in the creation event, so replay re-derives identical ids.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PromiseId(String);

impl PromiseId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PromiseId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What produces the resolution of a promise. Provider-native detail stays
/// opaque; the engine keeps only the facts needed for deterministic
/// branching and outward cancellation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PromiseSource {
    /// A run in another session; resolved by that run's terminal
    /// notification (push transport). The creating tool decides whether that
    /// run came from spawn or request.
    Run {
        target_session_id: String,
        target_run_id: u64,
    },
    /// A durable environment job; resolved by poll + nudge (P86 transport).
    EnvJob { instance_id: String, job_id: String },
    /// A durable timer owned by the session workflow.
    Timer { fire_at_ms: u64 },
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
    pub status: PromiseStatus,
    /// Resolution payload (CAS ref); set only when `status == Resolved`.
    pub payload_ref: Option<BlobRef>,
    /// Failure detail (CAS ref); set only when `status == Failed`.
    pub error_ref: Option<BlobRef>,
    /// Engine-owned hard deadline, independent of any await.
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
pub const PROMISE_EFFECT_TARGET_SESSION_ID: &str = "target_session_id";
pub const PROMISE_EFFECT_TARGET_RUN_ID: &str = "target_run_id";
pub const PROMISE_EFFECT_INSTANCE_ID: &str = "instance_id";
pub const PROMISE_EFFECT_JOB_ID: &str = "job_id";
pub const PROMISE_EFFECT_FIRE_AT_MS: &str = "fire_at_ms";
pub const PROMISE_EFFECT_DEADLINE_MS: &str = "deadline_ms";
pub const PROMISE_EFFECT_SOURCE_RUN: &str = "run";
pub const PROMISE_EFFECT_SOURCE_ENV_JOB: &str = "env_job";
pub const PROMISE_EFFECT_SOURCE_TIMER: &str = "timer";

/// Build the creation effect a tool executor attaches to its call result.
pub fn promise_create_effect(
    promise_id: &PromiseId,
    source: &PromiseSource,
    deadline_ms: Option<u64>,
) -> crate::ToolEffect {
    let mut data = BTreeMap::new();
    data.insert(PROMISE_EFFECT_ID.to_owned(), promise_id.as_str().to_owned());
    match source {
        PromiseSource::Run {
            target_session_id,
            target_run_id,
        } => {
            data.insert(
                PROMISE_EFFECT_SOURCE.to_owned(),
                PROMISE_EFFECT_SOURCE_RUN.to_owned(),
            );
            data.insert(
                PROMISE_EFFECT_TARGET_SESSION_ID.to_owned(),
                target_session_id.clone(),
            );
            data.insert(
                PROMISE_EFFECT_TARGET_RUN_ID.to_owned(),
                target_run_id.to_string(),
            );
        }
        PromiseSource::EnvJob {
            instance_id,
            job_id,
        } => {
            data.insert(
                PROMISE_EFFECT_SOURCE.to_owned(),
                PROMISE_EFFECT_SOURCE_ENV_JOB.to_owned(),
            );
            data.insert(PROMISE_EFFECT_INSTANCE_ID.to_owned(), instance_id.clone());
            data.insert(PROMISE_EFFECT_JOB_ID.to_owned(), job_id.clone());
        }
        PromiseSource::Timer { fire_at_ms } => {
            data.insert(
                PROMISE_EFFECT_SOURCE.to_owned(),
                PROMISE_EFFECT_SOURCE_TIMER.to_owned(),
            );
            data.insert(PROMISE_EFFECT_FIRE_AT_MS.to_owned(), fire_at_ms.to_string());
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
    let promise_id = PromiseId::new(field(PROMISE_EFFECT_ID)?);
    let source_kind = field(PROMISE_EFFECT_SOURCE)?;
    let parse_u64 = |key: &str, value: String| {
        value.parse::<u64>().map_err(|_| {
            DomainError::InvariantViolation(format!("promise create effect `{key}` is not a u64"))
        })
    };
    let source = match source_kind.as_str() {
        PROMISE_EFFECT_SOURCE_RUN => PromiseSource::Run {
            target_session_id: field(PROMISE_EFFECT_TARGET_SESSION_ID)?,
            target_run_id: parse_u64(
                PROMISE_EFFECT_TARGET_RUN_ID,
                field(PROMISE_EFFECT_TARGET_RUN_ID)?,
            )?,
        },
        PROMISE_EFFECT_SOURCE_ENV_JOB => PromiseSource::EnvJob {
            instance_id: field(PROMISE_EFFECT_INSTANCE_ID)?,
            job_id: field(PROMISE_EFFECT_JOB_ID)?,
        },
        PROMISE_EFFECT_SOURCE_TIMER => PromiseSource::Timer {
            fire_at_ms: parse_u64(PROMISE_EFFECT_FIRE_AT_MS, field(PROMISE_EFFECT_FIRE_AT_MS)?)?,
        },
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
    Ok(Some(PromiseId::new(promise_id.clone())))
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
    Ok(Some(PromiseId::new(promise_id.clone())))
}
