//! Idle power policy: the power reaper.
//!
//! One pass reads the daemon idle report of every `ready` provisioned
//! environment that carries an idle policy and, when a stage threshold has
//! been crossed and nothing is executing, records the matching power intent
//! (or a close). Activity is never persisted: the daemon owns the clock and
//! reports a monotonic idle duration; Lightspeed only decides.

use super::environment_providers::map_environments_error;
use super::*;

use ::environments::{
    BeginCloseEnvironment, EnvironmentRecord, EnvironmentStore, IdleAction, PowerState,
    SetEnvironmentPower,
};
use environment_client::EnvironmentDataClient;
use environment_protocol::{
    data::{
        handshake::{InitializeParams, InitializedParams},
        idle::{IdleParams, IdleResponse},
    },
    shared::CURRENT_PROTOCOL_VERSION,
};

/// Outcome of one reaper pass for reporting/tests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PowerReaperStats {
    pub candidates: usize,
    pub unreachable: usize,
    pub busy: usize,
    pub powered_down: usize,
    pub closed: usize,
}

/// Decide the idle action for one environment from the daemon's report.
/// Pure so it can be tested without a daemon: nothing is due while work
/// runs, while a power change is already pending, or before the first
/// supported stage threshold.
pub(crate) fn decide_idle_action(
    environment: &EnvironmentRecord,
    report: &IdleResponse,
) -> Option<IdleAction> {
    if !report.is_quiescent() {
        return None;
    }
    if environment.desired_power != PowerState::Running {
        return None;
    }
    let policy = environment.idle_policy.as_ref()?;
    policy.due_action(report.idle_for_ms, &environment.incarnation.power_states)
}

impl EnvironmentService {
    /// Public entry point for one reaper pass; used by acceptance tests that
    /// drive the reaper deterministically instead of running the loop.
    pub async fn reap_idle_environments_once(&self) -> Result<PowerReaperStats, AgentApiError> {
        self.reconcile_idle_power_once().await
    }

    pub(crate) async fn reconcile_idle_power_once(
        &self,
    ) -> Result<PowerReaperStats, AgentApiError> {
        let candidates = EnvironmentStore::list_environments_with_idle_policy(self.store.as_ref())
            .await
            .map_err(map_environments_error)?;
        let mut stats = PowerReaperStats {
            candidates: candidates.len(),
            ..PowerReaperStats::default()
        };
        let universe_id = self.store.config().universe_id;
        for environment in candidates {
            let report = match self.read_idle_report(universe_id, &environment).await {
                Some(report) => report,
                None => {
                    stats.unreachable += 1;
                    continue;
                }
            };
            let Some(action) = decide_idle_action(&environment, &report) else {
                if !report.is_quiescent() {
                    stats.busy += 1;
                }
                continue;
            };
            let now = now_ms()?;
            match action.power_state() {
                Some(desired_power) => {
                    EnvironmentStore::set_environment_power(
                        self.store.as_ref(),
                        SetEnvironmentPower {
                            environment_id: environment.environment_id.clone(),
                            desired_power,
                            updated_at_ms: now,
                        },
                    )
                    .await
                    .map_err(map_environments_error)?;
                    stats.powered_down += 1;
                    tracing::info!(
                        target: "temporal_server",
                        environment_id = %environment.environment_id,
                        idle_for_ms = report.idle_for_ms,
                        %desired_power,
                        "idle policy powered environment down"
                    );
                }
                None => {
                    match EnvironmentStore::begin_close_environment(
                        self.store.as_ref(),
                        BeginCloseEnvironment {
                            environment_id: environment.environment_id.clone(),
                            updated_at_ms: now,
                        },
                    )
                    .await
                    {
                        Ok(_) => stats.closed += 1,
                        // Already closing/closed by someone else: converged.
                        Err(::environments::EnvironmentRegistryError::InvalidInput { .. }) => {}
                        Err(error) => return Err(map_environments_error(error)),
                    }
                    tracing::info!(
                        target: "temporal_server",
                        environment_id = %environment.environment_id,
                        idle_for_ms = report.idle_for_ms,
                        "idle policy closed environment"
                    );
                }
            }
        }
        Ok(stats)
    }

    /// Ask the environment's daemon for its idle report through the ordinary
    /// on-demand route. `None` when the daemon is unreachable or predates
    /// `env/idle`; the reaper then leaves the environment alone.
    async fn read_idle_report(
        &self,
        universe_id: uuid::Uuid,
        environment: &EnvironmentRecord,
    ) -> Option<IdleResponse> {
        let connection = self
            .environment_gateway
            .connection_for(universe_id, environment);
        let mut client = EnvironmentDataClient::connect(
            &connection.endpoint,
            self.environment_gateway
                .connect_options("lightspeed-power-reaper"),
        )
        .await
        .ok()?;
        let response = client
            .initialize(&InitializeParams {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                client_name: "lightspeed-power-reaper".to_owned(),
                scope: connection.scope.clone(),
                resume_connection_id: None,
            })
            .await
            .ok()?;
        if response.protocol_version != CURRENT_PROTOCOL_VERSION {
            return None;
        }
        client.initialized(&InitializedParams {}).await.ok()?;
        client.idle(&IdleParams {}).await.ok()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ::environments::{
        EnvironmentIdlePolicy, EnvironmentIncarnationId, EnvironmentIncarnationRecord,
        EnvironmentProviderBindingId, EnvironmentProviderId, EnvironmentProvisionRequestId,
        EnvironmentSource, EnvironmentStatus, EnvironmentTemplateId,
    };

    use super::*;

    fn environment(
        policy: EnvironmentIdlePolicy,
        power_states: Vec<PowerState>,
    ) -> EnvironmentRecord {
        EnvironmentRecord {
            environment_id: ::environments::EnvironmentId::new("environment-1"),
            request_id: EnvironmentProvisionRequestId::new("request-1"),
            source: EnvironmentSource::Provisioned {
                provider_id: EnvironmentProviderId::new("incus"),
                binding_id: EnvironmentProviderBindingId::new("primary"),
            },
            display_name: None,
            status: EnvironmentStatus::Ready,
            desired_power: PowerState::Running,
            idle_policy: Some(policy),
            incarnation: EnvironmentIncarnationRecord {
                incarnation_id: EnvironmentIncarnationId::new("incarnation-1"),
                provision_request_id: Some(EnvironmentProvisionRequestId::new("request-1")),
                provider_target_id: Some("target-1".into()),
                template_id: Some(EnvironmentTemplateId::new("dev")),
                adoption_source_target: None,
                power_states,
                created_at_ms: 1,
                updated_at_ms: 1,
            },
            public_ingress_enabled: false,
            public_endpoint: None,
            metadata: BTreeMap::new(),
            last_seen_at_ms: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    fn report(idle_for_ms: u64, running_processes: u32, running_jobs: u32) -> IdleResponse {
        IdleResponse {
            idle_for_ms,
            running_processes,
            running_jobs,
            leftover_process_groups: 0,
        }
    }

    #[test]
    fn idle_decision_respects_work_pending_power_and_provider_support() {
        let policy = EnvironmentIdlePolicy {
            pause_after_ms: Some(1_000),
            suspend_after_ms: Some(2_000),
            stop_after_ms: Some(3_000),
            close_after_ms: Some(4_000),
        };
        let incus = vec![PowerState::Running, PowerState::Paused, PowerState::Stopped];
        let env = environment(policy.clone(), incus.clone());
        assert_eq!(decide_idle_action(&env, &report(500, 0, 0)), None);
        assert_eq!(
            decide_idle_action(&env, &report(1_500, 0, 0)),
            Some(IdleAction::Pause)
        );
        // Running work blocks every stage regardless of the clock.
        assert_eq!(decide_idle_action(&env, &report(10_000, 1, 0)), None);
        assert_eq!(decide_idle_action(&env, &report(10_000, 0, 2)), None);
        // Suspend is due but unsupported by Incus: pause remains.
        assert_eq!(
            decide_idle_action(&env, &report(2_500, 0, 0)),
            Some(IdleAction::Pause)
        );
        assert_eq!(
            decide_idle_action(&env, &report(3_500, 0, 0)),
            Some(IdleAction::Stop)
        );
        assert_eq!(
            decide_idle_action(&env, &report(4_500, 0, 0)),
            Some(IdleAction::Close)
        );
        // A Firecracker-shaped provider gets suspend.
        let firecracker = environment(policy.clone(), PowerState::ALL.to_vec());
        assert_eq!(
            decide_idle_action(&firecracker, &report(2_500, 0, 0)),
            Some(IdleAction::Suspend)
        );
        // A pending power change is left to the reconciler.
        let mut pending = environment(policy, incus);
        pending.desired_power = PowerState::Paused;
        assert_eq!(decide_idle_action(&pending, &report(10_000, 0, 0)), None);
        // No power control at all: only close can ever apply.
        let bare = environment(
            EnvironmentIdlePolicy {
                pause_after_ms: Some(1_000),
                ..EnvironmentIdlePolicy::default()
            },
            Vec::new(),
        );
        assert_eq!(decide_idle_action(&bare, &report(10_000, 0, 0)), None);
    }
}
