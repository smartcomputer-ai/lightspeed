//! Universe resolution shared by role-specific activity adapters.
use std::sync::Arc;

use temporalio_common::error::ApplicationFailure;
use temporalio_sdk::activities::{ActivityContext, ActivityError};
use uuid::Uuid;

use crate::{
    gateway::GatewayAgentApi,
    universe::{UniverseError, UniverseRuntime},
};

/// Temporal's workflow identity is authoritative, not the payload's universe.
/// Validate before resolving any universe-bound service or store.
pub(super) fn require_activity_universe(
    ctx: &ActivityContext,
    requested: Uuid,
) -> Result<(), ActivityError> {
    require_workflow_universe(ctx.info().workflow_id.as_deref(), requested)
}

fn workflow_universe(workflow_id: Option<&str>) -> Result<Uuid, ActivityError> {
    workflow_id
        .and_then(|id| id.split_once('/'))
        // System workflow suffixes can be longer than session IDs. Only the
        // universe boundary is checked here; workflows validate their full ID.
        .filter(|(_, suffix)| !suffix.is_empty() && !suffix.contains('/'))
        .and_then(|(universe, _)| Uuid::parse_str(universe).ok())
        .ok_or_else(|| {
            ActivityError::application(
                ApplicationFailure::builder(anyhow::anyhow!(
                    "activity requires a universe-qualified workflow identity"
                ))
                .type_name("ActivityWorkflowIdentityInvalid".to_owned())
                .non_retryable(true)
                .build(),
            )
        })
}

fn require_workflow_universe(
    workflow_id: Option<&str>,
    requested: Uuid,
) -> Result<(), ActivityError> {
    let origin = workflow_universe(workflow_id)?;
    if origin != requested {
        return Err(ActivityError::application(
            ApplicationFailure::builder(anyhow::anyhow!(
                "activity universe {requested} does not match workflow universe {origin}"
            ))
            .type_name("ActivityUniverseMismatch".to_owned())
            .non_retryable(true)
            .build(),
        ));
    }
    Ok(())
}

pub(super) enum WorkerUniverses {
    /// One pre-built service for one universe.
    Fixed {
        universe_id: Uuid,
        api: Arc<GatewayAgentApi>,
    },
    /// Lazy per-universe resolution over the deployment runtime.
    Runtime(Arc<UniverseRuntime>),
}

impl WorkerUniverses {
    pub(super) async fn api_for(
        &self,
        universe_id: Uuid,
    ) -> Result<Arc<GatewayAgentApi>, ActivityError> {
        match self {
            Self::Fixed {
                universe_id: served,
                api,
            } => {
                require_matching_universe(*served, universe_id)?;
                Ok(api.clone())
            }
            Self::Runtime(runtime) => runtime
                .state_for(universe_id)
                .await
                .map(|state| state.api.clone())
                .map_err(map_universe_error),
        }
    }
}

fn require_matching_universe(served: Uuid, requested: Uuid) -> Result<(), ActivityError> {
    if served != requested {
        return Err(ActivityError::application(
            ApplicationFailure::non_retryable(anyhow::anyhow!(
                "worker serves universe {served} but activity requested {requested}"
            )),
        ));
    }
    Ok(())
}

fn map_universe_error(error: UniverseError) -> ActivityError {
    ActivityError::application(match error {
        UniverseError::Unknown { .. } => ApplicationFailure::builder(anyhow::anyhow!("{error}"))
            .type_name(temporal_workflow::UNIVERSE_UNKNOWN_ERROR_TYPE.to_owned())
            .non_retryable(true)
            .build(),
        UniverseError::Runtime(_) => ApplicationFailure::new(anyhow::anyhow!("{error}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_requests_cannot_select_another_workflow_universe() {
        let origin = Uuid::from_u128(1);
        let other = Uuid::from_u128(2);
        // Session, workflow-tool, environment-job and conversation workflows
        // all use the same universe-qualified addressing boundary.
        for suffix in [
            "session-1",
            "tool-execution-1",
            "envjob-dev-build",
            "chat-telegram-abc",
        ] {
            let workflow_id = format!("{origin}/{suffix}");
            require_workflow_universe(Some(&workflow_id), origin).expect("same universe");
            let ActivityError::Application(error) =
                require_workflow_universe(Some(&workflow_id), other).expect_err("other universe")
            else {
                panic!("expected application failure");
            };
            assert!(error.is_non_retryable());
            assert_eq!(error.type_name(), Some("ActivityUniverseMismatch"));
        }
    }

    #[test]
    fn environment_job_identity_is_not_limited_to_session_id_length() {
        let universe = Uuid::from_u128(1);
        let workflow_id = temporal_workflow::compose_environment_job_workflow_id(
            universe,
            &"e".repeat(128),
            "job-012345678901234567890123",
        );
        require_workflow_universe(Some(&workflow_id), universe).expect("long job identity");
        assert!(require_workflow_universe(Some(&workflow_id), Uuid::from_u128(2)).is_err());
    }

    #[test]
    fn activity_requests_require_a_valid_workflow_identity() {
        let universe = Uuid::from_u128(1);
        for workflow_id in [
            None,
            Some(String::new()),
            Some("session-without-universe".into()),
            Some("not-a-uuid/session".into()),
            Some(format!("{universe}/")),
            Some(format!("{universe}/session/other")),
        ] {
            let ActivityError::Application(error) =
                require_workflow_universe(workflow_id.as_deref(), universe)
                    .expect_err("missing or invalid workflow identity")
            else {
                panic!("expected application failure");
            };
            assert!(error.is_non_retryable());
            assert_eq!(error.type_name(), Some("ActivityWorkflowIdentityInvalid"));
        }
    }

    #[test]
    fn fixed_workers_accept_only_their_served_universe() {
        let served = Uuid::from_u128(1);
        require_matching_universe(served, served).expect("matching universe");
        let ActivityError::Application(error) =
            require_matching_universe(served, Uuid::from_u128(2)).expect_err("different universe")
        else {
            panic!("expected application failure");
        };
        assert!(error.is_non_retryable());
    }

    #[test]
    fn unknown_universes_are_terminal_but_runtime_failures_are_retryable() {
        for (error, non_retryable) in [
            (
                UniverseError::Unknown {
                    universe_id: Uuid::from_u128(2),
                },
                true,
            ),
            (
                UniverseError::Runtime(anyhow::anyhow!("store unavailable")),
                false,
            ),
        ] {
            let ActivityError::Application(error) = map_universe_error(error) else {
                panic!("expected application failure");
            };
            assert_eq!(error.is_non_retryable(), non_retryable);
            assert_eq!(
                error.type_name(),
                non_retryable.then_some(temporal_workflow::UNIVERSE_UNKNOWN_ERROR_TYPE)
            );
        }
    }
}
