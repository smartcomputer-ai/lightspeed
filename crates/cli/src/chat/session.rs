use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);
static NEXT_SUBMISSION: AtomicU64 = AtomicU64::new(1);

pub(crate) fn new_session_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let next = NEXT_SESSION.fetch_add(1, Ordering::Relaxed);
    format!("session_{millis}_{next}")
}

/// Fresh `run/start` idempotency key; a retried request reuses the same key.
pub(crate) fn new_submission_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let next = NEXT_SUBMISSION.fetch_add(1, Ordering::Relaxed);
    format!("cli_{millis}_{next}")
}

pub(crate) fn validate_session_id(value: &str) -> Result<String> {
    api::validate_session_id(value).with_context(|| format!("invalid session id '{value}'"))?;
    Ok(value.to_string())
}
