use access::{AccessError, AccessScope, AuditEvent};

impl crate::PgAccessStore {
    pub async fn record_audit_event(&self, record: &AuditEvent) -> Result<(), AccessError> {
        let error = |e: sqlx::Error| AccessError::Store(e.to_string());
        let range = |e: std::num::TryFromIntError| AccessError::Store(e.to_string());
        sqlx::query("INSERT INTO access_audit_events(occurred_at_ms,method,outcome,error_kind,authenticated_principal_id,acting_principal_id,credential,universe_id,target,policy_revision,privileged) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
            .bind(i64::try_from(record.occurred_at_ms).map_err(range)?)
            .bind(&record.method)
            .bind(record.outcome.as_str())
            .bind(&record.error_kind)
            .bind(record.authenticated_principal)
            .bind(record.acting_principal)
            .bind(&record.credential)
            .bind(match record.scope {
                Some(AccessScope::Universe { universe_id }) => Some(universe_id),
                _ => None,
            })
            .bind(&record.target)
            .bind(record.policy_revision.map(i64::try_from).transpose().map_err(range)?)
            .bind(record.privileged)
            .execute(&self.pool).await.map_err(error)?;
        Ok(())
    }
}
