//! Significant security decisions, without request bodies, results or error messages.
use access::{ActionActor, AuditEvent, AuditIdentity, AuditOutcome, AuditStage};
use api::{AgentApiError, AgentApiErrorKind};
use sqlx::PgPool;
use std::{collections::HashSet, sync::Arc};
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Default)]
struct Recorded {
    failure: bool,
    admissions: HashSet<String>,
}
struct Attempt {
    id: Uuid,
    recorded: Mutex<Recorded>,
}

tokio::task_local! {
    static ATTEMPT: Arc<Attempt>;
}

/// One RPC or deployment operation, including its nested checks. Do not scope
/// this to a login/context lifetime: subsequent calls must get new attempts.
pub(super) async fn operation<F: Future>(future: F) -> F::Output {
    if ATTEMPT.try_with(|_| ()).is_ok() {
        future.await
    } else {
        ATTEMPT
            .scope(
                Arc::new(Attempt {
                    id: Uuid::new_v4(),
                    recorded: Mutex::default(),
                }),
                future,
            )
            .await
    }
}

/// Successful reads and routine execution traffic are deliberately absent.
/// Add an entry only when it answers a useful security/administration question.
pub(super) fn significant(method: &str) -> bool {
    use api::*;
    matches!(
        method,
        METHOD_SESSION_RUNS_START
            | METHOD_SESSION_RUNS_CANCEL
            | METHOD_SESSION_RUNS_APPROVALS_DECIDE
            | METHOD_SESSION_CONFIG_PUT
            | METHOD_SESSION_PROFILES_APPLY
            | METHOD_SESSION_CLOSE
            | METHOD_SESSION_DELETE
            | METHOD_SESSION_RETENTION_PUT
            | METHOD_BOTS_CREATE
            | METHOD_BOTS_PUT
            | METHOD_BOTS_CLOSE
            | METHOD_BOTS_DELETE
            | METHOD_BOTS_TRIGGERS_PUT
            | METHOD_BOTS_TRIGGERS_DELETE
            | METHOD_MCP_SERVERS_PUT
            | METHOD_MCP_SERVERS_DELETE
            | METHOD_AUTH_GRANTS_IMPORT
            | METHOD_AUTH_GRANTS_REVOKE
            | METHOD_AUTH_CLIENTS_CREATE
            | METHOD_AUTH_CLIENTS_DELETE
            | METHOD_AUTH_PROVIDERS_CREATE
            | METHOD_AUTH_PROVIDERS_DELETE
            | METHOD_AUTH_FLOWS_START
            | METHOD_AUTH_GITHUB_INSTALLATIONS_GRANT
            | METHOD_ENVIRONMENTS_CREATE
            | METHOD_ENVIRONMENTS_EXTERNAL_CREATE
            | METHOD_ENVIRONMENTS_CLOSE
            | METHOD_ENVIRONMENTS_REGISTRATION_KEYS_CREATE
            | METHOD_ENVIRONMENTS_REGISTRATION_KEYS_REVOKE
            | METHOD_ENVIRONMENTS_INGRESS_PUT
            | METHOD_ENVIRONMENTS_IDLE_POLICY_PUT
            | METHOD_ENVIRONMENTS_POWER_PUT
            | METHOD_ENVIRONMENTS_CREDENTIALS_BIND
            | METHOD_ENVIRONMENTS_CREDENTIALS_UNBIND
            | METHOD_VFS_WORKSPACES_DELETE
            | METHOD_CHANNELS_ACCOUNTS_CREATE
            | METHOD_CHANNELS_ACCOUNTS_PUT
            | METHOD_CHANNELS_ACCOUNTS_DELETE
            | METHOD_CHANNELS_PAIRINGS_DELETE
            | METHOD_DEPLOYMENT_UNIVERSES_CREATE
            | METHOD_DEPLOYMENT_UNIVERSES_DELETE
            | METHOD_DEPLOYMENT_IDENTITY_APPLY
            | METHOD_DEPLOYMENT_API_KEYS_CREATE
            | METHOD_DEPLOYMENT_API_KEYS_REVOKE
            | METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_PUT
            | METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_DELETE
            | METHOD_DEPLOYMENT_PROVIDER_BINDINGS_PUT
            | METHOD_DEPLOYMENT_PROVIDER_BINDINGS_DELETE
            | METHOD_DEPLOYMENT_ENVIRONMENTS_ADOPT
    )
}

pub(super) fn request(method: &str, stage: AuditStage) -> AuditEvent {
    let context = super::principal::request_context().ok();
    AuditEvent {
        attempt_id: ATTEMPT
            .try_with(|attempt| attempt.id)
            .unwrap_or_else(|_| Uuid::new_v4()),
        method: api::method_access(method).map(|_| method.to_owned()),
        identity: context
            .as_ref()
            .map(AuditIdentity::from)
            .unwrap_or_default(),
        actor: context.as_ref().map(|c| ActionActor::Principal {
            id: c.acting_principal.id,
        }),
        policy_revision: None,
        scope: context.as_ref().map(|context| context.target_scope),
        target: None,
        stage,
        outcome: AuditOutcome::Allowed,
        error_kind: None,
        occurred_at_ms: 0,
    }
}

pub(super) async fn record(pool: &PgPool, record: &mut AuditEvent) -> Result<(), AgentApiError> {
    let attempt = ATTEMPT.try_with(Arc::clone).ok();
    let mut recorded = match &attempt {
        Some(attempt) => Some(attempt.recorded.lock().await),
        None => None,
    };
    let failure = matches!(record.outcome, AuditOutcome::Denied | AuditOutcome::Failed);
    let admission = (record.stage == AuditStage::Admission
        && record.outcome == AuditOutcome::Allowed)
        .then(|| {
            serde_json::json!([
                record.method,
                record.scope,
                record.target,
                record.identity,
                record.actor,
                record.policy_revision
            ])
            .to_string()
        });
    if recorded.as_ref().is_some_and(|r| {
        (failure && r.failure)
            || admission
                .as_ref()
                .is_some_and(|key| r.admissions.contains(key))
    }) {
        return Ok(());
    }
    record.occurred_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| AgentApiError::internal(e.to_string()))?
        .as_millis() as u64;
    store_pg::PgAccessStore::new(pool.clone())
        .record_audit_event(record)
        .await
        .map_err(|e| AgentApiError::internal(e.to_string()))?;
    // Never mark a failed write as recorded; fail closed before the side effect.
    if let Some(recorded) = recorded.as_mut() {
        recorded.failure |= failure;
        if let Some(key) = admission {
            recorded.admissions.insert(key);
        }
    }
    Ok(())
}

pub(super) async fn failure(
    pool: &PgPool,
    record: &mut AuditEvent,
    error: &AgentApiError,
) -> Result<(), AgentApiError> {
    failed(record, error);
    self::record(pool, record).await
}

pub(super) fn failed(record: &mut AuditEvent, error: &AgentApiError) {
    record.outcome = if error.kind == AgentApiErrorKind::Rejected {
        AuditOutcome::Denied
    } else {
        AuditOutcome::Failed
    };
    record.error_kind = serde_json::to_value(&error.kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routine_traffic_is_quiet_but_control_and_configuration_are_significant() {
        use api::*;
        for method in [
            METHOD_SESSION_EVENTS_READ,
            METHOD_ACCESS_READ,
            METHOD_SESSION_START,
            METHOD_SESSION_RENAME,
            METHOD_SESSION_CONTEXT_APPEND,
            METHOD_SESSION_RUNS_STEER,
            METHOD_BLOBS_PUT,
            METHOD_VFS_SNAPSHOTS_COMMIT,
            METHOD_VFS_WORKSPACES_UPDATE,
            METHOD_AUTH_GRANTS_LEASE,
            METHOD_MCP_SERVERS_TOOLS_DISCOVER,
            METHOD_BOTS_EVENTS_ADMIT,
            METHOD_CHANNELS_INBOUND_ADMIT,
            METHOD_DEPLOYMENT_IDENTITY_SELF,
            METHOD_DEPLOYMENT_IDENTITY_DIRECTORY,
            METHOD_DEPLOYMENT_API_KEYS_LIST,
            METHOD_DEPLOYMENT_ENVIRONMENT_PROVIDERS_READ,
            METHOD_DEPLOYMENT_PROVIDER_BINDINGS_LIST,
        ] {
            assert!(!significant(method), "{method}");
        }
        for method in [
            METHOD_SESSION_RUNS_START,
            METHOD_SESSION_RUNS_CANCEL,
            METHOD_SESSION_RUNS_APPROVALS_DECIDE,
            METHOD_SESSION_DELETE,
            METHOD_MCP_SERVERS_PUT,
            METHOD_AUTH_GRANTS_REVOKE,
            METHOD_DEPLOYMENT_IDENTITY_APPLY,
            METHOD_DEPLOYMENT_UNIVERSES_DELETE,
        ] {
            assert!(significant(method), "{method}");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn attempts_follow_operations_not_task_or_identity_lifetimes() {
        let id = operation(async {
            let first = request(api::METHOD_SESSION_RUNS_START, AuditStage::Admission).attempt_id;
            assert_eq!(
                operation(async {
                    request(api::METHOD_SESSION_RUNS_START, AuditStage::Completion).attempt_id
                })
                .await,
                first
            );
            first
        })
        .await;
        let next = operation(async {
            request(api::METHOD_SESSION_RUNS_START, AuditStage::Admission).attempt_id
        })
        .await;
        assert_ne!(id, next);
        assert!(ATTEMPT.try_with(|_| ()).is_err());
    }
}
