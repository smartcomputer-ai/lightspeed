use async_trait::async_trait;
use engine::{RunId, SessionId};
use messaging::{
    EnqueueOutboundMessage, MessagingError, OutboundAck, OutboundMessage, OutboundOrigin,
    OutboundPayload, OutboundStatus, OutboxStore, ReadPendingOutbound, acked_message,
    validate_payload,
};
use sqlx::Row;
use uuid::Uuid;

use crate::PgStore;

#[async_trait]
impl OutboxStore for PgStore {
    async fn enqueue(
        &self,
        message: EnqueueOutboundMessage,
    ) -> Result<OutboundMessage, MessagingError> {
        self.ensure_universe()
            .await
            .map_err(|error| outbox_store_error("ensure universe", error))?;
        validate_payload(&message.payload)?;
        let outbox_id = format!("outbox_{}", Uuid::new_v4().simple());
        let payload_json = serde_json::to_value(&message.payload)
            .map_err(|error| outbox_store_error("encode payload", error))?;
        let row = sqlx::query(
            r#"
            INSERT INTO messaging_outbox (
                universe_id,
                outbox_id,
                session_id,
                run_id,
                origin,
                payload_json,
                status,
                attempts,
                created_at_ms,
                updated_at_ms
            )
            VALUES ($1, $2, $3, $4, $5, $6, 'pending', 0, $7, $7)
            RETURNING
                seq,
                outbox_id,
                session_id,
                run_id,
                origin,
                payload_json,
                status,
                attempts,
                channel_message_id,
                error,
                created_at_ms,
                updated_at_ms
            "#,
        )
        .bind(self.config.universe_id)
        .bind(&outbox_id)
        .bind(message.session_id.as_str())
        .bind(message.run_id.map(|run_id| run_id.as_u64() as i64))
        .bind(origin_column(message.origin))
        .bind(payload_json)
        .bind(message.created_at_ms)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| outbox_store_error("enqueue outbound message", error))?;
        outbound_message_from_row(&row)
    }

    async fn read_pending(
        &self,
        request: ReadPendingOutbound,
    ) -> Result<Vec<OutboundMessage>, MessagingError> {
        let rows = sqlx::query(
            r#"
            SELECT
                seq,
                outbox_id,
                session_id,
                run_id,
                origin,
                payload_json,
                status,
                attempts,
                channel_message_id,
                error,
                created_at_ms,
                updated_at_ms
            FROM messaging_outbox
            WHERE universe_id = $1
              AND status = 'pending'
              AND seq > $2
            ORDER BY seq ASC
            LIMIT $3
            "#,
        )
        .bind(self.config.universe_id)
        .bind(request.after_seq as i64)
        .bind(request.limit.min(256) as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| outbox_store_error("read pending outbound messages", error))?;
        rows.iter().map(outbound_message_from_row).collect()
    }

    async fn ack(
        &self,
        outbox_id: &str,
        ack: OutboundAck,
    ) -> Result<OutboundMessage, MessagingError> {
        let row = sqlx::query(
            r#"
            SELECT
                seq,
                outbox_id,
                session_id,
                run_id,
                origin,
                payload_json,
                status,
                attempts,
                channel_message_id,
                error,
                created_at_ms,
                updated_at_ms
            FROM messaging_outbox
            WHERE universe_id = $1 AND outbox_id = $2
            "#,
        )
        .bind(self.config.universe_id)
        .bind(outbox_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| outbox_store_error("read outbound message", error))?
        .ok_or_else(|| MessagingError::NotFound {
            outbox_id: outbox_id.to_owned(),
        })?;
        let message = outbound_message_from_row(&row)?;
        let now_ms = now_unix_ms();
        let updated = acked_message(message, ack, now_ms);

        sqlx::query(
            r#"
            UPDATE messaging_outbox
            SET status = $3,
                attempts = $4,
                channel_message_id = $5,
                error = $6,
                updated_at_ms = $7
            WHERE universe_id = $1 AND outbox_id = $2
            "#,
        )
        .bind(self.config.universe_id)
        .bind(outbox_id)
        .bind(status_column(updated.status))
        .bind(updated.attempts as i32)
        .bind(updated.channel_message_id.as_deref())
        .bind(updated.error.as_deref())
        .bind(updated.updated_at_ms)
        .execute(&self.pool)
        .await
        .map_err(|error| outbox_store_error("ack outbound message", error))?;
        Ok(updated)
    }

    async fn count_enqueued_since(
        &self,
        session_id: &SessionId,
        since_ms: i64,
    ) -> Result<u64, MessagingError> {
        let row = sqlx::query(
            r#"
            SELECT COUNT(*) AS enqueued
            FROM messaging_outbox
            WHERE universe_id = $1
              AND session_id = $2
              AND created_at_ms >= $3
            "#,
        )
        .bind(self.config.universe_id)
        .bind(session_id.as_str())
        .bind(since_ms)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| outbox_store_error("count outbound messages", error))?;
        let count: i64 = row
            .try_get("enqueued")
            .map_err(|error| outbox_store_error("decode count", error))?;
        Ok(count.max(0) as u64)
    }
}

pub(crate) fn outbound_message_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<OutboundMessage, MessagingError> {
    let decode = |what: &str, error: sqlx::Error| outbox_store_error(what, error);
    let seq: i64 = row.try_get("seq").map_err(|e| decode("seq", e))?;
    let payload_json: serde_json::Value = row
        .try_get("payload_json")
        .map_err(|e| decode("payload", e))?;
    let payload: OutboundPayload = serde_json::from_value(payload_json)
        .map_err(|error| outbox_store_error("decode payload", error))?;
    let origin: String = row.try_get("origin").map_err(|e| decode("origin", e))?;
    let status: String = row.try_get("status").map_err(|e| decode("status", e))?;
    let session_id: String = row
        .try_get("session_id")
        .map_err(|e| decode("session_id", e))?;
    let run_id: Option<i64> = row.try_get("run_id").map_err(|e| decode("run_id", e))?;
    let attempts: i32 = row.try_get("attempts").map_err(|e| decode("attempts", e))?;
    Ok(OutboundMessage {
        seq: seq.max(0) as u64,
        outbox_id: row
            .try_get("outbox_id")
            .map_err(|e| decode("outbox_id", e))?,
        session_id: SessionId::try_new(session_id)
            .map_err(|error| outbox_store_error("decode session id", error))?,
        run_id: run_id.map(|value| RunId::new(value.max(0) as u64)),
        origin: origin_from_column(&origin)?,
        payload,
        status: status_from_column(&status)?,
        attempts: attempts.max(0) as u32,
        channel_message_id: row
            .try_get("channel_message_id")
            .map_err(|e| decode("channel_message_id", e))?,
        error: row.try_get("error").map_err(|e| decode("error", e))?,
        created_at_ms: row
            .try_get("created_at_ms")
            .map_err(|e| decode("created_at_ms", e))?,
        updated_at_ms: row
            .try_get("updated_at_ms")
            .map_err(|e| decode("updated_at_ms", e))?,
    })
}

fn origin_column(origin: OutboundOrigin) -> &'static str {
    match origin {
        OutboundOrigin::ToolCall => "tool_call",
        OutboundOrigin::FinalText => "final_text",
        OutboundOrigin::Trigger => "trigger",
    }
}

fn origin_from_column(value: &str) -> Result<OutboundOrigin, MessagingError> {
    match value {
        "tool_call" => Ok(OutboundOrigin::ToolCall),
        "final_text" => Ok(OutboundOrigin::FinalText),
        "trigger" => Ok(OutboundOrigin::Trigger),
        other => Err(MessagingError::Store {
            message: format!("unknown outbox origin: {other}"),
        }),
    }
}

fn status_column(status: OutboundStatus) -> &'static str {
    match status {
        OutboundStatus::Pending => "pending",
        OutboundStatus::Delivered => "delivered",
        OutboundStatus::Failed => "failed",
    }
}

fn status_from_column(value: &str) -> Result<OutboundStatus, MessagingError> {
    match value {
        "pending" => Ok(OutboundStatus::Pending),
        "delivered" => Ok(OutboundStatus::Delivered),
        "failed" => Ok(OutboundStatus::Failed),
        other => Err(MessagingError::Store {
            message: format!("unknown outbox status: {other}"),
        }),
    }
}

fn now_unix_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn outbox_store_error(what: &str, error: impl std::fmt::Display) -> MessagingError {
    MessagingError::Store {
        message: format!("{what}: {error}"),
    }
}
