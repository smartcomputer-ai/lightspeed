use async_trait::async_trait;
use engine::{RunId, SessionId, ToolCallId, TurnId};
use environments::{
    CreateJobHandle, EnvironmentId, EnvironmentProviderId, EnvironmentRegistryError,
    JobHandleRecord, JobHandleStore, ListJobHandles,
};
use host_protocol::shared::{HostTargetId, JobId};
use sqlx::Row;

use crate::PgStore;

const JOB_HANDLE_COLUMNS: &str = r#"
    session_id,
    env_id,
    provider_id,
    target_id,
    namespace,
    job_id,
    name,
    queue_key,
    created_by_run_id,
    created_by_turn_id,
    created_by_tool_call_id,
    created_at_ms,
    start_request_hash
"#;

#[async_trait]
impl JobHandleStore for PgStore {
    async fn create_job_handles(
        &self,
        records: Vec<CreateJobHandle>,
    ) -> Result<Vec<JobHandleRecord>, EnvironmentRegistryError> {
        self.ensure_universe()
            .await
            .map_err(|error| environment_store_error("ensure universe", error))?;
        let records = records
            .into_iter()
            .map(CreateJobHandle::into_record)
            .collect::<Vec<_>>();
        for record in &records {
            record.validate()?;
        }

        let mut tx = self.pool.begin().await.map_err(|error| {
            environment_sql_error("begin create job handles transaction", error)
        })?;
        let mut created = Vec::with_capacity(records.len());
        for record in records {
            let query = format!(
                r#"
                INSERT INTO environment_jobs (
                    universe_id,
                    session_id,
                    env_id,
                    provider_id,
                    target_id,
                    namespace,
                    job_id,
                    name,
                    queue_key,
                    created_by_run_id,
                    created_by_turn_id,
                    created_by_tool_call_id,
                    created_at_ms,
                    start_request_hash
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
                ON CONFLICT (universe_id, session_id, env_id, job_id) DO UPDATE SET
                    start_request_hash = environment_jobs.start_request_hash
                WHERE environment_jobs.start_request_hash = EXCLUDED.start_request_hash
                RETURNING {JOB_HANDLE_COLUMNS}
                "#
            );
            let row = sqlx::query(&query)
                .bind(self.config.universe_id)
                .bind(record.session_id.as_str())
                .bind(record.env_id.as_str())
                .bind(record.provider_id.as_str())
                .bind(record.target_id.as_str())
                .bind(record.namespace.as_str())
                .bind(record.job_id.as_str())
                .bind(record.name.as_deref())
                .bind(record.queue_key.as_deref())
                .bind(optional_u64_to_i64(
                    record.created_by_run_id.map(RunId::as_u64),
                    "created_by_run_id",
                )?)
                .bind(optional_u64_to_i64(
                    record.created_by_turn_id.map(TurnId::as_u64),
                    "created_by_turn_id",
                )?)
                .bind(
                    record
                        .created_by_tool_call_id
                        .as_ref()
                        .map(ToolCallId::as_str),
                )
                .bind(record.created_at_ms)
                .bind(record.start_request_hash.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| environment_sql_error("create job handle", error))?;
            let Some(row) = row else {
                return Err(EnvironmentRegistryError::AlreadyExists {
                    kind: "job_handle",
                    id: format!(
                        "{}/{}/{}",
                        record.session_id,
                        record.env_id.as_str(),
                        record.job_id.as_str()
                    ),
                });
            };
            created.push(job_handle_from_row(&row)?);
        }
        tx.commit()
            .await
            .map_err(|error| environment_sql_error("commit create job handles", error))?;
        Ok(created)
    }

    async fn read_job_handle(
        &self,
        session_id: &SessionId,
        env_id: &EnvironmentId,
        job_id: &JobId,
    ) -> Result<JobHandleRecord, EnvironmentRegistryError> {
        let query = format!(
            r#"
            SELECT {JOB_HANDLE_COLUMNS}
            FROM environment_jobs
            WHERE universe_id = $1 AND session_id = $2 AND env_id = $3 AND job_id = $4
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(session_id.as_str())
            .bind(env_id.as_str())
            .bind(job_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| environment_sql_error("read job handle", error))?;
        let Some(row) = row else {
            return Err(job_handle_not_found(session_id, env_id, job_id));
        };
        job_handle_from_row(&row)
    }

    async fn list_job_handles(
        &self,
        request: ListJobHandles,
    ) -> Result<Vec<JobHandleRecord>, EnvironmentRegistryError> {
        validate_list_job_handles_pg(&request)?;
        let mut query = format!(
            r#"
            SELECT {JOB_HANDLE_COLUMNS}
            FROM environment_jobs
            WHERE universe_id = $1 AND session_id = $2
            "#
        );
        let mut next_param = 3;
        if request.env_id.is_some() {
            query.push_str(&format!(" AND env_id = ${next_param}"));
            next_param += 1;
        }
        query.push_str(" ORDER BY created_at_ms DESC, env_id, job_id");
        if request.limit.is_some() {
            query.push_str(&format!(" LIMIT ${next_param}"));
        }

        let mut sql = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(request.session_id.as_str());
        if let Some(env_id) = request.env_id.as_ref() {
            sql = sql.bind(env_id.as_str());
        }
        if let Some(limit) = request.limit {
            sql = sql.bind(usize_to_i64(limit, "job handle list limit")?);
        }

        let rows = sql
            .fetch_all(&self.pool)
            .await
            .map_err(|error| environment_sql_error("list job handles", error))?;
        rows.iter().map(job_handle_from_row).collect()
    }

    async fn delete_job_handle(
        &self,
        session_id: &SessionId,
        env_id: &EnvironmentId,
        job_id: &JobId,
    ) -> Result<JobHandleRecord, EnvironmentRegistryError> {
        let query = format!(
            r#"
            DELETE FROM environment_jobs
            WHERE universe_id = $1 AND session_id = $2 AND env_id = $3 AND job_id = $4
            RETURNING {JOB_HANDLE_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(session_id.as_str())
            .bind(env_id.as_str())
            .bind(job_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| environment_sql_error("delete job handle", error))?;
        let Some(row) = row else {
            return Err(job_handle_not_found(session_id, env_id, job_id));
        };
        job_handle_from_row(&row)
    }
}

fn job_handle_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<JobHandleRecord, EnvironmentRegistryError> {
    let session_id: String = row
        .try_get("session_id")
        .map_err(|error| environment_sql_error("decode job handle session id", error))?;
    let env_id: String = row
        .try_get("env_id")
        .map_err(|error| environment_sql_error("decode job handle env id", error))?;
    let provider_id: String = row
        .try_get("provider_id")
        .map_err(|error| environment_sql_error("decode job handle provider id", error))?;
    let target_id: String = row
        .try_get("target_id")
        .map_err(|error| environment_sql_error("decode job handle target id", error))?;
    let job_id: String = row
        .try_get("job_id")
        .map_err(|error| environment_sql_error("decode job handle job id", error))?;
    let run_id: Option<i64> = row
        .try_get("created_by_run_id")
        .map_err(|error| environment_sql_error("decode job handle run id", error))?;
    let turn_id: Option<i64> = row
        .try_get("created_by_turn_id")
        .map_err(|error| environment_sql_error("decode job handle turn id", error))?;
    let tool_call_id: Option<String> = row
        .try_get("created_by_tool_call_id")
        .map_err(|error| environment_sql_error("decode job handle tool call id", error))?;
    let record = JobHandleRecord {
        session_id: SessionId::try_new(session_id).map_err(|error| {
            EnvironmentRegistryError::Store {
                message: format!("decode job handle session id: {error}"),
            }
        })?,
        env_id: EnvironmentId::try_new(env_id).map_err(|error| {
            EnvironmentRegistryError::Store {
                message: format!("decode job handle env id: {error}"),
            }
        })?,
        provider_id: EnvironmentProviderId::try_new(provider_id).map_err(|error| {
            EnvironmentRegistryError::Store {
                message: format!("decode job handle provider id: {error}"),
            }
        })?,
        target_id: HostTargetId::new(target_id),
        namespace: row
            .try_get("namespace")
            .map_err(|error| environment_sql_error("decode job handle namespace", error))?,
        job_id: JobId::new(job_id),
        name: row
            .try_get("name")
            .map_err(|error| environment_sql_error("decode job handle name", error))?,
        queue_key: row
            .try_get("queue_key")
            .map_err(|error| environment_sql_error("decode job handle queue key", error))?,
        created_by_run_id: optional_i64_to_run_id(run_id)?,
        created_by_turn_id: optional_i64_to_turn_id(turn_id)?,
        created_by_tool_call_id: tool_call_id.map(ToolCallId::try_new).transpose().map_err(
            |error| EnvironmentRegistryError::Store {
                message: format!("decode job handle tool call id: {error}"),
            },
        )?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|error| environment_sql_error("decode job handle created_at_ms", error))?,
        start_request_hash: row
            .try_get("start_request_hash")
            .map_err(|error| environment_sql_error("decode job handle request hash", error))?,
    };
    record.validate()?;
    Ok(record)
}

fn job_handle_not_found(
    session_id: &SessionId,
    env_id: &EnvironmentId,
    job_id: &JobId,
) -> EnvironmentRegistryError {
    EnvironmentRegistryError::NotFound {
        kind: "job_handle",
        id: format!("{session_id}/{}/{}", env_id.as_str(), job_id.as_str()),
    }
}

fn environment_store_error(action: &str, error: crate::PgStoreError) -> EnvironmentRegistryError {
    EnvironmentRegistryError::Store {
        message: format!("{action}: {error}"),
    }
}

fn environment_sql_error(action: &str, error: sqlx::Error) -> EnvironmentRegistryError {
    EnvironmentRegistryError::Store {
        message: format!("{action}: {error}"),
    }
}

fn validate_list_job_handles_pg(request: &ListJobHandles) -> Result<(), EnvironmentRegistryError> {
    if matches!(request.limit, Some(0)) {
        return Err(EnvironmentRegistryError::InvalidInput {
            message: "job handle list limit must be greater than zero".to_owned(),
        });
    }
    Ok(())
}

fn optional_u64_to_i64(
    value: Option<u64>,
    name: &'static str,
) -> Result<Option<i64>, EnvironmentRegistryError> {
    value
        .map(|value| {
            i64::try_from(value).map_err(|_| EnvironmentRegistryError::InvalidInput {
                message: format!("{name} is too large: {value}"),
            })
        })
        .transpose()
}

fn optional_i64_to_run_id(value: Option<i64>) -> Result<Option<RunId>, EnvironmentRegistryError> {
    value
        .map(|value| i64_to_u64(value, "created_by_run_id").map(RunId::new))
        .transpose()
}

fn optional_i64_to_turn_id(value: Option<i64>) -> Result<Option<TurnId>, EnvironmentRegistryError> {
    value
        .map(|value| i64_to_u64(value, "created_by_turn_id").map(TurnId::new))
        .transpose()
}

fn i64_to_u64(value: i64, name: &'static str) -> Result<u64, EnvironmentRegistryError> {
    if value < 0 {
        return Err(EnvironmentRegistryError::Store {
            message: format!("decode {name}: negative value {value}"),
        });
    }
    Ok(value as u64)
}

fn usize_to_i64(value: usize, name: &'static str) -> Result<i64, EnvironmentRegistryError> {
    i64::try_from(value).map_err(|_| EnvironmentRegistryError::InvalidInput {
        message: format!("{name} is too large: {value}"),
    })
}
