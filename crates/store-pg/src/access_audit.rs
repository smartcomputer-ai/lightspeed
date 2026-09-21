use access::{AccessError, AuditEvent};
use serde_json::json;

impl crate::PgAccessStore {
    pub async fn record_audit_event(&self, record: &AuditEvent) -> Result<(), AccessError> {
        let error = |e: sqlx::Error| AccessError::Store(e.to_string());
        sqlx::query("INSERT INTO access_audit_events(attempt_id,method,identity,scope,target,stage,outcome,error_kind,occurred_at_ms,actor,policy_revision) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
            .bind(record.attempt_id).bind(&record.method).bind(json!(record.identity))
            .bind(record.scope.map(|scope| json!(scope))).bind(&record.target)
            .bind(json!(record.stage).as_str().expect("audit stage is a string"))
            .bind(json!(record.outcome).as_str().expect("audit outcome is a string"))
            .bind(&record.error_kind)
            .bind(i64::try_from(record.occurred_at_ms).map_err(|e| AccessError::Store(e.to_string()))?)
            .bind(record.actor.as_ref().map(|actor| json!(actor)))
            .bind(record.policy_revision.map(i64::try_from).transpose().map_err(|e| AccessError::Store(e.to_string()))?)
            .execute(&self.pool).await.map_err(error)?;
        Ok(())
    }
}
