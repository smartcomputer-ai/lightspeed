//! Temporal Schedules for `schedule` and `poll` triggers. One Schedule per
//! trigger, id derived from universe, bot, and trigger; the action starts
//! `BotTriggerFireWorkflow` on the bots queue with the trigger's identity as
//! input, overlap policy `Skip`, a five-minute catch-up window, paused
//! while the trigger or the bot is disabled. The trigger row stays
//! authoritative: the fire re-reads it, so stale Schedule state can never
//! admit stale configuration.
//!
//! The SDK's typed schedule client cannot carry workflow input, so the
//! Schedule is built and updated through the raw workflow service; pause,
//! delete, and describe go through the typed handle.

use std::time::Duration;

use api::{BotId, BotTriggerId, BotTriggerSpec};
use bots::{
    BotRecord, BotStore, BotTriggerRecord, BotTriggerStore,
    ids::{bot_schedule_id, bot_trigger_fire_workflow_id},
};
use temporal_workflow::bots::{BotTriggerFireArgs, BotTriggerFireKind};
use temporalio_client::{NamespacedClient as _, grpc::WorkflowService, schedules::ScheduleError};
use temporalio_common::protos::{
    coresdk::AsJsonPayloadExt as _,
    temporal::api::{
        common::v1::{Payloads, WorkflowType},
        enums::v1::ScheduleOverlapPolicy,
        schedule::v1::{
            CalendarSpec, IntervalSpec, Schedule, ScheduleAction, SchedulePolicies, ScheduleSpec,
            ScheduleState, schedule_action,
        },
        taskqueue::v1::TaskQueue,
        workflow::v1::NewWorkflowExecutionInfo,
        workflowservice::v1::{
            CreateScheduleRequest, DescribeScheduleRequest, UpdateScheduleRequest,
        },
    },
};
use tonic::IntoRequest as _;

use crate::gateway::GatewayAgentApi;

pub const SCHEDULE_CATCHUP_WINDOW: Duration = Duration::from_secs(5 * 60);
pub const BOT_TRIGGER_FIRE_WORKFLOW_TYPE: &str = "BotTriggerFireWorkflow";

/// What a trigger's Schedule should look like right now.
#[derive(Clone, Debug, PartialEq)]
pub struct DesiredSchedule {
    pub spec: ScheduleSpec,
    pub paused: bool,
    pub kind: BotTriggerFireKind,
}

/// The desired Schedule of a trigger, or `None` when the trigger kind has
/// no Schedule.
pub fn desired_schedule(bot: &BotRecord, trigger: &BotTriggerRecord) -> Option<DesiredSchedule> {
    let (spec, kind) = match &trigger.document.spec {
        BotTriggerSpec::Schedule {
            cron: Some(cron),
            timezone,
            ..
        } => (
            ScheduleSpec {
                cron_string: vec![cron.clone()],
                timezone_name: timezone.clone(),
                ..Default::default()
            },
            BotTriggerFireKind::Schedule,
        ),
        BotTriggerSpec::Schedule {
            at_ms: Some(at_ms), ..
        } => (one_shot_spec(*at_ms), BotTriggerFireKind::Schedule),
        BotTriggerSpec::Schedule { .. } => return None,
        BotTriggerSpec::Poll { interval_ms, .. } => (
            ScheduleSpec {
                interval: vec![IntervalSpec {
                    interval: Some(
                        Duration::from_millis(*interval_ms)
                            .try_into()
                            .expect("validated interval fits the proto range"),
                    ),
                    phase: None,
                }],
                ..Default::default()
            },
            BotTriggerFireKind::Poll,
        ),
        _ => return None,
    };
    Some(DesiredSchedule {
        spec,
        paused: !trigger.enabled() || !bot.document.enabled || bot.is_closed(),
        kind,
    })
}

/// A single fully pinned UTC calendar entry for a one-shot instant.
fn one_shot_spec(at_ms: i64) -> ScheduleSpec {
    use chrono::{Datelike as _, Timelike as _};
    let at = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(at_ms)
        .unwrap_or_else(chrono::Utc::now);
    ScheduleSpec {
        calendar: vec![CalendarSpec {
            second: at.second().to_string(),
            minute: at.minute().to_string(),
            hour: at.hour().to_string(),
            day_of_month: at.day().to_string(),
            month: at.month().to_string(),
            day_of_week: "*".to_owned(),
            year: at.year().to_string(),
            comment: "one-shot".to_owned(),
        }],
        timezone_name: "UTC".to_owned(),
        ..Default::default()
    }
}

fn policies() -> SchedulePolicies {
    SchedulePolicies {
        overlap_policy: ScheduleOverlapPolicy::Skip as i32,
        catchup_window: Some(
            SCHEDULE_CATCHUP_WINDOW
                .try_into()
                .expect("static duration fits the proto range"),
        ),
        ..Default::default()
    }
}

impl GatewayAgentApi {
    /// Converge one trigger's Schedule: create or update it for schedule /
    /// poll triggers, delete it for anything else.
    pub(crate) async fn reconcile_bot_trigger_schedule(
        &self,
        bot: &BotRecord,
        trigger: &BotTriggerRecord,
    ) -> anyhow::Result<()> {
        match desired_schedule(bot, trigger) {
            Some(desired) => {
                self.upsert_bot_trigger_schedule(bot, trigger, desired)
                    .await
            }
            None => {
                self.delete_bot_trigger_schedule(&bot.bot_id, &trigger.trigger_id)
                    .await
            }
        }
    }

    async fn upsert_bot_trigger_schedule(
        &self,
        bot: &BotRecord,
        trigger: &BotTriggerRecord,
        desired: DesiredSchedule,
    ) -> anyhow::Result<()> {
        let universe_id = self.universe_id();
        let schedule_id = bot_schedule_id(universe_id, &bot.bot_id, &trigger.trigger_id);
        let client = self.temporal_client().clone();
        let namespace = client.namespace();
        // `Client` implements the raw workflow service; the inherent typed
        // schedule methods shadow the names, so call through the trait.
        let mut service = client.clone();

        let described = WorkflowService::describe_schedule(
            &mut service,
            DescribeScheduleRequest {
                namespace: namespace.clone(),
                schedule_id: schedule_id.clone(),
            }
            .into_request(),
        )
        .await;
        match described {
            Ok(response) => {
                let mut schedule = response.into_inner().schedule.unwrap_or_default();
                schedule.spec = Some(desired.spec);
                schedule.policies = Some(policies());
                let state = schedule.state.get_or_insert_with(Default::default);
                state.paused = desired.paused;
                WorkflowService::update_schedule(
                    &mut service,
                    UpdateScheduleRequest {
                        namespace,
                        schedule_id: schedule_id.clone(),
                        schedule: Some(schedule),
                        identity: client.identity(),
                        request_id: uuid::Uuid::new_v4().to_string(),
                        ..Default::default()
                    }
                    .into_request(),
                )
                .await
                .map_err(|status| anyhow::anyhow!("update schedule {schedule_id}: {status}"))?;
                Ok(())
            }
            Err(status) if status.code() == tonic::Code::NotFound => {
                let args = BotTriggerFireArgs {
                    universe_id,
                    bot_id: bot.bot_id.clone(),
                    trigger_id: trigger.trigger_id.clone(),
                    kind: desired.kind,
                };
                let schedule = Schedule {
                    spec: Some(desired.spec.clone()),
                    action: Some(ScheduleAction {
                        action: Some(schedule_action::Action::StartWorkflow(
                            NewWorkflowExecutionInfo {
                                workflow_id: bot_trigger_fire_workflow_id(
                                    universe_id,
                                    &bot.bot_id,
                                    &trigger.trigger_id,
                                ),
                                workflow_type: Some(WorkflowType {
                                    name: BOT_TRIGGER_FIRE_WORKFLOW_TYPE.to_owned(),
                                }),
                                task_queue: Some(TaskQueue {
                                    name: self.bot_task_queue().to_owned(),
                                    ..Default::default()
                                }),
                                input: Some(Payloads {
                                    payloads: vec![args.as_json_payload()?],
                                }),
                                ..Default::default()
                            },
                        )),
                    }),
                    policies: Some(policies()),
                    state: Some(ScheduleState {
                        paused: desired.paused,
                        notes: format!("bot {} trigger {}", bot.bot_id, trigger.trigger_id),
                        ..Default::default()
                    }),
                };
                match WorkflowService::create_schedule(
                    &mut service,
                    CreateScheduleRequest {
                        namespace,
                        schedule_id: schedule_id.clone(),
                        schedule: Some(schedule),
                        identity: client.identity(),
                        request_id: uuid::Uuid::new_v4().to_string(),
                        ..Default::default()
                    }
                    .into_request(),
                )
                .await
                {
                    Ok(_) => Ok(()),
                    Err(status) if status.code() == tonic::Code::AlreadyExists => {
                        // Lost a create race; converge through update.
                        Box::pin(self.upsert_bot_trigger_schedule(bot, trigger, desired)).await
                    }
                    Err(status) => Err(anyhow::anyhow!("create schedule {schedule_id}: {status}")),
                }
            }
            Err(status) => Err(anyhow::anyhow!("describe schedule {schedule_id}: {status}")),
        }
    }

    /// Delete a trigger's Schedule; an absent Schedule is a no-op.
    pub(crate) async fn delete_bot_trigger_schedule(
        &self,
        bot_id: &BotId,
        trigger_id: &BotTriggerId,
    ) -> anyhow::Result<()> {
        let schedule_id = bot_schedule_id(self.universe_id(), bot_id, trigger_id);
        let handle = self
            .temporal_client()
            .get_schedule_handle(schedule_id.clone());
        match handle.delete().await {
            Ok(()) => Ok(()),
            Err(ScheduleError::Rpc(status)) if status.code() == tonic::Code::NotFound => Ok(()),
            Err(error) => Err(anyhow::anyhow!("delete schedule {schedule_id}: {error}")),
        }
    }

    /// Converge every schedule / poll trigger of the universe (boot, and a
    /// slow background sweep). Returns how many triggers were reconciled.
    pub async fn reconcile_bot_schedules_once(&self) -> Result<usize, api::AgentApiError> {
        let store = self.store();
        let mut reconciled = 0;
        for kind in [api::BotTriggerKind::Schedule, api::BotTriggerKind::Poll] {
            let triggers = store
                .list_bot_triggers_by_kind(kind)
                .await
                .map_err(super::map_bot_error)?;
            for trigger in triggers {
                let bot = match store.read_bot(&trigger.bot_id).await {
                    Ok(bot) => bot,
                    Err(bots::BotError::BotNotFound { .. }) => continue,
                    Err(error) => return Err(super::map_bot_error(error)),
                };
                self.reconcile_bot_trigger_schedule(&bot, &trigger)
                    .await
                    .map_err(|error| api::AgentApiError::internal(error.to_string()))?;
                reconciled += 1;
            }
        }
        Ok(reconciled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::{BotDocument, BotTriggerDocument, ProfileId};

    fn bot(enabled: bool) -> BotRecord {
        BotRecord {
            bot_id: BotId::new("triage"),
            revision: 1,
            document: BotDocument {
                display_name: None,
                description: None,
                profile_id: ProfileId::new("p"),
                brief: None,
                runs_per_day: None,
                breaker: None,
                routed_session_close_after_ms: None,
                self_config: false,
                emit: false,
                enabled,
            },
            event_seq: 0,
            closed_at_ms: None,
            closed_sessions: Vec::new(),
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    fn trigger(spec: BotTriggerSpec, enabled: bool) -> BotTriggerRecord {
        BotTriggerRecord {
            bot_id: BotId::new("triage"),
            trigger_id: BotTriggerId::new("t"),
            revision: 1,
            document: BotTriggerDocument {
                spec,
                filter: None,
                route: None,
                coalesce: None,
                deliver: None,
                session_close_after_ms: None,
                enabled,
            },
            secrets: Default::default(),
            disabled_reason: None,
            disabled_at_ms: None,
            last_filter_error: None,
            last_filter_error_at_ms: None,
            cursor: None,
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    #[test]
    fn cron_and_poll_specs_and_pause_state() {
        let cron = desired_schedule(
            &bot(true),
            &trigger(
                BotTriggerSpec::Schedule {
                    cron: Some("@hourly".to_owned()),
                    at_ms: None,
                    timezone: "Europe/Berlin".to_owned(),
                    summary: "s".to_owned(),
                },
                true,
            ),
        )
        .expect("schedule");
        assert_eq!(cron.spec.cron_string, vec!["@hourly".to_owned()]);
        assert_eq!(cron.spec.timezone_name, "Europe/Berlin");
        assert!(!cron.paused);
        assert_eq!(cron.kind, BotTriggerFireKind::Schedule);

        let poll = desired_schedule(
            &bot(false),
            &trigger(
                BotTriggerSpec::Poll {
                    source: api::PollSource::Exec {
                        environment_id: None,
                        argv: vec!["ls".to_owned()],
                        cwd: None,
                        timeout_ms: None,
                    },
                    interval_ms: 120_000,
                    items: None,
                    cursor: api::PollCursorSpec::IdSet {
                        id: "id".to_owned(),
                    },
                },
                true,
            ),
        )
        .expect("poll");
        assert_eq!(poll.spec.interval.len(), 1);
        assert!(poll.paused, "a disabled bot pauses its schedules");
        assert_eq!(poll.kind, BotTriggerFireKind::Poll);

        assert!(
            desired_schedule(
                &bot(true),
                &trigger(BotTriggerSpec::Bot { from: None }, true)
            )
            .is_none()
        );
    }

    #[test]
    fn one_shot_pins_every_calendar_field() {
        let spec = one_shot_spec(1_788_091_200_000); // 2026-08-30T12:00:00Z
        let calendar = &spec.calendar[0];
        assert_eq!(calendar.year, "2026");
        assert_eq!(calendar.month, "8");
        assert_eq!(calendar.day_of_month, "30");
        assert_eq!(calendar.hour, "12");
        assert_eq!(calendar.minute, "0");
        assert_eq!(calendar.second, "0");
        assert_eq!(spec.timezone_name, "UTC");
    }
}
