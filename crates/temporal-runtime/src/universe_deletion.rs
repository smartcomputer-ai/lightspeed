//! Resumable deployment teardown. The row is the cursor; only storage finalizes it.
use std::{sync::Arc, time::Duration};

use api::UniverseDeletionStep;
use bots::BotTriggerStore;
use environments::{
    EnvironmentCloseReason, EnvironmentProviderStore, EnvironmentRegistryError, EnvironmentState,
    EnvironmentStore, ListEnvironments,
};
use futures_util::TryStreamExt;
use object_store::{ObjectStoreExt, path::Path};
use serde_json::json;
use store_pg::{
    UniverseDeletionRecord,
    changes::{Change, ChangeKind},
};

use crate::universe::UniverseRuntime;

const TICK: Duration = Duration::from_secs(3);
const ENVIRONMENT_CLOSE_BUDGET_MS: u64 = 120_000;

impl UniverseRuntime {
    pub async fn run_universe_deletion(self: Arc<Self>) {
        crate::gateway::replica::run_as_leader(
            self.stores().pool(),
            store_pg::RuntimeLoop::UniverseDeletion,
            || self.clone().deletion_loop(),
        )
        .await;
    }

    async fn deletion_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut changes = self.stores().changes().subscribe_all();
        loop {
            tokio::select! {
                _ = tick.tick() => {},
                change = changes.recv() => {
                    if matches!(change, Ok(Some(ref change)) if change.kind != ChangeKind::Universe) { continue; }
                }
            }
            if let Err(error) = self.delete_universes_once().await {
                tracing::warn!(%error, "universe deletion scan failed");
            }
        }
    }

    /// One durable step per universe. Public for deployment maintenance and live tests.
    pub async fn delete_universes_once(&self) -> anyhow::Result<()> {
        for mut record in store_pg::list_deleting_universes(self.stores().pool()).await? {
            if let Err(error) = self.delete_universe_step(&mut record).await {
                record.deletion.detail = Some(error.to_string());
                tracing::warn!(universe_id = %record.universe_id, %error, "universe deletion will retry");
            }
            store_pg::update_universe_deletion(self.stores().pool(), record.universe_id, &record)
                .await?;
        }
        Ok(())
    }

    async fn delete_universe_step(
        &self,
        record: &mut UniverseDeletionRecord,
    ) -> anyhow::Result<()> {
        let id = record.universe_id;
        match record.deletion.step {
            UniverseDeletionStep::Intake => {
                // Teardown alone may build services for a fenced row. It never populates the cache.
                let state = self.build_state(id).await?;
                let mut triggers = Vec::new();
                for kind in [api::BotTriggerKind::Schedule, api::BotTriggerKind::Poll] {
                    triggers.extend(state.store.list_bot_triggers_by_kind(kind).await?);
                }
                // A pre-fence reconciliation must finish before we remove its schedule.
                let mut guard = self.stores().pool().begin().await?;
                sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
                    .bind(format!("universe-schedules/{id}"))
                    .execute(&mut *guard)
                    .await?;
                for trigger in &triggers {
                    if state
                        .api
                        .delete_bot_trigger_schedule_counted(&trigger.bot_id, &trigger.trigger_id)
                        .await?
                    {
                        increment(record, "schedulesDeleted", 1);
                    }
                }
                sqlx::query("DELETE FROM channel_accounts WHERE universe_id = $1")
                    .bind(id)
                    .execute(&mut *guard)
                    .await?;
                guard.commit().await?;
                advance(record, UniverseDeletionStep::Workflows);
            }
            UniverseDeletionStep::Workflows => {
                // Space empty observations by a full tick even if unrelated hints wake the pass.
                let now = clock_ms();
                if record.progress["workflowScanAtMs"]
                    .as_u64()
                    .is_some_and(|last| now.saturating_sub(last) < TICK.as_millis() as u64)
                {
                    return Ok(());
                }
                let (observed, terminated) =
                    crate::universe_workflows::terminate_universe_workflows(self.client(), id)
                        .await?;
                record.progress["workflowScanAtMs"] = json!(now);
                increment(record, "workflowsTerminated", terminated);
                let empty = if observed == 0 {
                    record.progress["emptyScans"].as_u64().unwrap_or(0) + 1
                } else {
                    0
                };
                record.progress["emptyScans"] = json!(empty);
                if empty >= 2 {
                    record.progress["environmentsStartedAtMs"] = json!(now);
                    advance(record, UniverseDeletionStep::Environments);
                } else {
                    record.deletion.detail = Some("Waiting for two empty workflow listings".into());
                }
            }
            UniverseDeletionStep::Environments => {
                let state = self.build_state(id).await?;
                let mut after = None;
                let mut pending = Vec::new();
                let mut closed = 0;
                let mut unconfirmed_closed = Vec::new();
                loop {
                    let page = state
                        .store
                        .list_environments(ListEnvironments {
                            include_closed: true,
                            limit: environments::LIST_ENVIRONMENTS_MAX_LIMIT,
                            after,
                            ..Default::default()
                        })
                        .await?;
                    for mut environment in page.environments {
                        if environment.state != EnvironmentState::Closed
                            && (environment.state != EnvironmentState::Closing
                                || environment.close_reason
                                    != Some(EnvironmentCloseReason::UniverseDeleted))
                        {
                            environment = state
                                .api
                                .environment_service()
                                .begin_close(
                                    &environment.environment_id,
                                    EnvironmentCloseReason::UniverseDeleted,
                                )
                                .await?;
                        }
                        if environment.state == EnvironmentState::Closed {
                            let closed_during_deletion = environment
                                .closed_at_ms
                                .is_some_and(|at| at >= record.deletion.started_at_ms as i64);
                            if closed_during_deletion
                                && let Some(source) = environment.provisioned()
                                && matches!(
                                    state.store.read_provider(&source.provider_id).await,
                                    Err(EnvironmentRegistryError::NotFound { .. })
                                )
                            {
                                unconfirmed_closed.push(source.clone());
                                continue;
                            }
                            if closed_during_deletion {
                                closed += 1;
                            }
                        } else if let Some(source) = environment.provisioned() {
                            pending.push(source.clone());
                        }
                    }
                    after = page.next_after;
                    if after.is_none() {
                        break;
                    }
                }
                self.environment_signals().nudge_work();
                record.summary["removed"]["environmentsClosed"] = json!(closed);
                let started = record.progress["environmentsStartedAtMs"]
                    .as_u64()
                    .unwrap_or(record.deletion.started_at_ms);
                if environment_wait_complete(
                    clock_ms().saturating_sub(started),
                    pending.iter().map(|source| source.attempts),
                ) {
                    record.summary["removed"]["environmentsUnconfirmed"] = json!(pending.iter().chain(unconfirmed_closed.iter()).map(|source| json!({
                        "providerId": source.provider_id, "providerTargetId": source.provider_target_id,
                    })).collect::<Vec<_>>());
                    advance(record, UniverseDeletionStep::Storage);
                } else {
                    record.deletion.detail = Some(format!(
                        "Waiting for {} provisioned environments to close",
                        pending.len()
                    ));
                }
            }
            UniverseDeletionStep::Storage => {
                let mut guard =
                    store_pg::lock_universe_objects_for_deletion(self.stores().pool(), id).await?;
                let keys: Vec<String> = sqlx::query_scalar("SELECT object_key FROM cas_blobs WHERE universe_id = $1 AND storage_kind = 'object' AND object_key IS NOT NULL ORDER BY object_key")
                    .bind(id).fetch_all(&mut *guard).await?;
                if let Some(objects) = self.stores().object_store() {
                    for key in keys {
                        let path = Path::from(key);
                        match objects.head(&path).await {
                            Ok(_) => {}
                            Err(object_store::Error::NotFound { .. }) => continue,
                            Err(error) => return Err(error.into()),
                        }
                        match objects.delete(&path).await {
                            Ok(()) => increment(record, "objectsDeleted", 1),
                            Err(object_store::Error::NotFound { .. }) => {}
                            Err(error) => return Err(error.into()),
                        }
                    }
                    let prefix =
                        store_pg::universe_cas_object_prefix(self.stores().object_prefix(), id);
                    let removed =
                        store_pg::delete_objects_under_prefix(objects.as_ref(), &prefix).await?;
                    increment(record, "objectsDeleted", removed);
                    let prefix_path = Path::from(prefix.trim_end_matches('/'));
                    anyhow::ensure!(
                        objects.list(Some(&prefix_path)).try_next().await?.is_none(),
                        "waiting for the universe object prefix to become empty"
                    );
                } else {
                    anyhow::ensure!(
                        keys.is_empty(),
                        "universe has external objects but no object store is configured"
                    );
                }
                record.deletion.detail = None;
                sqlx::query("UPDATE universes SET deletion_json = $2 WHERE universe_id = $1 AND deleting_at_ms IS NOT NULL")
                    .bind(id).bind(json!(record)).execute(&mut *guard).await?;
                store_pg::finish_universe_deletion_in_transaction(
                    &mut guard,
                    id,
                    clock_ms() as i64,
                )
                .await?;
                guard.commit().await?;
                self.stores().changes().publish(Change::new(
                    id,
                    ChangeKind::Universe,
                    id.to_string(),
                ));
                self.evict(id).await;
            }
        }
        Ok(())
    }
}

fn environment_wait_complete(elapsed_ms: u64, attempts: impl Iterator<Item = u32>) -> bool {
    elapsed_ms >= ENVIRONMENT_CLOSE_BUDGET_MS || attempts.into_iter().all(|attempts| attempts >= 2)
}

fn advance(record: &mut UniverseDeletionRecord, step: UniverseDeletionStep) {
    record.deletion.step = step;
    record.deletion.detail = None;
}

fn increment(record: &mut UniverseDeletionRecord, key: &str, count: u64) {
    record.summary["removed"][key] = json!(
        record.summary["removed"][key]
            .as_u64()
            .unwrap_or(0)
            .saturating_add(count)
    );
}

fn clock_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environments_wait_for_two_attempts_each_but_never_past_budget() {
        assert!(environment_wait_complete(0, [].into_iter()));
        assert!(!environment_wait_complete(0, [2, 1].into_iter()));
        assert!(environment_wait_complete(0, [2, 2].into_iter()));
        assert!(!environment_wait_complete(119_999, [0].into_iter()));
        assert!(environment_wait_complete(120_000, [0].into_iter()));
    }
}
