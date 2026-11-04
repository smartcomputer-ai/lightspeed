//! CEL filter evaluation and route computation at admission, where the
//! payload is in hand. Filters fail closed: an expression that errors (or
//! yields a non-boolean) refuses the event and reports why, so a broken
//! filter surfaces on the trigger instead of silently delivering. Route
//! keys fail open: a broken key expression falls back to the preset key
//! and then to a shared `default` key, so an event is never dropped by a
//! routing typo.

use std::collections::BTreeMap;

use api::{BotEventDocument, BotId, BotTriggerRoute, WebhookPreset};
use serde_json::Value;

use crate::ids::{bot_keyed_session_id, bot_per_event_session_id};
use crate::poll::number_to_string;
use crate::records::{RoutedSession, RoutedSessionClosePolicy};

/// Longest route key taken from a CEL expression or a preset (characters).
pub const MAX_ROUTE_KEY_CHARS: usize = 200;
/// Longest routed-session label taken from a preset (characters).
pub const MAX_ROUTE_LABEL_CHARS: usize = 200;
/// Shared key of `perKey` routing when neither the expression nor the
/// preset yields one.
pub const DEFAULT_ROUTE_KEY: &str = "default";
const PER_EVENT_LABEL_CHARS: usize = 24;

/// Parse-only check of a CEL expression (filters and route keys).
pub fn validate_expression(expression: &str) -> Result<(), String> {
    cel::Program::compile(expression)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// The `event` variable of a filter: envelope identity without the
/// payload. `sender` is the sending bot's id for bot-originated events and
/// `null` in CEL otherwise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterEvent {
    pub id: String,
    pub kind: String,
    pub source: String,
    pub occurred_at_ms: i64,
    pub sender: Option<String>,
}

/// Everything a filter or route key may read: `event`, `data` (the payload;
/// `{}` when absent), and `headers` (lowercased, sanitized).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterContext {
    pub event: FilterEvent,
    pub data: Value,
    pub headers: BTreeMap<String, String>,
}

impl FilterContext {
    /// The admission context of a stored envelope.
    pub fn from_document(event_id: impl Into<String>, document: &BotEventDocument) -> Self {
        Self {
            event: FilterEvent {
                id: event_id.into(),
                kind: document.kind.clone(),
                source: document.source.clone(),
                occurred_at_ms: document.occurred_at_ms,
                sender: document
                    .sender
                    .as_ref()
                    .map(|sender| sender.bot.to_string()),
            },
            data: document.data.clone().unwrap_or(Value::Null),
            headers: document.headers.clone(),
        }
    }

    fn cel_context(&self) -> Result<cel::Context<'static>, String> {
        let mut event = serde_json::Map::new();
        event.insert("id".to_owned(), Value::String(self.event.id.clone()));
        event.insert("kind".to_owned(), Value::String(self.event.kind.clone()));
        event.insert(
            "source".to_owned(),
            Value::String(self.event.source.clone()),
        );
        event.insert(
            "occurredAtMs".to_owned(),
            Value::Number(self.event.occurred_at_ms.into()),
        );
        event.insert(
            "sender".to_owned(),
            self.event.sender.clone().map_or(Value::Null, Value::String),
        );
        let empty = Value::Object(serde_json::Map::new());
        let data = if self.data.is_null() {
            &empty
        } else {
            &self.data
        };

        let mut context = cel::Context::default();
        context
            .add_variable("event", Value::Object(event))
            .map_err(|error| format!("event is not a CEL value: {error}"))?;
        context
            .add_variable("data", data)
            .map_err(|error| format!("data is not a CEL value: {error}"))?;
        context
            .add_variable("headers", &self.headers)
            .map_err(|error| format!("headers are not a CEL value: {error}"))?;
        Ok(context)
    }
}

fn evaluate(expression: &str, context: &FilterContext) -> Result<cel::Value, String> {
    let program = cel::Program::compile(expression).map_err(|error| error.to_string())?;
    let context = context.cel_context()?;
    program.execute(&context).map_err(|error| error.to_string())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterResult {
    pub matched: bool,
    /// Why the filter could not decide (parse or evaluation failure, a
    /// non-boolean result); always `matched: false`.
    pub error: Option<String>,
}

/// Evaluate a CEL filter over the admission context. Fails closed.
pub fn evaluate_filter(filter: &str, context: &FilterContext) -> FilterResult {
    match evaluate(filter, context) {
        Ok(cel::Value::Bool(matched)) => FilterResult {
            matched,
            error: None,
        },
        Ok(other) => FilterResult {
            matched: false,
            error: Some(format!(
                "filter evaluated to {} rather than bool",
                other.type_of()
            )),
        },
        Err(error) => FilterResult {
            matched: false,
            error: Some(error),
        },
    }
}

/// Route presets that know a key and a label from the payload's shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoutePreset {
    Github,
    Chat,
}

impl From<WebhookPreset> for RoutePreset {
    fn from(preset: WebhookPreset) -> Self {
        match preset {
            WebhookPreset::Github => Self::Github,
        }
    }
}

/// The routing target of an event; `None` is the bot's main session.
///
/// `perKey` takes the CEL key when it yields a non-empty string or a number,
/// else the preset's key, else [`DEFAULT_ROUTE_KEY`]. The returned close policy is
/// [`RoutedSessionClosePolicy::Inherit`]; the caller applies the trigger's
/// `sessionCloseAfterMs`.
pub fn compute_route_session(
    bot_id: &BotId,
    route: &BotTriggerRoute,
    preset: Option<RoutePreset>,
    event_id: &str,
    context: &FilterContext,
) -> Option<RoutedSession> {
    // A chat conversation always gets its own session: its reply tools are
    // bound to the conversation, which the main session cannot carry.
    let chat_default = BotTriggerRoute::PerKey { key: None };
    let route = match (preset, route) {
        (Some(RoutePreset::Chat), BotTriggerRoute::Bot) => &chat_default,
        _ => route,
    };
    match route {
        BotTriggerRoute::Bot => None,
        BotTriggerRoute::PerEvent => Some(RoutedSession {
            session_id: bot_per_event_session_id(bot_id, event_id),
            label: format!("event {}", truncate_chars(event_id, PER_EVENT_LABEL_CHARS)),
            session_key: None,
            close_policy: RoutedSessionClosePolicy::Inherit,
        }),
        BotTriggerRoute::PerKey { key } => {
            let key = key
                .as_deref()
                .and_then(|expression| expression_route_key(expression, context))
                .or_else(|| preset_route_key(preset, &context.data))
                .unwrap_or_else(|| DEFAULT_ROUTE_KEY.to_owned());
            let label = preset_route_label(preset, &context.data).unwrap_or_else(|| key.clone());
            Some(RoutedSession {
                session_id: bot_keyed_session_id(bot_id, &key),
                label,
                session_key: Some(key),
                close_policy: RoutedSessionClosePolicy::Inherit,
            })
        }
    }
}

/// A route key from a CEL expression: a non-empty string (capped) or a
/// number; anything else, including an evaluation error, yields `None` so
/// the caller falls back.
fn expression_route_key(expression: &str, context: &FilterContext) -> Option<String> {
    match evaluate(expression, context).ok()? {
        cel::Value::String(value) if !value.is_empty() => {
            Some(truncate_chars(&value, MAX_ROUTE_KEY_CHARS).to_owned())
        }
        cel::Value::Int(value) => Some(value.to_string()),
        cel::Value::UInt(value) => Some(value.to_string()),
        cel::Value::Float(value) => Some(value.to_string()),
        _ => None,
    }
}

/// The preset's key for a payload: GitHub keys a pull request (`pr-N`), an
/// issue (`issue-N`), or the repository; chat keys the conversation.
pub fn preset_route_key(preset: Option<RoutePreset>, data: &Value) -> Option<String> {
    match preset? {
        RoutePreset::Chat => data
            .get("conversation")
            .and_then(|conversation| conversation.get("key"))
            .and_then(Value::as_str)
            .filter(|key| !key.is_empty())
            .map(str::to_owned),
        RoutePreset::Github => {
            if let Some(number) = data
                .get("pull_request")
                .and_then(|pull_request| pull_request.get("number"))
                .and_then(Value::as_number)
            {
                return Some(format!("pr-{}", number_to_string(number)));
            }
            if let Some(number) = data
                .get("issue")
                .and_then(|issue| issue.get("number"))
                .and_then(Value::as_number)
            {
                return Some(format!("issue-{}", number_to_string(number)));
            }
            data.get("repository")
                .and_then(|repository| repository.get("full_name"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        }
    }
}

/// A human label for the routed session where the preset knows one (a
/// chat's name); GitHub sessions are labelled by their key.
pub fn preset_route_label(preset: Option<RoutePreset>, data: &Value) -> Option<String> {
    match preset? {
        RoutePreset::Chat => data
            .get("conversation")
            .and_then(|conversation| conversation.get("label"))
            .and_then(Value::as_str)
            .filter(|label| !label.is_empty())
            .map(|label| truncate_chars(label, MAX_ROUTE_LABEL_CHARS).to_owned()),
        RoutePreset::Github => None,
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> &str {
    match value.char_indices().nth(max_chars) {
        Some((index, _)) => &value[..index],
        None => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::{BotEventSender, BotTriggerRoute};
    use serde_json::json;

    fn bot() -> BotId {
        BotId::new("triage")
    }

    fn context(data: Value) -> FilterContext {
        FilterContext {
            event: FilterEvent {
                id: "evt".to_owned(),
                kind: "issues.opened".to_owned(),
                source: "webhook:gh".to_owned(),
                occurred_at_ms: 1_755_648_000_000,
                sender: None,
            },
            data,
            headers: [("x-github-event".to_owned(), "issues".to_owned())]
                .into_iter()
                .collect(),
        }
    }

    fn default_context() -> FilterContext {
        context(json!({ "issue": { "number": 7 } }))
    }

    fn per_key(key: Option<&str>) -> BotTriggerRoute {
        BotTriggerRoute::PerKey {
            key: key.map(str::to_owned),
        }
    }

    fn is_hex(value: &str) -> bool {
        !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    }

    #[test]
    fn validates_expressions_at_parse_time() {
        assert!(validate_expression("event.kind == \"x\"").is_ok());
        assert!(validate_expression("data.issue.number > 3 && has(data.issue)").is_ok());
        assert!(validate_expression("event.kind == ").is_err());
        assert!(validate_expression("(").is_err());
    }

    #[test]
    fn filter_matches_rejects_and_fails_closed() {
        assert_eq!(
            evaluate_filter("event.kind == \"issues.opened\"", &default_context()),
            FilterResult {
                matched: true,
                error: None
            }
        );
        assert_eq!(
            evaluate_filter("event.kind == \"push\"", &default_context()),
            FilterResult {
                matched: false,
                error: None
            }
        );
        let errored = evaluate_filter("data.missing.deep == 1", &default_context());
        assert!(!errored.matched);
        assert!(errored.error.is_some(), "{errored:?}");

        let unparsable = evaluate_filter("event.kind ==", &default_context());
        assert!(!unparsable.matched);
        assert!(unparsable.error.is_some());
    }

    #[test]
    fn non_boolean_results_fail_closed_with_a_message() {
        let result = evaluate_filter("data.issue.number", &default_context());
        assert!(!result.matched);
        let error = result.error.expect("error");
        assert!(error.contains("bool"), "{error}");
    }

    #[test]
    fn filters_read_data_headers_and_event_fields() {
        let ctx = default_context();
        assert!(evaluate_filter("data.issue.number == 7", &ctx).matched);
        assert!(evaluate_filter("data.issue.number > 3", &ctx).matched);
        assert!(evaluate_filter("headers[\"x-github-event\"] == \"issues\"", &ctx).matched);
        assert!(evaluate_filter("event.source.startsWith(\"webhook:\")", &ctx).matched);
        assert!(evaluate_filter("event.occurredAtMs > 0", &ctx).matched);
        assert!(evaluate_filter("event.id == \"evt\"", &ctx).matched);
    }

    #[test]
    fn sender_is_null_without_a_sending_bot() {
        let ctx = default_context();
        let result = evaluate_filter("event.sender == \"other\"", &ctx);
        assert_eq!(
            result,
            FilterResult {
                matched: false,
                error: None
            }
        );
        assert!(evaluate_filter("event.sender == null", &ctx).matched);

        let mut sent = default_context();
        sent.event.sender = Some("other".to_owned());
        assert!(evaluate_filter("event.sender == \"other\"", &sent).matched);
    }

    #[test]
    fn absent_data_is_an_empty_map() {
        let ctx = context(Value::Null);
        assert!(evaluate_filter("size(data) == 0", &ctx).matched);
        assert!(evaluate_filter("!has(data.issue)", &ctx).matched);
    }

    #[test]
    fn context_from_document_carries_identity_payload_and_sender() {
        let document = BotEventDocument {
            version: BotEventDocument::VERSION,
            kind: "bot.message".to_owned(),
            source: "bot:planner".to_owned(),
            occurred_at_ms: 42,
            summary: "hi".to_owned(),
            data: Some(json!({ "text": "hi" })),
            headers: [("x-a".to_owned(), "1".to_owned())].into_iter().collect(),
            correlation_id: None,
            links: Vec::new(),
            sender: Some(BotEventSender {
                bot: BotId::new("planner"),
            }),
            hops: 1,
            in_reply_to: None,
        };
        let ctx = FilterContext::from_document("e-1", &document);
        assert_eq!(ctx.event.id, "e-1");
        assert_eq!(ctx.event.kind, "bot.message");
        assert_eq!(ctx.event.source, "bot:planner");
        assert_eq!(ctx.event.occurred_at_ms, 42);
        assert_eq!(ctx.event.sender.as_deref(), Some("planner"));
        assert_eq!(ctx.data, json!({ "text": "hi" }));
        assert_eq!(ctx.headers.get("x-a").map(String::as_str), Some("1"));
        assert!(
            evaluate_filter("event.sender == \"planner\" && data.text == \"hi\"", &ctx).matched
        );

        let mut no_data = document;
        no_data.data = None;
        assert!(FilterContext::from_document("e-2", &no_data).data.is_null());
    }

    #[test]
    fn routes_to_the_main_session_by_default() {
        assert_eq!(
            compute_route_session(&bot(), &BotTriggerRoute::Bot, None, "e", &default_context()),
            None
        );
        assert_eq!(
            compute_route_session(
                &bot(),
                &BotTriggerRoute::Bot,
                Some(RoutePreset::Github),
                "e",
                &default_context()
            ),
            None
        );
    }

    #[test]
    fn derives_per_event_sessions() {
        let routed = compute_route_session(
            &bot(),
            &BotTriggerRoute::PerEvent,
            None,
            "delivery-1",
            &default_context(),
        )
        .expect("routed");
        let suffix = routed
            .session_id
            .strip_prefix("bot:v1:triage:e-")
            .expect("per-event prefix");
        assert_eq!(suffix.len(), 12);
        assert!(is_hex(suffix), "{suffix}");
        assert_eq!(routed.label, "event delivery-1");
        assert_eq!(routed.session_key, None);
        assert_eq!(routed.close_policy, RoutedSessionClosePolicy::Inherit);
        assert_eq!(
            routed,
            compute_route_session(
                &bot(),
                &BotTriggerRoute::PerEvent,
                None,
                "delivery-1",
                &default_context()
            )
            .unwrap()
        );
    }

    #[test]
    fn per_event_labels_are_capped() {
        let long = "x".repeat(60);
        let routed = compute_route_session(
            &bot(),
            &BotTriggerRoute::PerEvent,
            None,
            &long,
            &default_context(),
        )
        .unwrap();
        assert_eq!(routed.label, format!("event {}", "x".repeat(24)));
    }

    #[test]
    fn derives_per_key_sessions_from_a_cel_key_with_digest_suffix() {
        let routed = compute_route_session(
            &bot(),
            &per_key(Some("data.issue.number")),
            None,
            "e",
            &default_context(),
        )
        .expect("routed");
        assert_eq!(routed.label, "7");
        assert_eq!(routed.session_key.as_deref(), Some("7"));
        let suffix = routed
            .session_id
            .strip_prefix("bot:v1:triage:k-7-")
            .expect("keyed prefix");
        assert_eq!(suffix.len(), 8);
        assert!(is_hex(suffix), "{suffix}");
        assert_eq!(routed.session_id, bot_keyed_session_id(&bot(), "7"));
        assert_eq!(routed.close_policy, RoutedSessionClosePolicy::Inherit);
    }

    #[test]
    fn string_keys_are_used_verbatim_and_capped() {
        let ctx = context(json!({ "repo": "acme/widgets", "long": "k".repeat(400) }));
        let routed =
            compute_route_session(&bot(), &per_key(Some("data.repo")), None, "e", &ctx).unwrap();
        assert_eq!(routed.label, "acme/widgets");
        assert_eq!(routed.session_key.as_deref(), Some("acme/widgets"));
        assert_eq!(
            routed.session_id,
            bot_keyed_session_id(&bot(), "acme/widgets")
        );

        let capped =
            compute_route_session(&bot(), &per_key(Some("data.long")), None, "e", &ctx).unwrap();
        assert_eq!(capped.label.chars().count(), MAX_ROUTE_KEY_CHARS);
    }

    #[test]
    fn uses_github_preset_keys_when_no_expression_is_set() {
        let pr = compute_route_session(
            &bot(),
            &per_key(None),
            Some(RoutePreset::Github),
            "e",
            &context(json!({ "pull_request": { "number": 12 } })),
        )
        .unwrap();
        assert_eq!(pr.label, "pr-12");
        let issue = compute_route_session(
            &bot(),
            &per_key(None),
            Some(RoutePreset::Github),
            "e",
            &context(json!({ "issue": { "number": 3 } })),
        )
        .unwrap();
        assert_eq!(issue.label, "issue-3");
        let repo = compute_route_session(
            &bot(),
            &per_key(None),
            Some(RoutePreset::Github),
            "e",
            &context(json!({ "repository": { "full_name": "acme/widgets" } })),
        )
        .unwrap();
        assert_eq!(repo.label, "acme/widgets");
        assert_eq!(
            repo.session_id,
            bot_keyed_session_id(&bot(), "acme/widgets")
        );

        // A pull request outranks the repository, a non-numeric number is
        // skipped.
        let both = context(json!({
            "pull_request": { "number": "x" },
            "issue": { "number": 4 },
            "repository": { "full_name": "acme/widgets" }
        }));
        assert_eq!(
            preset_route_key(Some(RoutePreset::Github), &both.data).as_deref(),
            Some("issue-4")
        );
    }

    #[test]
    fn falls_back_to_the_shared_default_key_on_evaluation_errors() {
        let routed = compute_route_session(
            &bot(),
            &per_key(Some("data.missing.deep")),
            None,
            "e",
            &context(json!({})),
        )
        .expect("a broken key never drops the event");
        assert_eq!(routed.label, DEFAULT_ROUTE_KEY);
        assert_eq!(
            routed.session_id,
            bot_keyed_session_id(&bot(), DEFAULT_ROUTE_KEY)
        );

        // Non-string, non-number results and empty strings fall back too.
        for expression in ["data.flag", "data.empty", "data.list", "this is not cel"] {
            let ctx = context(json!({ "flag": true, "empty": "", "list": [1] }));
            let routed =
                compute_route_session(&bot(), &per_key(Some(expression)), None, "e", &ctx).unwrap();
            assert_eq!(routed.label, DEFAULT_ROUTE_KEY, "{expression}");
        }
    }

    #[test]
    fn a_broken_expression_still_prefers_the_preset_key() {
        let routed = compute_route_session(
            &bot(),
            &per_key(Some("data.missing.deep")),
            Some(RoutePreset::Github),
            "e",
            &context(json!({ "issue": { "number": 9 } })),
        )
        .unwrap();
        assert_eq!(routed.label, "issue-9");
    }

    #[test]
    fn chat_preset_keys_and_labels_the_conversation() {
        let ctx = context(json!({
            "conversation": { "key": "telegram:12345", "label": "Ops room" },
            "text": "hello"
        }));
        let routed =
            compute_route_session(&bot(), &per_key(None), Some(RoutePreset::Chat), "e", &ctx)
                .unwrap();
        assert_eq!(routed.label, "Ops room");
        assert_eq!(routed.session_key.as_deref(), Some("telegram:12345"));
        let stored = serde_json::to_value(&routed).unwrap();
        assert_eq!(stored["sessionKey"], "telegram:12345");
        assert_eq!(
            serde_json::from_value::<RoutedSession>(stored.clone()).unwrap(),
            routed
        );
        let mut unkeyed = stored;
        unkeyed.as_object_mut().unwrap().remove("sessionKey");
        let unkeyed: RoutedSession = serde_json::from_value(unkeyed).unwrap();
        assert_eq!(unkeyed.session_key, None);
        assert_eq!(unkeyed.label, "Ops room");
        assert_eq!(
            routed.session_id,
            bot_keyed_session_id(&bot(), "telegram:12345")
        );

        // The main session can never take a chat: `bot` is forced per key.
        let forced = compute_route_session(
            &bot(),
            &BotTriggerRoute::Bot,
            Some(RoutePreset::Chat),
            "e",
            &ctx,
        )
        .expect("chat always routes");
        assert_eq!(forced, routed);

        // Without a label the key labels the session.
        let unlabelled = context(json!({ "conversation": { "key": "wa:1" } }));
        let routed = compute_route_session(
            &bot(),
            &per_key(None),
            Some(RoutePreset::Chat),
            "e",
            &unlabelled,
        )
        .unwrap();
        assert_eq!(routed.label, "wa:1");
        assert_eq!(
            preset_route_label(Some(RoutePreset::Chat), &unlabelled.data),
            None
        );
    }

    #[test]
    fn preset_labels_are_capped() {
        let data = json!({ "conversation": { "key": "k", "label": "é".repeat(300) } });
        let label = preset_route_label(Some(RoutePreset::Chat), &data).unwrap();
        assert_eq!(label.chars().count(), MAX_ROUTE_LABEL_CHARS);
        assert_eq!(preset_route_label(Some(RoutePreset::Github), &data), None);
        assert_eq!(preset_route_label(None, &data), None);
    }
}
