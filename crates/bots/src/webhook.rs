//! Webhook ingest: verification of the URL token and the trigger's
//! signature scheme against the raw body, header sanitization before
//! anything is persisted, and extraction of event identity and description
//! (the GitHub preset knows the provider's envelope; the generic path
//! dedupes by body digest and names events from a `kind` field).

use std::collections::BTreeMap;

use api::{WebhookPreset, WebhookVerification};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Map, Value};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use thiserror::Error;

use crate::ids::webhook_body_event_id;

/// Headers that must never be persisted into an event document.
pub const REDACTED_HEADERS: [&str; 4] = [
    "authorization",
    "cookie",
    "set-cookie",
    "proxy-authorization",
];
/// Longest header value kept (bytes, cut on a character boundary).
pub const HEADER_VALUE_CAP: usize = 500;
/// Most headers kept per delivery.
pub const HEADER_COUNT_CAP: usize = 40;
/// Longest event kind taken from a generic body's `kind` field (characters).
pub const MAX_WEBHOOK_KIND_CHARS: usize = 200;

/// Why a delivery was refused at the door. Every variant is an HTTP-level
/// refusal; none of them is stored as an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum WebhookRefusal {
    /// The URL token does not match the trigger's.
    #[error("unknown endpoint")]
    UnknownEndpoint,
    /// The signature header is absent or empty.
    #[error("missing signature header")]
    MissingSignature,
    /// The signature's prefix or digest does not match the body.
    #[error("signature mismatch")]
    InvalidSignature,
    /// The trigger wants a signature but no signing secret was leased.
    #[error("signing credential unavailable")]
    MissingSecret,
}

/// Constant-time byte comparison; unequal lengths compare unequal at once.
pub fn constant_time_eq(a: impl AsRef<[u8]>, b: impl AsRef<[u8]>) -> bool {
    a.as_ref().ct_eq(b.as_ref()).into()
}

/// Verify the URL token plus the trigger's signature scheme against the raw
/// body. The token is checked first, in constant time; `hmac-sha256`
/// compares the hex digest of the body under `signing_secret` with the
/// header's value after its prefix.
pub fn verify_webhook(
    verification: &WebhookVerification,
    url_token_expected: &str,
    url_token_given: &str,
    raw_body: &[u8],
    headers: &BTreeMap<String, String>,
    signing_secret: Option<&str>,
) -> Result<(), WebhookRefusal> {
    if !constant_time_eq(url_token_expected, url_token_given) {
        return Err(WebhookRefusal::UnknownEndpoint);
    }
    match verification {
        WebhookVerification::Token => Ok(()),
        WebhookVerification::HmacSha256 { header, prefix, .. } => {
            let secret = signing_secret.ok_or(WebhookRefusal::MissingSecret)?;
            let provided = header_value(headers, header)
                .filter(|value| !value.is_empty())
                .ok_or(WebhookRefusal::MissingSignature)?;
            let signature = provided
                .strip_prefix(prefix.as_deref().unwrap_or(""))
                .ok_or(WebhookRefusal::InvalidSignature)?;
            let expected = hmac_sha256_hex(secret, raw_body);
            if constant_time_eq(signature.to_ascii_lowercase(), expected) {
                Ok(())
            } else {
                Err(WebhookRefusal::InvalidSignature)
            }
        }
    }
}

/// Hex HMAC-SHA256 of `body` under `secret`.
pub fn hmac_sha256_hex(secret: &str, body: &[u8]) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

/// Normalize incoming headers: lowercase names, drop credentials, cap the
/// count and each value's size.
pub fn sanitize_headers<I, K, V>(raw: I) -> BTreeMap<String, String>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let mut result = BTreeMap::new();
    for (name, value) in raw {
        let lower = name.as_ref().to_ascii_lowercase();
        if REDACTED_HEADERS.contains(&lower.as_str()) {
            continue;
        }
        if result.len() >= HEADER_COUNT_CAP && !result.contains_key(&lower) {
            break;
        }
        result.insert(
            lower,
            truncate_bytes(value.as_ref(), HEADER_VALUE_CAP).to_owned(),
        );
    }
    result
}

/// What a verified delivery becomes: dedupe identity, a kind, a summary,
/// the parsed body, and (for presets) a narrower model-facing projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractedWebhookEvent {
    pub event_id: String,
    pub kind: String,
    pub summary: String,
    /// The full parsed body (`Null` for an empty body); the stored document
    /// always keeps it.
    pub data: Value,
    /// Salient projection of `data` for the model-facing rendering; `None`
    /// renders the full `data`.
    pub prompt_data: Option<Value>,
}

/// Turn a verified delivery into event identity and description. `headers`
/// should already be sanitized (lookups are case-insensitive regardless).
/// A non-empty body that is not JSON is refused with the parser's message.
pub fn extract_webhook_event(
    preset: Option<WebhookPreset>,
    raw_body: &[u8],
    headers: &BTreeMap<String, String>,
) -> Result<ExtractedWebhookEvent, String> {
    let data = parse_body(raw_body)?;
    match preset {
        Some(WebhookPreset::Github) => Ok(extract_github_event(data, raw_body, headers)),
        None => {
            let kind = data
                .get("kind")
                .and_then(Value::as_str)
                .filter(|kind| !kind.is_empty())
                .map_or_else(
                    || "webhook".to_owned(),
                    |kind| truncate_chars(kind, MAX_WEBHOOK_KIND_CHARS).to_owned(),
                );
            Ok(ExtractedWebhookEvent {
                event_id: webhook_body_event_id(raw_body),
                summary: format!("Webhook {kind} received"),
                kind,
                data,
                prompt_data: None,
            })
        }
    }
}

fn parse_body(raw_body: &[u8]) -> Result<Value, String> {
    if raw_body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Null);
    }
    serde_json::from_slice(raw_body).map_err(|error| format!("webhook body is not JSON: {error}"))
}

/// The GitHub envelope grammar is shared across event types: `action`,
/// `repository`, `sender`, and one or two subject objects named after the
/// event. Picking the subjects covers every kind without per-event
/// extractors; payloads without that shape (`push`) render in full.
fn extract_github_event(
    data: Value,
    raw_body: &[u8],
    headers: &BTreeMap<String, String>,
) -> ExtractedWebhookEvent {
    let gh_event = header_value(headers, "x-github-event")
        .filter(|event| !event.is_empty())
        .unwrap_or("unknown");
    let action = data.get("action").and_then(Value::as_str);
    let kind = match action {
        Some(action) => format!("{gh_event}.{action}"),
        None => gh_event.to_owned(),
    };
    let repo_name = data
        .get("repository")
        .and_then(|repository| repository.get("full_name"))
        .and_then(Value::as_str);

    let subjects: Vec<(&str, &Value)> = github_subject_keys(gh_event)
        .into_iter()
        .filter_map(|key| {
            data.get(key)
                .filter(|value| value.is_object())
                .map(|value| (key, value))
        })
        .collect();
    let prompt_data = if subjects.is_empty() {
        None
    } else {
        let mut projection = Map::new();
        if let Some(action) = action {
            projection.insert("action".to_owned(), Value::String(action.to_owned()));
        }
        if let Some(repo_name) = repo_name {
            projection.insert("repository".to_owned(), Value::String(repo_name.to_owned()));
        }
        if let Some(sender) = data.get("sender") {
            projection.insert("sender".to_owned(), sender.clone());
        }
        for (key, subject) in subjects {
            projection.insert(key.to_owned(), subject.clone());
        }
        Some(Value::Object(projection))
    };

    let summary = match repo_name {
        Some(repo_name) => format!("GitHub {kind} in {repo_name}"),
        None => format!("GitHub {kind}"),
    };
    ExtractedWebhookEvent {
        event_id: header_value(headers, "x-github-delivery")
            .filter(|id| !id.is_empty())
            .map_or_else(|| webhook_body_event_id(raw_body), str::to_owned),
        kind,
        summary,
        data,
        prompt_data,
    }
}

/// The subject object(s) of a GitHub event. Most events carry one object
/// named after the event (`pull_request`, `release`, `workflow_run`,
/// `check_run`, `discussion`, ...); the ones that do not are listed, and
/// the event's own name is always tried as well.
fn github_subject_keys(event: &str) -> Vec<&str> {
    let mut keys: Vec<&str> = match event {
        "issues" => vec!["issue"],
        "issue_comment" => vec!["issue", "comment"],
        "pull_request_review" => vec!["pull_request", "review"],
        "pull_request_review_comment" => vec!["pull_request", "comment"],
        "pull_request_review_thread" => vec!["pull_request", "thread"],
        "commit_comment" => vec!["comment"],
        "discussion_comment" => vec!["discussion", "comment"],
        "deployment_status" => vec!["deployment", "deployment_status"],
        "fork" => vec!["forkee"],
        _ => Vec::new(),
    };
    if !keys.contains(&event) {
        keys.push(event);
    }
    keys
}

/// Case-insensitive header lookup (exact lowercase key first).
fn header_value<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    let lower = name.to_ascii_lowercase();
    headers
        .get(&lower)
        .or_else(|| {
            headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(&lower))
                .map(|(_, value)| value)
        })
        .map(String::as_str)
}

fn truncate_bytes(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
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
    use serde_json::json;

    fn headers(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn hmac_verification(prefix: Option<&str>) -> WebhookVerification {
        WebhookVerification::HmacSha256 {
            grant_id: "grant-1".to_owned(),
            header: "X-Hub-Signature-256".to_owned(),
            prefix: prefix.map(str::to_owned),
            audience: None,
        }
    }

    #[test]
    fn accepts_the_url_token_and_rejects_mismatches() {
        let body = b"{}";
        assert_eq!(
            verify_webhook(
                &WebhookVerification::Token,
                "tok-1",
                "tok-1",
                body,
                &headers(&[]),
                None
            ),
            Ok(())
        );
        assert_eq!(
            verify_webhook(
                &WebhookVerification::Token,
                "tok-1",
                "tok-2",
                body,
                &headers(&[]),
                None
            ),
            Err(WebhookRefusal::UnknownEndpoint)
        );
        assert_eq!(
            verify_webhook(
                &WebhookVerification::Token,
                "tok-1",
                "tok-1-longer",
                body,
                &headers(&[]),
                None
            ),
            Err(WebhookRefusal::UnknownEndpoint)
        );
        // The token is checked before anything the scheme wants.
        assert_eq!(
            verify_webhook(
                &hmac_verification(None),
                "tok-1",
                "nope",
                body,
                &headers(&[]),
                None
            ),
            Err(WebhookRefusal::UnknownEndpoint)
        );
    }

    #[test]
    fn verifies_hmac_sha256_signatures_with_prefix() {
        let body = serde_json::to_vec(&json!({ "action": "opened" })).unwrap();
        let signature = format!("sha256={}", hmac_sha256_hex("s3cret-key", &body));
        let verification = hmac_verification(Some("sha256="));
        let signed = headers(&[("x-hub-signature-256", signature.as_str())]);

        assert_eq!(
            verify_webhook(
                &verification,
                "tok-1",
                "tok-1",
                &body,
                &signed,
                Some("s3cret-key")
            ),
            Ok(())
        );
        assert_eq!(
            verify_webhook(
                &verification,
                "tok-1",
                "tok-1",
                &body,
                &signed,
                Some("wrong-secret")
            ),
            Err(WebhookRefusal::InvalidSignature)
        );
        assert_eq!(
            verify_webhook(
                &verification,
                "tok-1",
                "tok-1",
                b"{}",
                &signed,
                Some("s3cret-key")
            ),
            Err(WebhookRefusal::InvalidSignature)
        );
        assert_eq!(
            verify_webhook(
                &verification,
                "tok-1",
                "tok-1",
                &body,
                &headers(&[]),
                Some("s3cret-key")
            ),
            Err(WebhookRefusal::MissingSignature)
        );
        assert_eq!(
            verify_webhook(
                &verification,
                "tok-1",
                "tok-1",
                &body,
                &headers(&[("x-hub-signature-256", "")]),
                Some("s3cret-key")
            ),
            Err(WebhookRefusal::MissingSignature)
        );
        assert_eq!(
            verify_webhook(
                &verification,
                "tok-1",
                "tok-1",
                &body,
                &headers(&[("x-hub-signature-256", "md5=nope")]),
                Some("s3cret-key")
            ),
            Err(WebhookRefusal::InvalidSignature)
        );
        assert_eq!(
            verify_webhook(&verification, "tok-1", "tok-1", &body, &signed, None),
            Err(WebhookRefusal::MissingSecret)
        );
    }

    #[test]
    fn signature_lookup_is_case_insensitive_and_hex_case_tolerant() {
        let body = b"payload";
        let upper = hmac_sha256_hex("k", body).to_ascii_uppercase();
        let given = headers(&[("X-Hub-Signature-256", upper.as_str())]);
        assert_eq!(
            verify_webhook(&hmac_verification(None), "t", "t", body, &given, Some("k")),
            Ok(())
        );
    }

    #[test]
    fn constant_time_eq_compares_bytes() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn github_preset_extracts_identity_naming_summary_and_projection() {
        let payload = json!({
            "action": "opened",
            "repository": { "full_name": "acme/widgets", "html_url": "https://github.com/acme/widgets" },
            "sender": { "login": "lukas", "avatar_url": "https://example.com/a.png", "id": 7 },
            "issue": { "number": 5, "title": "Broken build" }
        });
        let body = serde_json::to_vec(&payload).unwrap();
        let extraction = extract_webhook_event(
            Some(WebhookPreset::Github),
            &body,
            &headers(&[("x-github-event", "issues"), ("x-github-delivery", "d-123")]),
        )
        .unwrap();
        assert_eq!(extraction.event_id, "d-123");
        assert_eq!(extraction.kind, "issues.opened");
        assert_eq!(extraction.summary, "GitHub issues.opened in acme/widgets");
        // The stored document keeps the full body; only the prompt is
        // projected to the subject object plus envelope identity.
        assert_eq!(extraction.data, payload);
        assert_eq!(
            extraction.prompt_data,
            Some(json!({
                "action": "opened",
                "repository": "acme/widgets",
                "sender": { "login": "lukas", "avatar_url": "https://example.com/a.png", "id": 7 },
                "issue": { "number": 5, "title": "Broken build" }
            }))
        );
    }

    #[test]
    fn github_projection_picks_every_subject_of_the_event() {
        let payload = json!({
            "action": "created",
            "repository": { "full_name": "acme/widgets" },
            "issue": { "number": 5 },
            "comment": { "id": 9, "body": "LGTM" },
            "installation": { "id": 1 }
        });
        let body = serde_json::to_vec(&payload).unwrap();
        let extraction = extract_webhook_event(
            Some(WebhookPreset::Github),
            &body,
            &headers(&[("x-github-event", "issue_comment")]),
        )
        .unwrap();
        assert_eq!(extraction.kind, "issue_comment.created");
        assert_eq!(
            extraction.prompt_data,
            Some(json!({
                "action": "created",
                "repository": "acme/widgets",
                "issue": { "number": 5 },
                "comment": { "id": 9, "body": "LGTM" }
            }))
        );
        // The event's own name is always a subject candidate.
        let payload = json!({ "action": "completed", "workflow_run": { "id": 3 }, "other": 1 });
        let body = serde_json::to_vec(&payload).unwrap();
        let extraction = extract_webhook_event(
            Some(WebhookPreset::Github),
            &body,
            &headers(&[("x-github-event", "workflow_run")]),
        )
        .unwrap();
        assert_eq!(
            extraction.prompt_data,
            Some(json!({ "action": "completed", "workflow_run": { "id": 3 } }))
        );
    }

    #[test]
    fn github_falls_back_to_the_full_body_without_a_subject_object() {
        let payload = json!({ "ref": "refs/heads/main", "commits": [{ "message": "fix" }] });
        let body = serde_json::to_vec(&payload).unwrap();
        let extraction = extract_webhook_event(
            Some(WebhookPreset::Github),
            &body,
            &headers(&[("X-GitHub-Event", "push"), ("X-GitHub-Delivery", "d-9")]),
        )
        .unwrap();
        assert_eq!(extraction.event_id, "d-9");
        assert_eq!(extraction.kind, "push");
        assert_eq!(extraction.summary, "GitHub push");
        assert_eq!(extraction.data, payload);
        assert_eq!(extraction.prompt_data, None);
    }

    #[test]
    fn github_without_headers_uses_the_body_digest_and_unknown_kind() {
        let body = br#"{"action":"x"}"#;
        let extraction =
            extract_webhook_event(Some(WebhookPreset::Github), body, &headers(&[])).unwrap();
        assert_eq!(extraction.event_id, webhook_body_event_id(body));
        assert_eq!(extraction.kind, "unknown.x");
    }

    #[test]
    fn generic_path_uses_a_body_digest_and_the_kind_field() {
        let body = serde_json::to_vec(&json!({ "kind": "deploy.finished", "ok": true })).unwrap();
        let extraction = extract_webhook_event(None, &body, &headers(&[])).unwrap();
        let digest = extraction
            .event_id
            .strip_prefix("whk-")
            .expect("whk- prefix");
        assert_eq!(digest.len(), 64);
        assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(extraction.kind, "deploy.finished");
        assert_eq!(extraction.summary, "Webhook deploy.finished received");
        assert_eq!(extraction.prompt_data, None);
        // Identical retried payloads converge on the same dedupe identity.
        assert_eq!(
            extract_webhook_event(None, &body, &headers(&[]))
                .unwrap()
                .event_id,
            extraction.event_id
        );
        assert_ne!(
            extract_webhook_event(None, br#"{"kind":"other"}"#, &headers(&[]))
                .unwrap()
                .event_id,
            extraction.event_id
        );

        let unnamed = extract_webhook_event(None, br#"{"ok":true}"#, &headers(&[])).unwrap();
        assert_eq!(unnamed.kind, "webhook");
        let long_kind = json!({ "kind": "k".repeat(300) });
        let capped = extract_webhook_event(
            None,
            &serde_json::to_vec(&long_kind).unwrap(),
            &headers(&[]),
        )
        .unwrap();
        assert_eq!(capped.kind.chars().count(), MAX_WEBHOOK_KIND_CHARS);
    }

    #[test]
    fn empty_bodies_carry_null_data_and_non_json_bodies_are_refused() {
        let empty = extract_webhook_event(None, b"  \n", &headers(&[])).unwrap();
        assert_eq!(empty.data, Value::Null);
        assert_eq!(empty.kind, "webhook");

        let error = extract_webhook_event(None, b"payload=%7B%7D", &headers(&[])).unwrap_err();
        assert!(error.contains("not JSON"), "{error}");
        assert!(
            extract_webhook_event(Some(WebhookPreset::Github), b"<html>", &headers(&[])).is_err()
        );
    }

    #[test]
    fn redacts_credential_headers_and_caps_values() {
        let long = "a".repeat(1_000);
        let sanitized = sanitize_headers([
            ("Authorization", "Bearer secret"),
            ("Cookie", "session=1"),
            ("Set-Cookie", "a=b"),
            ("Proxy-Authorization", "x"),
            ("X-Long", long.as_str()),
            ("X-Ok", "fine"),
        ]);
        assert!(!sanitized.contains_key("authorization"));
        assert!(!sanitized.contains_key("cookie"));
        assert!(!sanitized.contains_key("set-cookie"));
        assert!(!sanitized.contains_key("proxy-authorization"));
        assert_eq!(sanitized["x-long"].len(), HEADER_VALUE_CAP);
        assert_eq!(sanitized["x-ok"], "fine");
        assert_eq!(sanitized.len(), 2);
    }

    #[test]
    fn header_caps_respect_count_and_character_boundaries() {
        let many: Vec<(String, String)> = (0..60)
            .map(|index| (format!("x-h-{index:02}"), index.to_string()))
            .collect();
        let sanitized = sanitize_headers(many.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        assert_eq!(sanitized.len(), HEADER_COUNT_CAP);

        let multibyte = "é".repeat(400);
        let sanitized = sanitize_headers([("X-U", multibyte.as_str())]);
        let kept = &sanitized["x-u"];
        assert!(kept.len() <= HEADER_VALUE_CAP);
        assert_eq!(kept.chars().count(), HEADER_VALUE_CAP / 2);

        // A BTreeMap of owned strings sanitizes as-is.
        let owned = headers(&[("X-A", "1")]);
        assert_eq!(sanitize_headers(&owned)["x-a"], "1");
    }
}
