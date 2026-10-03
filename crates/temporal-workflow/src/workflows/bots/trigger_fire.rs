//! One trigger fire, started by the trigger's Temporal Schedule (or by
//! hand): read the nominal fire time and run the one activity that re-reads
//! the trigger row and admits what it produces. Schedule fires admit one
//! event; poll fires fetch the source and admit each new item.

use temporalio_macros::{workflow, workflow_methods};
use temporalio_sdk::{WorkflowContext, WorkflowContextView, WorkflowResult};

use super::{
    BotActivities, BotPollFireResult, BotScheduleFireResult, BotTriggerFireArgs,
    BotTriggerFireKind, BotTriggerFireRequest, bot_activity_options, bot_poll_activity_options,
};

/// Search attribute Temporal stamps on workflows started by a Schedule.
pub const TEMPORAL_SCHEDULED_START_TIME: &str = "TemporalScheduledStartTime";

#[workflow]
#[derive(Default)]
pub struct BotTriggerFireWorkflow {
    outcome: Option<BotTriggerFireOutcome>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum BotTriggerFireOutcome {
    Schedule(BotScheduleFireResult),
    Poll(BotPollFireResult),
}

#[workflow_methods]
impl BotTriggerFireWorkflow {
    #[run(name = "BotTriggerFireWorkflow")]
    pub async fn run(
        ctx: &mut WorkflowContext<Self>,
        args: BotTriggerFireArgs,
    ) -> WorkflowResult<BotTriggerFireOutcome> {
        let scheduled_at_ms = scheduled_start_time_ms(ctx).unwrap_or_else(|| {
            ctx.workflow_time()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|elapsed| elapsed.as_millis() as i64)
                .unwrap_or(0)
        });
        let request = BotTriggerFireRequest {
            universe_id: args.universe_id,
            bot_id: args.bot_id,
            trigger_id: args.trigger_id,
            scheduled_at_ms,
        };
        let outcome = match args.kind {
            BotTriggerFireKind::Schedule => {
                let result = ctx
                    .execute_activity(
                        BotActivities::admit_schedule_event,
                        request,
                        bot_activity_options(),
                    )
                    .await
                    .map_err(|error| {
                        temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!(
                            "schedule fire failed: {error}"
                        ))
                    })?;
                BotTriggerFireOutcome::Schedule(result)
            }
            BotTriggerFireKind::Poll => {
                let result = ctx
                    .execute_activity(
                        BotActivities::poll_trigger,
                        request,
                        bot_poll_activity_options(),
                    )
                    .await
                    .map_err(|error| {
                        temporalio_sdk::ApplicationFailure::new(anyhow::anyhow!(
                            "poll fire failed: {error}"
                        ))
                    })?;
                BotTriggerFireOutcome::Poll(result)
            }
        };
        ctx.state_mut(|state| state.outcome = Some(outcome.clone()));
        Ok(outcome)
    }

    #[query(name = "fire_outcome")]
    pub fn fire_outcome(&self, _ctx: &WorkflowContextView) -> Option<BotTriggerFireOutcome> {
        self.outcome.clone()
    }
}

/// The Schedule's nominal fire time from the `TemporalScheduledStartTime`
/// search attribute, when present.
fn scheduled_start_time_ms(ctx: &WorkflowContext<BotTriggerFireWorkflow>) -> Option<i64> {
    let attributes = ctx.search_attributes();
    let payload = attributes.raw_payload(TEMPORAL_SCHEDULED_START_TIME)?;
    parse_search_attribute_time_ms(&payload.data)
}

/// Temporal encodes datetime search attributes as a JSON string payload
/// (RFC 3339).
pub(crate) fn parse_search_attribute_time_ms(data: &[u8]) -> Option<i64> {
    let text = serde_json::from_slice::<String>(data).ok()?;
    let parsed = chrono::DateTime::parse_from_rfc3339(&text).ok()?;
    Some(parsed.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::parse_search_attribute_time_ms;

    #[test]
    fn scheduled_start_time_parses_rfc3339_json_payload() {
        let ms = parse_search_attribute_time_ms(br#""2026-08-30T12:00:00Z""#).expect("parses");
        assert_eq!(ms, 1_788_091_200_000);
        assert_eq!(parse_search_attribute_time_ms(b"not json"), None);
        assert_eq!(parse_search_attribute_time_ms(br#""yesterday""#), None);
    }
}
