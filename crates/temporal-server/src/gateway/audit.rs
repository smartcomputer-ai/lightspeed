//! Durable access audit, written once per request at the API boundary.
//!
//! A row is recorded for every call of a method declared `audit: true` and for
//! every refusal of an authenticated caller. Callers without a valid credential
//! are only logged: they must not be able to make the deployment write. The
//! record is best-effort by design. It never changes a response, so an audit
//! outage cannot block stopping work or discard a committed result; transactional
//! identity and key changes keep their own in-transaction change log.
use access::{AuditEvent, AuditOutcome, RequestContext, auditable_identifier};
use api::{AgentApiErrorKind, JsonRpcResponse};
use serde_json::{Map, Value};
use sqlx::PgPool;

/// Parameter names that identify the resource an operation addresses. Only
/// these are copied, never metadata, content or configuration values.
const TARGET_FIELDS: &[&str] = &[
    "sessionId",
    "runId",
    "approvalId",
    "botId",
    "triggerId",
    "profileId",
    "serverId",
    "grantId",
    "clientId",
    "providerId",
    "flowId",
    "environmentId",
    "bindingId",
    "workspaceId",
    "accountId",
    "universeId",
    "principalId",
];

/// Resource documents whose own identifier names the target, such as
/// `server.serverId`. Free-form objects like metadata are never scanned.
const TARGET_DOCUMENTS: &[&str] = &["server", "bot", "trigger", "account", "assignment"];

/// Resource identifiers of a request, taken before its parameters are consumed.
pub(super) fn target(params: Option<&Value>) -> Option<Value> {
    let mut target = Map::new();
    let params = params?.as_object()?;
    collect(params, &mut target);
    for document in TARGET_DOCUMENTS {
        if let Some(document) = params.get(*document).and_then(Value::as_object) {
            collect(document, &mut target);
        }
    }
    // A scope selects whose keys or identities are managed.
    if let Some(scope) = params.get("scope")
        && serde_json::from_value::<access::AccessScope>(scope.clone()).is_ok()
    {
        target.insert("scope".into(), scope.clone());
    }
    // A typed resource reference names the governed resource.
    if let Some(resource) = params.get("resource")
        && serde_json::from_value::<access::ResourceRef>(resource.clone()).is_ok()
    {
        target.insert("resource".into(), resource.clone());
    }
    // The typed change itself is in the transactional change log.
    if let Some(operation) = params.get("operation").and_then(Value::as_str)
        && operation.len() <= 64
        && operation
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b == b'_')
    {
        target.insert("operation".into(), operation.into());
    }
    // Only a well-formed display prefix: a pasted secret must not be retained.
    if let Some(prefix) = params.get("keyPrefix").and_then(Value::as_str)
        && prefix.len() == auth::API_KEY_DISPLAY_PREFIX_LEN
        && prefix.starts_with(auth::API_KEY_SECRET_PREFIX)
        && prefix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        target.insert("keyPrefix".into(), prefix.into());
    }
    (!target.is_empty()).then_some(Value::Object(target))
}

fn collect(params: &Map<String, Value>, target: &mut Map<String, Value>) {
    for field in TARGET_FIELDS {
        if !target.contains_key(*field)
            && let Some(id) = params
                .get(*field)
                .and_then(Value::as_str)
                .and_then(auditable_identifier)
        {
            target.insert((*field).into(), id.into());
        }
    }
}

/// An authenticated caller refused before dispatch.
pub(super) async fn refused(pool: &PgPool, mut event: AuditEvent, target: Option<Value>) {
    event.target = target;
    event.error_kind = error_kind(&AgentApiErrorKind::Forbidden);
    write(pool, &event).await;
}

/// The outcome of a dispatched request. `privileged` says a decision of the
/// request relied on `read_private_content`: such a read is recorded even
/// though reads are otherwise quiet.
pub(super) async fn completed(
    pool: &PgPool,
    method: &str,
    target: Option<Value>,
    context: &RequestContext,
    response: &JsonRpcResponse,
    privileged: bool,
) {
    let error = response
        .error
        .as_ref()
        .map(|error| error.data.as_ref().map(|error| &error.kind));
    let outcome = match error {
        // The caller was identified at admission, so a credential revoked while
        // the request was parked is an attributable denial too.
        Some(Some(AgentApiErrorKind::Forbidden | AgentApiErrorKind::Unauthenticated)) => {
            AuditOutcome::Denied
        }
        _ if !api::method_audited(method) && !privileged => return,
        Some(_) => AuditOutcome::Failed,
        None => AuditOutcome::Succeeded,
    };
    let mut event = AuditEvent::new(method, outcome, now_ms()).with_context(context);
    event.target = target;
    event.error_kind = error.flatten().and_then(error_kind);
    event.privileged = privileged;
    write(pool, &event).await;
}

fn error_kind(kind: &AgentApiErrorKind) -> Option<String> {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

async fn write(pool: &PgPool, event: &AuditEvent) {
    if let Err(error) = store_pg::PgAccessStore::new(pool.clone())
        .record_audit_event(event)
        .await
    {
        tracing::error!(
            target: "temporal_server",
            method = %event.method,
            outcome = event.outcome.as_str(),
            acting_principal = ?event.acting_principal,
            %error,
            "access audit record was not persisted"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_method_declares_its_audit_policy_and_routine_traffic_is_quiet() {
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
            assert!(!method_audited(method), "{method}");
        }
        for method in [
            METHOD_SESSION_RUNS_START,
            METHOD_SESSION_RUNS_CANCEL,
            METHOD_SESSION_RUNS_APPROVALS_DECIDE,
            METHOD_SESSION_DELETE,
            METHOD_MCP_SERVERS_PUT,
            METHOD_AUTH_GRANTS_REVOKE,
            METHOD_DEPLOYMENT_IDENTITY_APPLY,
            METHOD_DEPLOYMENT_API_KEYS_CREATE,
            METHOD_DEPLOYMENT_UNIVERSES_DELETE,
        ] {
            assert!(method_audited(method), "{method}");
        }
        assert!(!method_audited("session/missing"));
    }

    #[test]
    fn targets_keep_identifiers_and_drop_everything_else() {
        let target = target(Some(&json!({
            "sessionId": "session_1",
            "input": [{"text": "secret prompt"}],
            "metadata": {"sessionId": "spoofed", "note": "free text"},
            "server": {"serverId": "mcp_1", "serverUrl": "https://example.test", "headers": {"x": "y"}},
            "scope": {"kind": "deployment"},
            "operation": "assign_role",
            "botId": "x".repeat(300),
            "resource": {"kind": "session", "id": "session_1"},
        })))
        .unwrap();
        assert_eq!(
            target,
            json!({
                "sessionId": "session_1",
                "serverId": "mcp_1",
                "scope": {"kind": "deployment"},
                "operation": "assign_role",
                "resource": {"kind": "session", "id": "session_1"},
            })
        );
        assert_eq!(
            super::target(Some(&json!({"resource": {"kind": "planet", "id": "x"}}))),
            None
        );
        assert_eq!(super::target(None), None);
        assert_eq!(super::target(Some(&json!({"input": "only content"}))), None);
    }

    #[test]
    fn a_pasted_secret_is_never_retained_as_a_key_prefix() {
        let secret = format!("lsk_{}", "a".repeat(43));
        assert_eq!(target(Some(&json!({"keyPrefix": secret}))), None);
        assert_eq!(
            target(Some(&json!({"keyPrefix": "lsk_ab12cd34"}))),
            Some(json!({"keyPrefix": "lsk_ab12cd34"}))
        );
    }
}
