use std::time::{SystemTime, UNIX_EPOCH};

use super::AgentApiError;

pub(super) fn now_ms() -> Result<i64, AgentApiError> {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| AgentApiError::internal(format!("system clock is before epoch: {error}")))?
        .as_millis();
    i64::try_from(ms)
        .map_err(|_| AgentApiError::internal("current timestamp does not fit in i64 milliseconds"))
}
