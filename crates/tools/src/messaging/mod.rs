//! Messaging toolset (P71 G5): the agent's explicit channel vocabulary for
//! sessions bound to a chat channel. Send/react/edit enqueue durable outbox
//! rows that the channel bridge delivers; noop is the structured way to
//! decline a reply. The bridge resolves the target chat from its session
//! binding, so the tools carry no channel addressing.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use engine::{
    FunctionToolSpec, RunId, SessionId, ToolKind, ToolName, ToolParallelism, ToolSpec,
    ToolTargetRequirement,
};
use messaging::{
    EnqueueOutboundMessage, MessagingError, OutboundOrigin, OutboundPayload, OutboxStore,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    error::{ToolError, ToolResult},
    runtime::{
        ToolBinding, ToolDocument, ToolExecutionMode, ToolInvocationOutput, ToolSpecBundle,
        decode_args, encode_output,
    },
};

pub const MESSAGE_SEND_TOOL_NAME: &str = "message_send";
pub const MESSAGE_REACT_TOOL_NAME: &str = "message_react";
pub const MESSAGE_EDIT_TOOL_NAME: &str = "message_edit";
pub const MESSAGE_NOOP_TOOL_NAME: &str = "message_noop";

pub const MESSAGING_LOGICAL_ID_PREFIX: &str = "messaging.";
pub const MESSAGING_ACTIVITY_TYPE: &str = "lightspeed.messaging";

/// Default per-session enqueue cap per minute, enforced at outbox admission.
pub const DEFAULT_MESSAGES_PER_MINUTE: u32 = 30;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct MessagingToolsetConfig {
    #[serde(default)]
    pub enabled: bool,
}

impl MessagingToolsetConfig {
    pub fn disabled() -> Self {
        Self { enabled: false }
    }

    pub fn enabled() -> Self {
        Self { enabled: true }
    }
}

pub fn is_messaging_tool(tool_name: &ToolName) -> bool {
    matches!(
        tool_name.as_str(),
        MESSAGE_SEND_TOOL_NAME
            | MESSAGE_REACT_TOOL_NAME
            | MESSAGE_EDIT_TOOL_NAME
            | MESSAGE_NOOP_TOOL_NAME
    )
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MessageSendArgs {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MessageReactArgs {
    pub message_id: String,
    pub emoji: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MessageEditArgs {
    pub message_id: String,
    pub text: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MessageNoopArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MessagingToolResult {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox_id: Option<String>,
}

pub fn messaging_tool_bundles(config: &MessagingToolsetConfig) -> ToolResult<Vec<ToolSpecBundle>> {
    if !config.enabled {
        return Ok(Vec::new());
    }
    Ok(vec![
        function_bundle(
            MESSAGE_SEND_TOOL_NAME,
            "Send a chat message to the conversation this session is bound to. \
             This is the only way your words reach the chat: your final output \
             is internal notes and is only delivered as a fallback when you \
             use no messaging tool in a turn. Sending several short messages \
             is fine. reply_to quote-replies to a channel message id (the \
             #id in message envelopes); use it in group chats or when \
             referring back to an older message, but omit it in a direct \
             chat when you are simply answering the latest message — plain \
             sends read more naturally there. Only use ids you have actually \
             seen, never guess.",
            json!({
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "Message text to deliver to the chat."
                    },
                    "reply_to": {
                        "type": ["string", "null"],
                        "description": "Optional channel message id to quote-reply to."
                    }
                },
                "required": ["text"],
                "additionalProperties": false
            }),
        )?,
        function_bundle(
            MESSAGE_REACT_TOOL_NAME,
            "React to a chat message with a single emoji. Often the right \
             acknowledgement for messages that need no text reply. Only use \
             message ids you have actually seen as #id markers in message \
             envelopes or delivery notes — never guess ids. Telegram accepts \
             only its standard reaction set (\u{1F44D} \u{2764} \u{1F525} \u{1F389} \u{1F602} \
             \u{1F62E} \u{1F622} \u{1F64F} and similar); prefer common emoji.",
            json!({
                "type": "object",
                "properties": {
                    "message_id": {
                        "type": "string",
                        "description": "Channel message id to react to (the #id in message envelopes)."
                    },
                    "emoji": {
                        "type": "string",
                        "description": "Single emoji, for example \"👍\"."
                    }
                },
                "required": ["message_id", "emoji"],
                "additionalProperties": false
            }),
        )?,
        function_bundle(
            MESSAGE_EDIT_TOOL_NAME,
            "Edit a chat message you previously sent. Only your own messages \
             can be edited; use the #id from the delivery note that appears \
             in context after one of your messages is delivered. Never guess \
             message ids.",
            json!({
                "type": "object",
                "properties": {
                    "message_id": {
                        "type": "string",
                        "description": "Channel message id of your own message to edit."
                    },
                    "text": {
                        "type": "string",
                        "description": "Replacement message text."
                    }
                },
                "required": ["message_id", "text"],
                "additionalProperties": false
            }),
        )?,
        function_bundle(
            MESSAGE_NOOP_TOOL_NAME,
            "Explicitly decline to reply to the chat for this turn. Use this \
             when no response is warranted (for example a plain \"ok\" or \
             \"thanks\"), so the conversation ends quietly instead of your \
             final output being delivered as a fallback.",
            json!({
                "type": "object",
                "properties": {
                    "reason": {
                        "type": ["string", "null"],
                        "description": "Optional short note on why no reply is needed."
                    }
                },
                "required": [],
                "additionalProperties": false
            }),
        )?,
    ])
}

pub fn messaging_tool_bindings(execution: ToolExecutionMode) -> Vec<ToolBinding> {
    [
        MESSAGE_SEND_TOOL_NAME,
        MESSAGE_REACT_TOOL_NAME,
        MESSAGE_EDIT_TOOL_NAME,
        MESSAGE_NOOP_TOOL_NAME,
    ]
    .into_iter()
    .map(|tool_name| {
        ToolBinding::new(
            ToolName::new(tool_name),
            format!("{MESSAGING_LOGICAL_ID_PREFIX}{tool_name}"),
            MESSAGING_ACTIVITY_TYPE,
            execution.clone(),
            ToolParallelism::Exclusive,
        )
    })
    .collect()
}

fn function_bundle(
    tool_name: &'static str,
    description: &'static str,
    input_schema: Value,
) -> ToolResult<ToolSpecBundle> {
    let description = ToolDocument::text("text/plain; charset=utf-8", description);
    let input_schema = ToolDocument::text(
        "application/schema+json",
        serde_json::to_string(&input_schema).map_err(|error| ToolError::InvalidRequest {
            message: format!("failed to encode {tool_name} schema: {error}"),
        })?,
    );
    Ok(ToolSpecBundle {
        spec: ToolSpec {
            name: ToolName::new(tool_name),
            kind: ToolKind::Function(FunctionToolSpec {
                model_name: None,
                description_ref: Some(description.blob_ref.clone()),
                input_schema_ref: input_schema.blob_ref.clone(),
                output_schema_ref: None,
                strict: Some(false),
                provider_options_ref: None,
            }),
            parallelism: ToolParallelism::Exclusive,
            target_requirement: ToolTargetRequirement::None,
        },
        documents: vec![description, input_schema],
    })
}

/// Executes messaging tool calls by enqueueing durable outbox rows. The
/// result reports durable-enqueue, not delivery confirmation.
#[derive(Clone)]
pub struct MessagingToolExecutor {
    outbox: Arc<dyn OutboxStore>,
    messages_per_minute: u32,
}

impl MessagingToolExecutor {
    pub fn new(outbox: Arc<dyn OutboxStore>) -> Self {
        Self {
            outbox,
            messages_per_minute: DEFAULT_MESSAGES_PER_MINUTE,
        }
    }

    pub fn with_messages_per_minute(mut self, messages_per_minute: u32) -> Self {
        self.messages_per_minute = messages_per_minute.max(1);
        self
    }

    pub async fn invoke(
        &self,
        session_id: &SessionId,
        run_id: RunId,
        tool_name: &ToolName,
        arguments: Value,
    ) -> ToolResult<ToolInvocationOutput> {
        let payload = match tool_name.as_str() {
            MESSAGE_SEND_TOOL_NAME => {
                let args: MessageSendArgs = decode_args(arguments)?;
                Some(OutboundPayload::Send {
                    text: args.text,
                    reply_to: args.reply_to,
                })
            }
            MESSAGE_REACT_TOOL_NAME => {
                let args: MessageReactArgs = decode_args(arguments)?;
                Some(OutboundPayload::React {
                    message_id: args.message_id,
                    emoji: args.emoji,
                })
            }
            MESSAGE_EDIT_TOOL_NAME => {
                let args: MessageEditArgs = decode_args(arguments)?;
                Some(OutboundPayload::Edit {
                    message_id: args.message_id,
                    text: args.text,
                })
            }
            MESSAGE_NOOP_TOOL_NAME => {
                let args: MessageNoopArgs = decode_args(arguments)?;
                let note = match args.reason {
                    Some(reason) if !reason.trim().is_empty() => {
                        format!("No reply will be sent to the chat ({reason}).")
                    }
                    _ => "No reply will be sent to the chat.".to_owned(),
                };
                return encode_output(
                    &MessagingToolResult {
                        status: "acknowledged".to_owned(),
                        outbox_id: None,
                    },
                    note,
                );
            }
            other => {
                return Err(ToolError::InvalidRequest {
                    message: format!("unknown messaging tool: {other}"),
                });
            }
        };
        let payload = payload.expect("payload set for non-noop tools");

        let now_ms = now_unix_ms();
        let window_start = now_ms - 60_000;
        let recent = self
            .outbox
            .count_enqueued_since(session_id, window_start)
            .await
            .map_err(messaging_tool_error)?;
        if recent >= u64::from(self.messages_per_minute) {
            return Err(ToolError::InvalidRequest {
                message: format!(
                    "messaging rate limit reached ({} messages/minute); wait before sending more",
                    self.messages_per_minute
                ),
            });
        }

        let enqueued = self
            .outbox
            .enqueue(EnqueueOutboundMessage {
                session_id: session_id.clone(),
                run_id: Some(run_id),
                origin: OutboundOrigin::ToolCall,
                payload,
                created_at_ms: now_ms,
            })
            .await
            .map_err(messaging_tool_error)?;

        encode_output(
            &MessagingToolResult {
                status: "enqueued".to_owned(),
                outbox_id: Some(enqueued.outbox_id.clone()),
            },
            format!(
                "Enqueued for delivery to the bound chat (outbox id {}). Delivery is asynchronous.",
                enqueued.outbox_id
            ),
        )
    }
}

fn messaging_tool_error(error: MessagingError) -> ToolError {
    match error {
        MessagingError::InvalidInput { message } | MessagingError::RateLimited { message } => {
            ToolError::InvalidRequest { message }
        }
        other => ToolError::InvalidRequest {
            message: other.to_string(),
        },
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use messaging::{InMemoryOutboxStore, OutboundStatus, ReadPendingOutbound};

    use super::*;

    fn executor() -> (Arc<InMemoryOutboxStore>, MessagingToolExecutor) {
        let store = Arc::new(InMemoryOutboxStore::new());
        let executor = MessagingToolExecutor::new(store.clone());
        (store, executor)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn message_send_enqueues_an_outbox_row() {
        let (store, executor) = executor();
        let output = executor
            .invoke(
                &SessionId::new("session_1"),
                RunId::new(7),
                &ToolName::new(MESSAGE_SEND_TOOL_NAME),
                json!({ "text": "hello chat", "reply_to": "4123" }),
            )
            .await
            .expect("invoke");
        assert!(output.model_visible_text.contains("Enqueued"));

        let pending = store
            .read_pending(ReadPendingOutbound {
                after_seq: 0,
                limit: 10,
            })
            .await
            .expect("read");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].status, OutboundStatus::Pending);
        assert_eq!(pending[0].run_id, Some(RunId::new(7)));
        assert_eq!(
            pending[0].payload,
            OutboundPayload::Send {
                text: "hello chat".to_owned(),
                reply_to: Some("4123".to_owned()),
            }
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn message_noop_writes_nothing() {
        let (store, executor) = executor();
        let output = executor
            .invoke(
                &SessionId::new("session_1"),
                RunId::new(1),
                &ToolName::new(MESSAGE_NOOP_TOOL_NAME),
                json!({ "reason": "user just said thanks" }),
            )
            .await
            .expect("invoke");
        assert!(output.model_visible_text.contains("No reply"));

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
    async fn rate_limit_rejects_beyond_cap() {
        let (_, executor) = executor();
        let executor = executor.with_messages_per_minute(2);
        let session = SessionId::new("session_1");
        for _ in 0..2 {
            executor
                .invoke(
                    &session,
                    RunId::new(1),
                    &ToolName::new(MESSAGE_SEND_TOOL_NAME),
                    json!({ "text": "hi" }),
                )
                .await
                .expect("invoke under cap");
        }
        let error = executor
            .invoke(
                &session,
                RunId::new(1),
                &ToolName::new(MESSAGE_SEND_TOOL_NAME),
                json!({ "text": "one too many" }),
            )
            .await
            .expect_err("rate limited");
        assert!(matches!(error, ToolError::InvalidRequest { .. }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bundles_cover_the_tool_family() {
        let bundles = messaging_tool_bundles(&MessagingToolsetConfig::enabled()).expect("bundles");
        let names: Vec<&str> = bundles
            .iter()
            .map(|bundle| bundle.spec.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                MESSAGE_SEND_TOOL_NAME,
                MESSAGE_REACT_TOOL_NAME,
                MESSAGE_EDIT_TOOL_NAME,
                MESSAGE_NOOP_TOOL_NAME
            ]
        );
        assert!(
            messaging_tool_bundles(&MessagingToolsetConfig::disabled())
                .expect("disabled")
                .is_empty()
        );
    }
}
