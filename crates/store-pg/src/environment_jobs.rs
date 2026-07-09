use async_trait::async_trait;
use engine::{RunId, SessionId, ToolCallId, TurnId};
use environments::{
    CreateJobHandle, EnvironmentInstanceId, EnvironmentJobGroupId, EnvironmentJobGroupRecord,
    EnvironmentJobGroupStatus, EnvironmentRegistryError, JobHandleRecord, JobHandleStore,
    ListJobHandles, ReserveEnvironmentJobGroup, UpdateEnvironmentJobGroupStatus,
};
use host_protocol::shared::JobId;
use sqlx::Row;

use crate::PgStore;

const GROUP_COLUMNS: &str = r#"
    instance_id, job_group_id, request_id, start_request_hash, status,
    created_at_ms, updated_at_ms, terminal_at_ms
"#;

const JOB_COLUMNS: &str = r#"
    instance_id, job_group_id, job_id, name, queue_key,
    created_by_session_id, created_by_run_id, created_by_turn_id,
    created_by_tool_call_id, created_at_ms, start_request_hash
"#;

#[async_trait]
impl JobHandleStore for PgStore {
    async fn reserve_job_group(
        &self,
        request: ReserveEnvironmentJobGroup,
    ) -> Result<EnvironmentJobGroupRecord, EnvironmentRegistryError> {
        self.ensure_universe()
            .await
            .map_err(|error| store_error("ensure universe", error))?;
        let record = request.into_record();
        record.validate()?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| sql_error("begin reserve job group", error))?;
        let status: Option<String> = sqlx::query_scalar(
            "SELECT status FROM environments WHERE universe_id = $1 AND instance_id = $2 FOR SHARE",
        )
        .bind(self.config.universe_id)
        .bind(record.instance_id.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| sql_error("read job group environment", error))?;
        let Some(status) = status else {
            return Err(not_found("environment_instance", &record.instance_id));
        };
        if matches!(status.as_str(), "closing" | "closed") {
            return invalid("cannot start jobs on a closing environment instance");
        }
        let query = format!(
            r#"
            INSERT INTO environment_job_groups (
                universe_id, instance_id, job_group_id, request_id,
                start_request_hash, status, created_at_ms, updated_at_ms, terminal_at_ms
            ) VALUES ($1,$2,$3,$4,$5,'starting',$6,$6,NULL)
            ON CONFLICT (universe_id, instance_id, job_group_id) DO UPDATE SET
                start_request_hash = environment_job_groups.start_request_hash
            WHERE environment_job_groups.start_request_hash = EXCLUDED.start_request_hash
            RETURNING {GROUP_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(record.instance_id.as_str())
            .bind(record.job_group_id.as_str())
            .bind(&record.request_id)
            .bind(&record.start_request_hash)
            .bind(record.created_at_ms)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| sql_error("reserve environment job group", error))?
            .ok_or_else(|| EnvironmentRegistryError::AlreadyExists {
                kind: "environment_job_group",
                id: format!("{}/{}", record.instance_id, record.job_group_id),
            })?;
        let record = group_from_row(&row)?;
        tx.commit()
            .await
            .map_err(|error| sql_error("commit reserve job group", error))?;
        Ok(record)
    }

    async fn read_job_group(
        &self,
        instance_id: &EnvironmentInstanceId,
        job_group_id: &EnvironmentJobGroupId,
    ) -> Result<EnvironmentJobGroupRecord, EnvironmentRegistryError> {
        let query = format!(
            "SELECT {GROUP_COLUMNS} FROM environment_job_groups WHERE universe_id = $1 AND instance_id = $2 AND job_group_id = $3"
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(instance_id.as_str())
            .bind(job_group_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| sql_error("read environment job group", error))?
            .ok_or_else(|| group_not_found(instance_id, job_group_id))?;
        group_from_row(&row)
    }

    async fn update_job_group_status(
        &self,
        request: UpdateEnvironmentJobGroupStatus,
    ) -> Result<EnvironmentJobGroupRecord, EnvironmentRegistryError> {
        let terminal_at_ms = request
            .status
            .is_terminal()
            .then_some(request.updated_at_ms);
        let query = format!(
            "UPDATE environment_job_groups SET status = $4, updated_at_ms = $5, terminal_at_ms = $6 WHERE universe_id = $1 AND instance_id = $2 AND job_group_id = $3 RETURNING {GROUP_COLUMNS}"
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(request.instance_id.as_str())
            .bind(request.job_group_id.as_str())
            .bind(group_status_to_str(request.status))
            .bind(request.updated_at_ms)
            .bind(terminal_at_ms)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| sql_error("update environment job group", error))?
            .ok_or_else(|| group_not_found(&request.instance_id, &request.job_group_id))?;
        group_from_row(&row)
    }

    async fn create_job_handles(
        &self,
        requests: Vec<CreateJobHandle>,
    ) -> Result<Vec<JobHandleRecord>, EnvironmentRegistryError> {
        let records = requests
            .into_iter()
            .map(CreateJobHandle::into_record)
            .collect::<Vec<_>>();
        for record in &records {
            record.validate()?;
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| sql_error("begin create job handles", error))?;
        let mut created = Vec::with_capacity(records.len());
        for record in records {
            let query = format!(
                r#"
                INSERT INTO environment_jobs (
                    universe_id, instance_id, job_group_id, job_id, name, queue_key,
                    created_by_session_id, created_by_run_id, created_by_turn_id,
                    created_by_tool_call_id, created_at_ms, start_request_hash
                ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)
                ON CONFLICT (universe_id, instance_id, job_id) DO UPDATE SET
                    start_request_hash = environment_jobs.start_request_hash
                WHERE environment_jobs.start_request_hash = EXCLUDED.start_request_hash
                RETURNING {JOB_COLUMNS}
                "#
            );
            let row = sqlx::query(&query)
                .bind(self.config.universe_id)
                .bind(record.instance_id.as_str())
                .bind(record.job_group_id.as_str())
                .bind(record.job_id.as_str())
                .bind(record.name.as_deref())
                .bind(record.queue_key.as_deref())
                .bind(record.created_by_session_id.as_ref().map(SessionId::as_str))
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
                .bind(&record.start_request_hash)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|error| sql_error("create environment job handle", error))?
                .ok_or_else(|| EnvironmentRegistryError::AlreadyExists {
                    kind: "job_handle",
                    id: format!("{}/{}", record.instance_id, record.job_id),
                })?;
            created.push(job_from_row(&row)?);
        }
        tx.commit()
            .await
            .map_err(|error| sql_error("commit create job handles", error))?;
        Ok(created)
    }

    async fn read_job_handle(
        &self,
        instance_id: &EnvironmentInstanceId,
        job_id: &JobId,
    ) -> Result<JobHandleRecord, EnvironmentRegistryError> {
        let query = format!(
            "SELECT {JOB_COLUMNS} FROM environment_jobs WHERE universe_id = $1 AND instance_id = $2 AND job_id = $3"
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(instance_id.as_str())
            .bind(job_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| sql_error("read environment job handle", error))?
            .ok_or_else(|| job_not_found(instance_id, job_id))?;
        job_from_row(&row)
    }

    async fn list_job_handles(
        &self,
        request: ListJobHandles,
    ) -> Result<Vec<JobHandleRecord>, EnvironmentRegistryError> {
        if matches!(request.limit, Some(0)) {
            return invalid("job handle list limit must be greater than zero");
        }
        let mut query =
            format!("SELECT {JOB_COLUMNS} FROM environment_jobs WHERE universe_id = $1");
        let mut next = 2;
        if request.instance_id.is_some() {
            query.push_str(&format!(" AND instance_id = ${next}"));
            next += 1;
        }
        if request.job_group_id.is_some() {
            query.push_str(&format!(" AND job_group_id = ${next}"));
            next += 1;
        }
        if request.created_by_session_id.is_some() {
            query.push_str(&format!(" AND created_by_session_id = ${next}"));
            next += 1;
        }
        query.push_str(" ORDER BY created_at_ms DESC, instance_id, job_id");
        if request.limit.is_some() {
            query.push_str(&format!(" LIMIT ${next}"));
        }
        let mut sql = sqlx::query(&query).bind(self.config.universe_id);
        if let Some(id) = request.instance_id.as_ref() {
            sql = sql.bind(id.as_str());
        }
        if let Some(id) = request.job_group_id.as_ref() {
            sql = sql.bind(id.as_str());
        }
        if let Some(id) = request.created_by_session_id.as_ref() {
            sql = sql.bind(id.as_str());
        }
        if let Some(limit) = request.limit {
            sql = sql.bind(usize_to_i64(limit)?);
        }
        let rows = sql
            .fetch_all(&self.pool)
            .await
            .map_err(|error| sql_error("list environment job handles", error))?;
        rows.iter().map(job_from_row).collect()
    }

    async fn delete_job_handle(
        &self,
        instance_id: &EnvironmentInstanceId,
        job_id: &JobId,
    ) -> Result<JobHandleRecord, EnvironmentRegistryError> {
        let query = format!(
            "DELETE FROM environment_jobs WHERE universe_id = $1 AND instance_id = $2 AND job_id = $3 RETURNING {JOB_COLUMNS}"
        );
        let row = sqlx::query(&query)
            .bind(self.config.universe_id)
            .bind(instance_id.as_str())
            .bind(job_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| sql_error("delete environment job handle", error))?
            .ok_or_else(|| job_not_found(instance_id, job_id))?;
        job_from_row(&row)
    }
}

fn group_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<EnvironmentJobGroupRecord, EnvironmentRegistryError> {
    let status: String = row
        .try_get("status")
        .map_err(|error| sql_error("decode job group status", error))?;
    let record = EnvironmentJobGroupRecord {
        instance_id: EnvironmentInstanceId::try_new(column(row, "instance_id")?)
            .map_err(|error| store_message(format!("decode instance id: {error}")))?,
        job_group_id: EnvironmentJobGroupId::try_new(column(row, "job_group_id")?)
            .map_err(|error| store_message(format!("decode job group id: {error}")))?,
        request_id: column(row, "request_id")?,
        start_request_hash: column(row, "start_request_hash")?,
        status: group_status_from_str(&status)?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|error| sql_error("decode group created_at_ms", error))?,
        updated_at_ms: row
            .try_get("updated_at_ms")
            .map_err(|error| sql_error("decode group updated_at_ms", error))?,
        terminal_at_ms: row
            .try_get("terminal_at_ms")
            .map_err(|error| sql_error("decode group terminal_at_ms", error))?,
    };
    record.validate()?;
    Ok(record)
}

fn job_from_row(row: &sqlx::postgres::PgRow) -> Result<JobHandleRecord, EnvironmentRegistryError> {
    let session_id: Option<String> = row
        .try_get("created_by_session_id")
        .map_err(|error| sql_error("decode job session id", error))?;
    let run_id: Option<i64> = row
        .try_get("created_by_run_id")
        .map_err(|error| sql_error("decode job run id", error))?;
    let turn_id: Option<i64> = row
        .try_get("created_by_turn_id")
        .map_err(|error| sql_error("decode job turn id", error))?;
    let tool_call_id: Option<String> = row
        .try_get("created_by_tool_call_id")
        .map_err(|error| sql_error("decode job tool call id", error))?;
    let record = JobHandleRecord {
        instance_id: EnvironmentInstanceId::try_new(column(row, "instance_id")?)
            .map_err(|error| store_message(format!("decode instance id: {error}")))?,
        job_group_id: EnvironmentJobGroupId::try_new(column(row, "job_group_id")?)
            .map_err(|error| store_message(format!("decode job group id: {error}")))?,
        job_id: JobId::new(column(row, "job_id")?),
        name: row
            .try_get("name")
            .map_err(|error| sql_error("decode job name", error))?,
        queue_key: row
            .try_get("queue_key")
            .map_err(|error| sql_error("decode job queue key", error))?,
        created_by_session_id: session_id
            .map(SessionId::try_new)
            .transpose()
            .map_err(|error| store_message(format!("decode job session id: {error}")))?,
        created_by_run_id: optional_i64(run_id, "created_by_run_id")?.map(RunId::new),
        created_by_turn_id: optional_i64(turn_id, "created_by_turn_id")?.map(TurnId::new),
        created_by_tool_call_id: tool_call_id
            .map(ToolCallId::try_new)
            .transpose()
            .map_err(|error| store_message(format!("decode job tool call id: {error}")))?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|error| sql_error("decode job created_at_ms", error))?,
        start_request_hash: column(row, "start_request_hash")?,
    };
    record.validate()?;
    Ok(record)
}

fn group_status_to_str(value: EnvironmentJobGroupStatus) -> &'static str {
    match value {
        EnvironmentJobGroupStatus::Starting => "starting",
        EnvironmentJobGroupStatus::Running => "running",
        EnvironmentJobGroupStatus::Terminal => "terminal",
        EnvironmentJobGroupStatus::Failed => "failed",
    }
}
fn group_status_from_str(
    value: &str,
) -> Result<EnvironmentJobGroupStatus, EnvironmentRegistryError> {
    match value {
        "starting" => Ok(EnvironmentJobGroupStatus::Starting),
        "running" => Ok(EnvironmentJobGroupStatus::Running),
        "terminal" => Ok(EnvironmentJobGroupStatus::Terminal),
        "failed" => Ok(EnvironmentJobGroupStatus::Failed),
        other => Err(store_message(format!("unknown job group status: {other}"))),
    }
}

fn optional_u64_to_i64(
    value: Option<u64>,
    name: &str,
) -> Result<Option<i64>, EnvironmentRegistryError> {
    value
        .map(|value| {
            i64::try_from(value).map_err(|_| EnvironmentRegistryError::InvalidInput {
                message: format!("{name} is too large"),
            })
        })
        .transpose()
}
fn optional_i64(value: Option<i64>, name: &str) -> Result<Option<u64>, EnvironmentRegistryError> {
    value
        .map(|value| u64::try_from(value).map_err(|_| store_message(format!("negative {name}"))))
        .transpose()
}
fn usize_to_i64(value: usize) -> Result<i64, EnvironmentRegistryError> {
    i64::try_from(value).map_err(|_| EnvironmentRegistryError::InvalidInput {
        message: "job list limit is too large".to_owned(),
    })
}
fn column(row: &sqlx::postgres::PgRow, name: &str) -> Result<String, EnvironmentRegistryError> {
    row.try_get(name)
        .map_err(|error| sql_error("decode environment job column", error))
}
fn not_found(kind: &'static str, id: &impl ToString) -> EnvironmentRegistryError {
    EnvironmentRegistryError::NotFound {
        kind,
        id: id.to_string(),
    }
}
fn group_not_found(
    instance_id: &EnvironmentInstanceId,
    group_id: &EnvironmentJobGroupId,
) -> EnvironmentRegistryError {
    EnvironmentRegistryError::NotFound {
        kind: "environment_job_group",
        id: format!("{instance_id}/{group_id}"),
    }
}
fn job_not_found(instance_id: &EnvironmentInstanceId, job_id: &JobId) -> EnvironmentRegistryError {
    EnvironmentRegistryError::NotFound {
        kind: "job_handle",
        id: format!("{instance_id}/{job_id}"),
    }
}
fn invalid<T>(message: impl Into<String>) -> Result<T, EnvironmentRegistryError> {
    Err(EnvironmentRegistryError::InvalidInput {
        message: message.into(),
    })
}
fn store_error(action: &str, error: crate::PgStoreError) -> EnvironmentRegistryError {
    EnvironmentRegistryError::Store {
        message: format!("{action}: {error}"),
    }
}
fn sql_error(action: &str, error: sqlx::Error) -> EnvironmentRegistryError {
    EnvironmentRegistryError::Store {
        message: format!("{action}: {error}"),
    }
}
fn store_message(message: impl Into<String>) -> EnvironmentRegistryError {
    EnvironmentRegistryError::Store {
        message: message.into(),
    }
}
