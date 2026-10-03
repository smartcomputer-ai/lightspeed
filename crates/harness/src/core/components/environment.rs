use serde::{Deserialize, Serialize};

use crate::{CoreAgentState, DomainError, EnvironmentId, ToolEffect};

pub const ENVIRONMENT_ACTIVATE_EFFECT_KIND: &str = "lightspeed.environment.activate";
pub const ENVIRONMENT_DEACTIVATE_EFFECT_KIND: &str = "lightspeed.environment.deactivate";
const ENVIRONMENT_ID_EFFECT_KEY: &str = "environment_id";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_environment_id: Option<EnvironmentId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentEvent {
    ActiveEnvironmentSet { environment_id: EnvironmentId },
    ActiveEnvironmentCleared,
}

pub(crate) fn apply_event(
    state: &mut CoreAgentState,
    event: &EnvironmentEvent,
) -> Result<(), DomainError> {
    match event {
        EnvironmentEvent::ActiveEnvironmentSet { environment_id } => {
            state.environment.active_environment_id = Some(environment_id.clone());
        }
        EnvironmentEvent::ActiveEnvironmentCleared => {
            state.environment.active_environment_id = None;
        }
    }
    Ok(())
}

pub fn environment_activate_effect(environment_id: &EnvironmentId) -> ToolEffect {
    ToolEffect {
        kind: ENVIRONMENT_ACTIVATE_EFFECT_KIND.to_owned(),
        data: [(
            ENVIRONMENT_ID_EFFECT_KEY.to_owned(),
            environment_id.as_str().to_owned(),
        )]
        .into_iter()
        .collect(),
    }
}

pub fn environment_deactivate_effect() -> ToolEffect {
    ToolEffect {
        kind: ENVIRONMENT_DEACTIVATE_EFFECT_KIND.to_owned(),
        data: Default::default(),
    }
}

pub(crate) fn is_environment_selection_effect(effect: &ToolEffect) -> bool {
    matches!(
        effect.kind.as_str(),
        ENVIRONMENT_ACTIVATE_EFFECT_KIND | ENVIRONMENT_DEACTIVATE_EFFECT_KIND
    )
}

pub(crate) fn environment_event_from_effect(
    effect: &ToolEffect,
) -> Result<Option<EnvironmentEvent>, DomainError> {
    match effect.kind.as_str() {
        ENVIRONMENT_ACTIVATE_EFFECT_KIND => {
            if effect.data.len() != 1 {
                return Err(DomainError::InvariantViolation(
                    "environment activate effect must contain only environment_id".to_owned(),
                ));
            }
            let environment_id = effect.data.get(ENVIRONMENT_ID_EFFECT_KEY).ok_or_else(|| {
                DomainError::InvariantViolation(
                    "environment activate effect is missing environment_id".to_owned(),
                )
            })?;
            Ok(Some(EnvironmentEvent::ActiveEnvironmentSet {
                environment_id: EnvironmentId::try_new(environment_id.clone()).map_err(
                    |error| {
                        DomainError::InvariantViolation(format!(
                            "environment activate effect has invalid environment_id: {error}"
                        ))
                    },
                )?,
            }))
        }
        ENVIRONMENT_DEACTIVATE_EFFECT_KIND => {
            if !effect.data.is_empty() {
                return Err(DomainError::InvariantViolation(
                    "environment deactivate effect must not contain data".to_owned(),
                ));
            }
            Ok(Some(EnvironmentEvent::ActiveEnvironmentCleared))
        }
        _ => Ok(None),
    }
}
