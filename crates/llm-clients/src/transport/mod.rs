//! Shared transport substrate for provider-native clients.

pub mod headers;
pub mod http;
pub mod sse;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use headers::HeaderSnapshot;
pub use http::{HttpClient, HttpClientConfig};
pub use sse::{SseEvent, SseParser};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApiResponse<T> {
    pub parsed: T,
    pub raw_json: Value,
    pub status: u16,
    pub headers: HeaderSnapshot,
}

impl<T> ApiResponse<T> {
    pub fn new(
        parsed: T,
        raw_json: Value,
        status: reqwest::StatusCode,
        headers: HeaderSnapshot,
    ) -> Self {
        Self {
            parsed,
            raw_json,
            status: status.as_u16(),
            headers,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApiStreamEvent<E> {
    pub parsed: E,
    pub raw_sse: SseEvent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_json: Option<Value>,
}

impl<E> ApiStreamEvent<E> {
    pub fn new(parsed: E, raw_sse: SseEvent, raw_json: Option<Value>) -> Self {
        Self {
            parsed,
            raw_sse,
            raw_json,
        }
    }
}
