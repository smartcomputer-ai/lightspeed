//! Idle policy: when a provider powers down or closes an idle target.
//!
//! The policy belongs to the provider. A template names a default
//! (`template/list` `idle`), `target/create` may override it (`idle`), and
//! the provider runs the clock: it reads envd `env/getIdle` over its own
//! guest channel and applies [`IdlePolicy::due_action`] to `idleForMs`.
//! Running processes, jobs and leftover process groups hold `idleForMs` at
//! zero, so they keep the target awake. A provider never powers a target
//! down while it relays a data connection to it. Lightspeed only validates
//! a policy and passes it through.

use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::control::target::PowerState;

/// Staged idle policy. Each set threshold is measured against envd
/// `env/getIdle` `idleForMs`, and the set thresholds are non-decreasing in
/// the order pause → suspend → stop → close. Every stage is optional; a
/// policy sets at least one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdlePolicy {
    /// Pause the target after this long idle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_after_ms: Option<u64>,
    /// Suspend the target after this long idle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspend_after_ms: Option<u64>,
    /// Stop the target after this long idle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_after_ms: Option<u64>,
    /// Close the target after this long idle; the provider then reports it
    /// `closed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_after_ms: Option<u64>,
}

/// One stage of an idle policy, in escalation order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IdleAction {
    Pause,
    Suspend,
    Stop,
    Close,
}

impl IdleAction {
    /// The power state this stage moves a target to; `None` for close.
    pub fn power_state(self) -> Option<PowerState> {
        match self {
            Self::Pause => Some(PowerState::Paused),
            Self::Suspend => Some(PowerState::Suspended),
            Self::Stop => Some(PowerState::Stopped),
            Self::Close => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Suspend => "suspend",
            Self::Stop => "stop",
            Self::Close => "close",
        }
    }
}

impl fmt::Display for IdleAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Why an idle policy is invalid.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum IdlePolicyError {
    #[error("idle policy sets no stage")]
    Empty,

    #[error("idle policy {0} threshold must be positive")]
    ZeroThreshold(IdleAction),

    #[error("idle policy {later} threshold must not be below {earlier}")]
    OutOfOrder {
        earlier: IdleAction,
        later: IdleAction,
    },
}

impl IdlePolicy {
    pub fn is_empty(&self) -> bool {
        self.pause_after_ms.is_none()
            && self.suspend_after_ms.is_none()
            && self.stop_after_ms.is_none()
            && self.close_after_ms.is_none()
    }

    /// The set stages in escalation order, with their thresholds.
    pub fn stages(&self) -> Vec<(IdleAction, u64)> {
        [
            (IdleAction::Pause, self.pause_after_ms),
            (IdleAction::Suspend, self.suspend_after_ms),
            (IdleAction::Stop, self.stop_after_ms),
            (IdleAction::Close, self.close_after_ms),
        ]
        .into_iter()
        .filter_map(|(action, threshold)| threshold.map(|threshold| (action, threshold)))
        .collect()
    }

    /// At least one stage, every threshold positive, thresholds
    /// non-decreasing in escalation order.
    pub fn validate(&self) -> Result<(), IdlePolicyError> {
        if self.is_empty() {
            return Err(IdlePolicyError::Empty);
        }
        let mut previous: Option<(IdleAction, u64)> = None;
        for (action, threshold) in self.stages() {
            if threshold == 0 {
                return Err(IdlePolicyError::ZeroThreshold(action));
            }
            if let Some((earlier, earlier_threshold)) = previous
                && threshold < earlier_threshold
            {
                return Err(IdlePolicyError::OutOfOrder {
                    earlier,
                    later: action,
                });
            }
            previous = Some((action, threshold));
        }
        Ok(())
    }

    /// The most escalated stage whose threshold `idle_for_ms` has crossed
    /// and whose power state is in `supported` (`close` always qualifies);
    /// `None` while no stage applies.
    pub fn due_action(&self, idle_for_ms: u64, supported: &[PowerState]) -> Option<IdleAction> {
        self.stages()
            .into_iter()
            .filter(|(_, threshold)| idle_for_ms >= *threshold)
            .filter(|(action, _)| {
                action
                    .power_state()
                    .is_none_or(|state| supported.contains(&state))
            })
            .map(|(action, _)| action)
            .next_back()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn full() -> IdlePolicy {
        IdlePolicy {
            pause_after_ms: Some(10),
            suspend_after_ms: Some(20),
            stop_after_ms: Some(30),
            close_after_ms: Some(40),
        }
    }

    #[test]
    fn validation_requires_a_positive_non_decreasing_stage() {
        assert_eq!(full().validate(), Ok(()));
        assert_eq!(
            IdlePolicy::default().validate(),
            Err(IdlePolicyError::Empty)
        );
        assert_eq!(
            IdlePolicy {
                pause_after_ms: Some(0),
                ..IdlePolicy::default()
            }
            .validate(),
            Err(IdlePolicyError::ZeroThreshold(IdleAction::Pause))
        );
        assert_eq!(
            IdlePolicy {
                pause_after_ms: Some(30),
                stop_after_ms: Some(10),
                ..IdlePolicy::default()
            }
            .validate(),
            Err(IdlePolicyError::OutOfOrder {
                earlier: IdleAction::Pause,
                later: IdleAction::Stop
            })
        );
        assert_eq!(
            IdlePolicy {
                stop_after_ms: Some(10),
                close_after_ms: Some(10),
                ..IdlePolicy::default()
            }
            .validate(),
            Ok(()),
            "equal thresholds are allowed"
        );
    }

    #[test]
    fn the_most_escalated_supported_stage_is_due() {
        let policy = full();
        let incus = [PowerState::Running, PowerState::Paused, PowerState::Stopped];
        assert_eq!(policy.due_action(5, &incus), None);
        assert_eq!(policy.due_action(10, &incus), Some(IdleAction::Pause));
        // Suspend is due but unsupported: pause stays the most escalated
        // applicable stage.
        assert_eq!(policy.due_action(25, &incus), Some(IdleAction::Pause));
        assert_eq!(policy.due_action(30, &incus), Some(IdleAction::Stop));
        assert_eq!(policy.due_action(40, &incus), Some(IdleAction::Close));
        // Close needs no power support.
        assert_eq!(policy.due_action(40, &[]), Some(IdleAction::Close));
        assert_eq!(policy.due_action(30, &[]), None);
    }

    #[test]
    fn the_wire_shape_is_camel_case_and_strict() {
        let policy = IdlePolicy {
            pause_after_ms: Some(600_000),
            close_after_ms: Some(86_400_000),
            ..IdlePolicy::default()
        };
        let value = serde_json::to_value(&policy).unwrap();
        assert_eq!(
            value,
            json!({"pauseAfterMs": 600_000, "closeAfterMs": 86_400_000})
        );
        assert_eq!(serde_json::from_value::<IdlePolicy>(value).unwrap(), policy);
        assert!(serde_json::from_value::<IdlePolicy>(json!({"pauseAfter": 1})).is_err());
        assert_eq!(IdleAction::Suspend.to_string(), "suspend");
        assert_eq!(IdleAction::Close.power_state(), None);
    }
}
