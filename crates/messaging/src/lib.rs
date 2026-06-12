//! Channel-neutral outbound message types and the delivery outbox store
//! (P71 G4). The outbox is the single durable spine between sessions that
//! decide to speak (messaging tools, automatic delivery, future trigger
//! announcements) and the channel bridges that deliver.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use engine::{RunId, SessionId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// What the bridge must do at the channel. The bridge resolves the target
/// chat from its session binding; the outbox only records the session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutboundPayload {
    Send {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_to: Option<String>,
    },
    React {
        message_id: String,
        emoji: String,
    },
    Edit {
        message_id: String,
        text: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboundOrigin {
    ToolCall,
    FinalText,
    Trigger,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboundStatus {
    Pending,
    Delivered,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboundMessage {
    /// Monotonic sequence assigned by the store; the read cursor.
    pub seq: u64,
    pub outbox_id: String,
    pub session_id: SessionId,
    pub run_id: Option<RunId>,
    pub origin: OutboundOrigin,
    pub payload: OutboundPayload,
    pub status: OutboundStatus,
    pub attempts: u32,
    pub channel_message_id: Option<String>,
    pub error: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnqueueOutboundMessage {
    pub session_id: SessionId,
    pub run_id: Option<RunId>,
    pub origin: OutboundOrigin,
    pub payload: OutboundPayload,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadPendingOutbound {
    /// Return pending entries with `seq` greater than this cursor.
    pub after_seq: u64,
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutboundAck {
    Delivered {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel_message_id: Option<String>,
    },
    Failed {
        error: String,
        retryable: bool,
    },
}

/// Retryable failures re-enter `Pending` until this attempt cap, then park
/// as `Failed` for inspection.
pub const MAX_DELIVERY_ATTEMPTS: u32 = 5;

pub const MAX_OUTBOUND_TEXT_CHARS: usize = 8_000;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MessagingError {
    #[error("outbox message not found: {outbox_id}")]
    NotFound { outbox_id: String },
    #[error("invalid messaging request: {message}")]
    InvalidInput { message: String },
    #[error("messaging rate limit exceeded: {message}")]
    RateLimited { message: String },
    #[error("messaging store failure: {message}")]
    Store { message: String },
}

pub fn validate_payload(payload: &OutboundPayload) -> Result<(), MessagingError> {
    let invalid = |message: String| Err(MessagingError::InvalidInput { message });
    match payload {
        OutboundPayload::Send { text, reply_to } => {
            if text.trim().is_empty() {
                return invalid("message text must not be empty".to_owned());
            }
            if text.chars().count() > MAX_OUTBOUND_TEXT_CHARS {
                return invalid(format!(
                    "message text exceeds {MAX_OUTBOUND_TEXT_CHARS} characters"
                ));
            }
            if reply_to.as_deref().is_some_and(|id| id.trim().is_empty()) {
                return invalid("reply_to must not be empty when set".to_owned());
            }
            Ok(())
        }
        OutboundPayload::React { message_id, emoji } => {
            if message_id.trim().is_empty() {
                return invalid("react message_id must not be empty".to_owned());
            }
            if emoji.trim().is_empty() || emoji.chars().count() > 8 {
                return invalid("react emoji must be a single short emoji".to_owned());
            }
            Ok(())
        }
        OutboundPayload::Edit { message_id, text } => {
            if message_id.trim().is_empty() {
                return invalid("edit message_id must not be empty".to_owned());
            }
            if text.trim().is_empty() {
                return invalid("edit text must not be empty".to_owned());
            }
            if text.chars().count() > MAX_OUTBOUND_TEXT_CHARS {
                return invalid(format!(
                    "edit text exceeds {MAX_OUTBOUND_TEXT_CHARS} characters"
                ));
            }
            Ok(())
        }
    }
}

#[async_trait]
pub trait OutboxStore: Send + Sync {
    async fn enqueue(
        &self,
        message: EnqueueOutboundMessage,
    ) -> Result<OutboundMessage, MessagingError>;

    /// Pending entries with `seq > after_seq`, oldest first. Entries stay
    /// visible until acked, so a restarted consumer re-reads unacked work by
    /// resetting its cursor (single-consumer model for the first version).
    async fn read_pending(
        &self,
        request: ReadPendingOutbound,
    ) -> Result<Vec<OutboundMessage>, MessagingError>;

    async fn ack(
        &self,
        outbox_id: &str,
        ack: OutboundAck,
    ) -> Result<OutboundMessage, MessagingError>;

    /// Number of messages enqueued for `session_id` since `since_ms`; the
    /// rate-cap input at admission.
    async fn count_enqueued_since(
        &self,
        session_id: &SessionId,
        since_ms: i64,
    ) -> Result<u64, MessagingError>;
}

fn apply_ack(message: &mut OutboundMessage, ack: OutboundAck, now_ms: i64) {
    message.attempts += 1;
    message.updated_at_ms = now_ms;
    match ack {
        OutboundAck::Delivered { channel_message_id } => {
            message.status = OutboundStatus::Delivered;
            message.channel_message_id = channel_message_id;
            message.error = None;
        }
        OutboundAck::Failed { error, retryable } => {
            message.error = Some(error);
            message.status = if retryable && message.attempts < MAX_DELIVERY_ATTEMPTS {
                OutboundStatus::Pending
            } else {
                OutboundStatus::Failed
            };
        }
    }
}

#[derive(Default)]
struct InMemoryOutboxState {
    next_seq: u64,
    messages: BTreeMap<u64, OutboundMessage>,
}

/// In-memory store for tests and non-durable local runs.
#[derive(Clone, Default)]
pub struct InMemoryOutboxStore {
    inner: Arc<RwLock<InMemoryOutboxState>>,
}

impl InMemoryOutboxStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl OutboxStore for InMemoryOutboxStore {
    async fn enqueue(
        &self,
        message: EnqueueOutboundMessage,
    ) -> Result<OutboundMessage, MessagingError> {
        validate_payload(&message.payload)?;
        let mut state = self.inner.write().expect("outbox lock");
        state.next_seq += 1;
        let seq = state.next_seq;
        let record = OutboundMessage {
            seq,
            outbox_id: format!("outbox_{seq}"),
            session_id: message.session_id,
            run_id: message.run_id,
            origin: message.origin,
            payload: message.payload,
            status: OutboundStatus::Pending,
            attempts: 0,
            channel_message_id: None,
            error: None,
            created_at_ms: message.created_at_ms,
            updated_at_ms: message.created_at_ms,
        };
        state.messages.insert(seq, record.clone());
        Ok(record)
    }

    async fn read_pending(
        &self,
        request: ReadPendingOutbound,
    ) -> Result<Vec<OutboundMessage>, MessagingError> {
        let state = self.inner.read().expect("outbox lock");
        Ok(state
            .messages
            .range(request.after_seq.saturating_add(1)..)
            .filter(|(_, message)| message.status == OutboundStatus::Pending)
            .take(request.limit)
            .map(|(_, message)| message.clone())
            .collect())
    }

    async fn ack(
        &self,
        outbox_id: &str,
        ack: OutboundAck,
    ) -> Result<OutboundMessage, MessagingError> {
        let mut state = self.inner.write().expect("outbox lock");
        let message = state
            .messages
            .values_mut()
            .find(|message| message.outbox_id == outbox_id)
            .ok_or_else(|| MessagingError::NotFound {
                outbox_id: outbox_id.to_owned(),
            })?;
        apply_ack(message, ack, message.updated_at_ms);
        Ok(message.clone())
    }

    async fn count_enqueued_since(
        &self,
        session_id: &SessionId,
        since_ms: i64,
    ) -> Result<u64, MessagingError> {
        let state = self.inner.read().expect("outbox lock");
        Ok(state
            .messages
            .values()
            .filter(|message| {
                &message.session_id == session_id && message.created_at_ms >= since_ms
            })
            .count() as u64)
    }
}

/// Shared ack-state transition for store implementations.
pub fn acked_message(
    mut message: OutboundMessage,
    ack: OutboundAck,
    now_ms: i64,
) -> OutboundMessage {
    apply_ack(&mut message, ack, now_ms);
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enqueue_request(session: &str, text: &str) -> EnqueueOutboundMessage {
        EnqueueOutboundMessage {
            session_id: SessionId::new(session),
            run_id: Some(RunId::new(1)),
            origin: OutboundOrigin::ToolCall,
            payload: OutboundPayload::Send {
                text: text.to_owned(),
                reply_to: None,
            },
            created_at_ms: 10,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn enqueue_read_ack_round_trip() {
        let store = InMemoryOutboxStore::new();
        let first = store
            .enqueue(enqueue_request("session_1", "hello"))
            .await
            .expect("enqueue");
        let second = store
            .enqueue(enqueue_request("session_1", "world"))
            .await
            .expect("enqueue");
        assert!(second.seq > first.seq);

        let pending = store
            .read_pending(ReadPendingOutbound {
                after_seq: 0,
                limit: 10,
            })
            .await
            .expect("read");
        assert_eq!(pending.len(), 2);

        store
            .ack(
                &first.outbox_id,
                OutboundAck::Delivered {
                    channel_message_id: Some("42".to_owned()),
                },
            )
            .await
            .expect("ack");

        let pending = store
            .read_pending(ReadPendingOutbound {
                after_seq: 0,
                limit: 10,
            })
            .await
            .expect("read");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].outbox_id, second.outbox_id);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retryable_failures_repend_until_the_attempt_cap() {
        let store = InMemoryOutboxStore::new();
        let message = store
            .enqueue(enqueue_request("session_1", "hello"))
            .await
            .expect("enqueue");

        for attempt in 1..MAX_DELIVERY_ATTEMPTS {
            let acked = store
                .ack(
                    &message.outbox_id,
                    OutboundAck::Failed {
                        error: "channel offline".to_owned(),
                        retryable: true,
                    },
                )
                .await
                .expect("ack");
            assert_eq!(acked.attempts, attempt);
            assert_eq!(acked.status, OutboundStatus::Pending, "attempt {attempt}");
        }

        let parked = store
            .ack(
                &message.outbox_id,
                OutboundAck::Failed {
                    error: "channel offline".to_owned(),
                    retryable: true,
                },
            )
            .await
            .expect("ack");
        assert_eq!(parked.status, OutboundStatus::Failed);

        let pending = store
            .read_pending(ReadPendingOutbound {
                after_seq: 0,
                limit: 10,
            })
            .await
            .expect("read");
        assert!(pending.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn non_retryable_failure_parks_immediately() {
        let store = InMemoryOutboxStore::new();
        let message = store
            .enqueue(enqueue_request("session_1", "hello"))
            .await
            .expect("enqueue");
        let acked = store
            .ack(
                &message.outbox_id,
                OutboundAck::Failed {
                    error: "chat not found".to_owned(),
                    retryable: false,
                },
            )
            .await
            .expect("ack");
        assert_eq!(acked.status, OutboundStatus::Failed);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_invalid_payloads() {
        let store = InMemoryOutboxStore::new();
        let error = store
            .enqueue(EnqueueOutboundMessage {
                payload: OutboundPayload::Send {
                    text: "   ".to_owned(),
                    reply_to: None,
                },
                ..enqueue_request("session_1", "x")
            })
            .await
            .expect_err("empty text rejected");
        assert!(matches!(error, MessagingError::InvalidInput { .. }));

        let error = store
            .enqueue(EnqueueOutboundMessage {
                payload: OutboundPayload::React {
                    message_id: String::new(),
                    emoji: "👍".to_owned(),
                },
                ..enqueue_request("session_1", "x")
            })
            .await
            .expect_err("empty message id rejected");
        assert!(matches!(error, MessagingError::InvalidInput { .. }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn counts_enqueued_messages_per_session_window() {
        let store = InMemoryOutboxStore::new();
        store
            .enqueue(enqueue_request("session_1", "one"))
            .await
            .expect("enqueue");
        store
            .enqueue(enqueue_request("session_2", "two"))
            .await
            .expect("enqueue");
        let count = store
            .count_enqueued_since(&SessionId::new("session_1"), 0)
            .await
            .expect("count");
        assert_eq!(count, 1);
        let none = store
            .count_enqueued_since(&SessionId::new("session_1"), 100)
            .await
            .expect("count");
        assert_eq!(none, 0);
    }
}
