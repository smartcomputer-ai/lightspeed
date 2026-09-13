//! Universe resolution shared by role-specific activity adapters.
use std::sync::Arc;

use temporalio_common::error::ApplicationFailure;
use temporalio_sdk::activities::ActivityError;
use uuid::Uuid;

use crate::{
    gateway::GatewayAgentApi,
    universe::{UniverseError, UniverseRuntime},
};

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
                .state_for(universe_id, false)
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
        UniverseError::Unknown { .. } => {
            ApplicationFailure::non_retryable(anyhow::anyhow!("{error}"))
        }
        UniverseError::Runtime(_) => ApplicationFailure::new(anyhow::anyhow!("{error}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        }
    }
}
