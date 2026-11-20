//! Validation of bot and trigger documents at the write boundary. What is
//! refused here never reaches a row: a filter that cannot parse would
//! otherwise surface only as silently refused events.

use api::{
    BotBreaker, BotCoalescePolicy, BotDocument, BotTriggerDocument, BotTriggerRoute,
    BotTriggerSpec, PollCursorSpec, PollSource, WebhookVerification,
};
use chrono::Utc;

use crate::BotError;

pub const MAX_DISPLAY_NAME_LEN: usize = 200;
pub const MAX_DESCRIPTION_LEN: usize = 2_000;
pub const MAX_BRIEF_LEN: usize = 32_000;
pub const MAX_FILTER_LEN: usize = 2_000;
pub const MAX_ROUTE_KEY_LEN: usize = 500;
pub const MAX_CRON_LEN: usize = 200;
pub const MAX_SUMMARY_LEN: usize = 2_000;
pub const MIN_COALESCE_MS: u64 = 100;
pub const MAX_COALESCE_MS: u64 = 604_800_000;
pub const MIN_COALESCE_COUNT: u32 = 2;
pub const MAX_COALESCE_COUNT: u32 = 100;
pub const MIN_POLL_INTERVAL_MS: u64 = 60_000;
pub const MAX_POLL_INTERVAL_MS: u64 = 604_800_000;
pub const MAX_POLL_ARGV: usize = 64;
pub const MIN_POLL_TIMEOUT_MS: u64 = 1_000;
pub const MAX_POLL_TIMEOUT_MS: u64 = 600_000;
pub const MAX_POLL_BODY_LEN: usize = 100_000;
pub const MAX_SESSION_CLOSE_AFTER_MS: u64 = 31_536_000_000;
pub const MIN_BREAKER_WINDOW_MS: u64 = 1_000;
pub const MAX_BREAKER_WINDOW_MS: u64 = 86_400_000;
pub const MAX_BREAKER_FIRES: u32 = 100_000;
pub const MAX_INBOX_FROM: usize = 100;
/// A one-shot `atMs` must lie at least this far in the future.
pub const MIN_ONE_SHOT_LEAD_MS: i64 = 30_000;

/// Header names that carry credentials and must come from a grant instead.
pub const CREDENTIAL_HEADER_NAMES: [&str; 6] = [
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "api-key",
];

fn invalid(message: impl Into<String>) -> BotError {
    BotError::invalid(message)
}

fn nonempty(label: &str, value: &str, max: usize) -> Result<(), BotError> {
    if value.trim().is_empty() {
        return Err(invalid(format!("{label} must not be empty")));
    }
    if value.len() > max {
        return Err(invalid(format!("{label} must be at most {max} bytes")));
    }
    Ok(())
}

fn nonempty_optional(label: &str, value: Option<&str>, max: usize) -> Result<(), BotError> {
    match value {
        Some(value) => nonempty(label, value, max),
        None => Ok(()),
    }
}

pub fn validate_breaker(breaker: &BotBreaker) -> Result<(), BotError> {
    if breaker.fires == 0 || breaker.fires > MAX_BREAKER_FIRES {
        return Err(invalid(format!(
            "breaker.fires must be 1..={MAX_BREAKER_FIRES}"
        )));
    }
    if breaker.window_ms < MIN_BREAKER_WINDOW_MS || breaker.window_ms > MAX_BREAKER_WINDOW_MS {
        return Err(invalid(format!(
            "breaker.windowMs must be {MIN_BREAKER_WINDOW_MS}..={MAX_BREAKER_WINDOW_MS}"
        )));
    }
    Ok(())
}

pub fn validate_bot_document(document: &BotDocument) -> Result<(), BotError> {
    nonempty_optional(
        "displayName",
        document.display_name.as_deref(),
        MAX_DISPLAY_NAME_LEN,
    )?;
    nonempty_optional(
        "description",
        document.description.as_deref(),
        MAX_DESCRIPTION_LEN,
    )?;
    nonempty_optional("brief", document.brief.as_deref(), MAX_BRIEF_LEN)?;
    if document.runs_per_day == Some(0) {
        return Err(invalid(
            "runsPerDay must be at least 1 (omit for unlimited)",
        ));
    }
    if let Some(breaker) = &document.breaker {
        validate_breaker(breaker)?;
    }
    if let Some(close_after_ms) = document.routed_session_close_after_ms
        && !(1_000..=MAX_SESSION_CLOSE_AFTER_MS).contains(&close_after_ms)
    {
        return Err(invalid(format!(
            "routedSessionCloseAfterMs must be 1000..={MAX_SESSION_CLOSE_AFTER_MS}"
        )));
    }
    Ok(())
}

/// Classic 5-field cron or an `@macro`. Temporal Schedules take exactly
/// that; Quartz-style expressions (a seconds field, `?`) are rejected with
/// a message that names the expected shape.
pub fn validate_cron(value: &str) -> Result<(), BotError> {
    let value = value.trim();
    nonempty("cron", value, MAX_CRON_LEN)?;
    if value.starts_with('@') {
        return match value {
            "@yearly" | "@annually" | "@monthly" | "@weekly" | "@daily" | "@midnight"
            | "@hourly" => Ok(()),
            other => Err(invalid(format!(
                "unknown cron macro {other}; expected @hourly, @daily, @weekly, @monthly, or @yearly"
            ))),
        };
    }
    if value.contains('?') || value.split_whitespace().count() != 5 {
        return Err(invalid(
            "expected 5-field cron (minute hour day month weekday) or an @-macro like @daily",
        ));
    }
    // The `cron` crate speaks 6/7-field Quartz; prepend seconds to check the
    // classic fields parse and lie in range.
    let quartz = format!("0 {value}");
    quartz
        .parse::<cron::Schedule>()
        .map(|_| ())
        .map_err(|error| invalid(format!("invalid cron expression: {error}")))
}

pub fn validate_timezone(value: &str) -> Result<(), BotError> {
    nonempty("timezone", value, 64)?;
    value
        .parse::<chrono_tz::Tz>()
        .map(|_| ())
        .map_err(|_| invalid(format!("unknown IANA timezone {value:?}")))
}

fn validate_header_name(label: &str, value: &str) -> Result<(), BotError> {
    nonempty(label, value, 200)?;
    let valid = value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+.^_`|~-".contains(&byte));
    if !valid {
        return Err(invalid(format!("{label} is not a valid HTTP header name")));
    }
    Ok(())
}

pub fn validate_route(route: &BotTriggerRoute) -> Result<(), BotError> {
    if let BotTriggerRoute::PerKey { key: Some(key) } = route {
        nonempty("route.key", key, MAX_ROUTE_KEY_LEN)?;
        crate::filter::validate_expression(key)
            .map_err(|error| invalid(format!("invalid CEL in route.key: {error}")))?;
    }
    Ok(())
}

pub fn validate_coalesce(policy: &BotCoalescePolicy) -> Result<(), BotError> {
    for (label, value) in [
        ("coalesce.debounceMs", policy.debounce_ms),
        ("coalesce.maxWaitMs", policy.max_wait_ms),
    ] {
        if !(MIN_COALESCE_MS..=MAX_COALESCE_MS).contains(&value) {
            return Err(invalid(format!(
                "{label} must be {MIN_COALESCE_MS}..={MAX_COALESCE_MS}"
            )));
        }
    }
    if policy.max_wait_ms < policy.debounce_ms {
        return Err(invalid("coalesce.maxWaitMs must cover debounceMs"));
    }
    if policy.max_count < MIN_COALESCE_COUNT || policy.max_count > MAX_COALESCE_COUNT {
        return Err(invalid(format!(
            "coalesce.maxCount must be {MIN_COALESCE_COUNT}..={MAX_COALESCE_COUNT}"
        )));
    }
    Ok(())
}

fn validate_poll_source(source: &PollSource) -> Result<(), BotError> {
    match source {
        PollSource::Http {
            url,
            headers,
            auth,
            body,
            ..
        } => {
            nonempty("source.url", url, 2_000)?;
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(invalid("source.url must be http(s)"));
            }
            let leased_header = auth
                .as_ref()
                .and_then(|auth| auth.header.as_deref())
                .unwrap_or("authorization")
                .to_ascii_lowercase();
            for (name, value) in headers {
                validate_header_name("source.headers name", name)?;
                if value.len() > 2_000 {
                    return Err(invalid(format!(
                        "source.headers[{name}] must be at most 2000 bytes"
                    )));
                }
                let lower = name.to_ascii_lowercase();
                if CREDENTIAL_HEADER_NAMES.contains(&lower.as_str()) {
                    return Err(invalid(format!(
                        "credential header {name} must use source.auth.grantId"
                    )));
                }
                if auth.is_some() && lower == leased_header {
                    return Err(invalid(format!(
                        "{name} conflicts with the leased credential header"
                    )));
                }
            }
            if let Some(auth) = auth {
                nonempty("source.auth.grantId", &auth.grant_id, 300)?;
                if let Some(header) = &auth.header {
                    validate_header_name("source.auth.header", header)?;
                }
                if let Some(scheme) = &auth.scheme
                    && scheme.len() > 100
                {
                    return Err(invalid("source.auth.scheme must be at most 100 bytes"));
                }
                nonempty_optional("source.auth.audience", auth.audience.as_deref(), 2_000)?;
            }
            if let Some(body) = body
                && body.len() > MAX_POLL_BODY_LEN
            {
                return Err(invalid(format!(
                    "source.body must be at most {MAX_POLL_BODY_LEN} bytes"
                )));
            }
            Ok(())
        }
        PollSource::Exec {
            environment_id,
            argv,
            cwd,
            timeout_ms,
        } => {
            nonempty_optional("source.environmentId", environment_id.as_deref(), 300)?;
            if argv.is_empty() || argv.len() > MAX_POLL_ARGV {
                return Err(invalid(format!(
                    "source.argv must have 1..={MAX_POLL_ARGV} entries"
                )));
            }
            for arg in argv {
                if arg.is_empty() || arg.len() > 10_000 {
                    return Err(invalid("source.argv entries must be 1..=10000 bytes"));
                }
            }
            nonempty_optional("source.cwd", cwd.as_deref(), 2_000)?;
            if let Some(timeout) = timeout_ms
                && (*timeout < MIN_POLL_TIMEOUT_MS || *timeout > MAX_POLL_TIMEOUT_MS)
            {
                return Err(invalid(format!(
                    "source.timeoutMs must be {MIN_POLL_TIMEOUT_MS}..={MAX_POLL_TIMEOUT_MS}"
                )));
            }
            Ok(())
        }
    }
}

/// Validate a trigger document. `now_ms` anchors the one-shot lead check.
pub fn validate_trigger_document(
    document: &BotTriggerDocument,
    now_ms: i64,
) -> Result<(), BotError> {
    match &document.spec {
        BotTriggerSpec::Schedule {
            cron,
            at_ms,
            timezone,
            summary,
        } => {
            match (cron, at_ms) {
                (Some(cron), None) => validate_cron(cron)?,
                (None, Some(at_ms)) => {
                    if *at_ms <= now_ms + MIN_ONE_SHOT_LEAD_MS {
                        return Err(invalid(
                            "a one-shot atMs must lie at least 30 seconds in the future",
                        ));
                    }
                }
                _ => return Err(invalid("set exactly one of cron or atMs")),
            }
            validate_timezone(timezone)?;
            nonempty("summary", summary, MAX_SUMMARY_LEN)?;
            if document.filter.is_some()
                || document.route.is_some()
                || document.coalesce.is_some()
                || document.deliver.is_some()
                || document.session_close_after_ms.is_some()
            {
                return Err(invalid(
                    "schedule triggers deliver to the main session and take no filter, route, coalesce, deliver, or sessionCloseAfterMs",
                ));
            }
        }
        BotTriggerSpec::Webhook { verification, .. } => {
            if let WebhookVerification::HmacSha256 {
                grant_id,
                header,
                prefix,
                audience,
            } = verification
            {
                nonempty("verification.grantId", grant_id, 300)?;
                validate_header_name("verification.header", header)?;
                if let Some(prefix) = prefix
                    && prefix.len() > 20
                {
                    return Err(invalid("verification.prefix must be at most 20 bytes"));
                }
                nonempty_optional("verification.audience", audience.as_deref(), 2_000)?;
            }
        }
        BotTriggerSpec::Poll {
            source,
            interval_ms,
            items,
            cursor,
        } => {
            validate_poll_source(source)?;
            if *interval_ms < MIN_POLL_INTERVAL_MS || *interval_ms > MAX_POLL_INTERVAL_MS {
                return Err(invalid(format!(
                    "intervalMs must be {MIN_POLL_INTERVAL_MS}..={MAX_POLL_INTERVAL_MS}"
                )));
            }
            nonempty_optional("items", items.as_deref(), 500)?;
            match cursor {
                PollCursorSpec::IdSet { id } => nonempty("cursor.id", id, 500)?,
                PollCursorSpec::Watermark { field } => nonempty("cursor.field", field, 500)?,
            }
        }
        BotTriggerSpec::Bot { from } => {
            if let Some(from) = from
                && from.len() > MAX_INBOX_FROM
            {
                return Err(invalid(format!(
                    "from must list at most {MAX_INBOX_FROM} bots"
                )));
            }
        }
    }
    if let Some(filter) = &document.filter {
        nonempty("filter", filter, MAX_FILTER_LEN)?;
        crate::filter::validate_expression(filter)
            .map_err(|error| invalid(format!("invalid CEL in filter: {error}")))?;
    }
    if let Some(route) = &document.route {
        validate_route(route)?;
    }
    if let Some(coalesce) = &document.coalesce {
        validate_coalesce(coalesce)?;
    }
    if let Some(close_after_ms) = document.session_close_after_ms
        && close_after_ms > MAX_SESSION_CLOSE_AFTER_MS
    {
        return Err(invalid(format!(
            "sessionCloseAfterMs must be at most {MAX_SESSION_CLOSE_AFTER_MS}"
        )));
    }
    Ok(())
}

/// Wall-clock milliseconds, for callers that anchor validation at "now".
pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::{PollCursorSpec, PollSource, ProfileId};

    fn schedule(cron: Option<&str>, at_ms: Option<i64>) -> BotTriggerDocument {
        BotTriggerDocument {
            spec: BotTriggerSpec::Schedule {
                cron: cron.map(str::to_owned),
                at_ms,
                timezone: "UTC".to_owned(),
                summary: "check the queue".to_owned(),
            },
            filter: None,
            route: None,
            coalesce: None,
            deliver: None,
            session_close_after_ms: None,
            enabled: true,
        }
    }

    #[test]
    fn cron_accepts_classic_and_rejects_quartz() {
        assert!(validate_cron("*/5 * * * *").is_ok());
        assert!(validate_cron("0 9 * * MON-FRI").is_ok());
        assert!(validate_cron("@daily").is_ok());
        assert!(validate_cron("0 0 12 * * ?").is_err());
        assert!(validate_cron("* * * *").is_err());
        assert!(validate_cron("99 * * * *").is_err());
    }

    #[test]
    fn schedule_needs_exactly_one_of_cron_or_at() {
        assert!(validate_trigger_document(&schedule(Some("@hourly"), None), 0).is_ok());
        assert!(validate_trigger_document(&schedule(None, Some(10_000_000)), 0).is_ok());
        assert!(validate_trigger_document(&schedule(None, None), 0).is_err());
        assert!(validate_trigger_document(&schedule(Some("@hourly"), Some(1)), 0).is_err());
        assert!(validate_trigger_document(&schedule(None, Some(10_000)), 0).is_err());
    }

    #[test]
    fn poll_rejects_credential_headers() {
        let document = BotTriggerDocument {
            spec: BotTriggerSpec::Poll {
                source: PollSource::Http {
                    url: "https://example.com/feed".to_owned(),
                    method: Default::default(),
                    headers: [("Authorization".to_owned(), "x".to_owned())]
                        .into_iter()
                        .collect(),
                    auth: None,
                    body: None,
                },
                interval_ms: 60_000,
                items: None,
                cursor: PollCursorSpec::IdSet {
                    id: "id".to_owned(),
                },
            },
            filter: None,
            route: None,
            coalesce: None,
            deliver: None,
            session_close_after_ms: None,
            enabled: true,
        };
        let error = validate_trigger_document(&document, 0).unwrap_err();
        assert!(error.to_string().contains("grantId"), "{error}");
    }

    #[test]
    fn bot_document_bounds() {
        let mut document = BotDocument {
            display_name: Some("Triage".to_owned()),
            description: None,
            profile_id: ProfileId::new("triage"),
            brief: None,
            runs_per_day: Some(0),
            breaker: None,
            routed_session_close_after_ms: None,
            self_config: false,
            emit: false,
            enabled: true,
        };
        assert!(validate_bot_document(&document).is_err());
        document.runs_per_day = Some(5);
        assert!(validate_bot_document(&document).is_ok());
        document.breaker = Some(BotBreaker {
            fires: 0,
            window_ms: 1_000,
        });
        assert!(validate_bot_document(&document).is_err());
    }
}
