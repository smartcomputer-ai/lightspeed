//! Header snapshot and retry metadata helpers.

use reqwest::header::{HeaderMap, HeaderName, RETRY_AFTER};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};

const REDACTED_HEADER_VALUE: &str = "<redacted>";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeaderSnapshot {
    pub entries: Vec<(String, String)>,
}

impl HeaderSnapshot {
    pub fn from_headermap(headers: &HeaderMap) -> Self {
        let mut entries = Vec::new();
        for (name, value) in headers.iter() {
            let name = name.as_str().to_ascii_lowercase();
            let value = if is_sensitive_header(&name) {
                REDACTED_HEADER_VALUE.to_string()
            } else {
                value
                    .to_str()
                    .map(str::to_string)
                    .unwrap_or_else(|_| "<non-utf8>".to_string())
            };
            entries.push((name, value));
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
        Self { entries }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.entries
            .iter()
            .find(|(entry_name, _)| entry_name == &name)
            .map(|(_, value)| value.as_str())
    }

    pub fn retry_after(&self) -> Option<Duration> {
        self.get(RETRY_AFTER.as_str()).and_then(parse_retry_after)
    }
}

pub fn parse_retry_after(value: &str) -> Option<Duration> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    let retry_at = httpdate::parse_http_date(value).ok()?;
    retry_at.duration_since(SystemTime::now()).ok()
}

pub fn header_name(name: &'static str) -> HeaderName {
    HeaderName::from_static(name)
}

fn is_sensitive_header(name: &str) -> bool {
    matches!(
        name,
        "authorization" | "proxy-authorization" | "x-api-key" | "api-key" | "anthropic-api-key"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    #[test]
    fn header_snapshot_sorts_and_lowercases_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("X-RateLimit-Remaining", HeaderValue::from_static("10"));
        headers.insert("Retry-After", HeaderValue::from_static("2"));

        let snapshot = HeaderSnapshot::from_headermap(&headers);

        assert_eq!(snapshot.get("x-ratelimit-remaining"), Some("10"));
        assert_eq!(snapshot.get("RETRY-AFTER"), Some("2"));
        assert_eq!(snapshot.retry_after(), Some(Duration::from_secs(2)));
    }

    #[test]
    fn header_snapshot_redacts_sensitive_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer secret"));
        headers.insert("x-api-key", HeaderValue::from_static("sk-secret"));

        let snapshot = HeaderSnapshot::from_headermap(&headers);

        assert_eq!(snapshot.get("authorization"), Some("<redacted>"));
        assert_eq!(snapshot.get("x-api-key"), Some("<redacted>"));
    }

    #[test]
    fn parse_retry_after_accepts_delta_seconds() {
        assert_eq!(parse_retry_after("42"), Some(Duration::from_secs(42)));
    }

    #[test]
    fn parse_retry_after_rejects_invalid_values() {
        assert_eq!(parse_retry_after("soon"), None);
    }
}
