//! Ephemeral JavaScript execution with an explicit JSON tool boundary.

mod engine;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

/// Cancellation is cooperative at the native engine's interrupt boundary.
#[derive(Clone, Debug, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolBinding {
    pub name: String,
    pub binding_id: String,
}

#[derive(Clone, Debug)]
pub struct ExecutionInput {
    pub source: String,
    pub bindings: Vec<ToolBinding>,
    pub limits: ExecutionLimits,
}

/// Every budget is positive; there are intentionally no deployment defaults.
#[derive(Clone, Debug)]
pub struct ExecutionLimits {
    pub timeout_ms: u64,
    pub max_memory_bytes: u64,
    pub max_stack_bytes: u64,
    pub max_source_bytes: u64,
    pub max_catalog_bytes: u64,
    pub max_request_bytes: u64,
    pub max_result_bytes: u64,
    pub max_output_bytes: u64,
    pub max_tool_calls: u32,
    pub max_outstanding_tool_calls: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HostRequest {
    pub request_id: String,
    pub binding_id: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HostError {
    pub kind: String,
    pub message: String,
    pub value: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HostCompletion {
    pub request_id: String,
    pub outcome: Result<Value, HostError>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionErrorKind {
    Javascript,
    UnsupportedValue,
    LimitExceeded,
    TimedOut,
    Cancelled,
    HostDisconnected,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct ExecutionError {
    pub kind: ExecutionErrorKind,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionMetrics {
    pub startup_micros: u64,
    pub elapsed_micros: u64,
    pub tool_calls: u32,
    pub pending_jobs: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutputSelection {
    Text { index: usize },
    Media { request_id: String },
    File { request_id: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExecutionReport {
    pub output: Vec<Value>,
    /// Ordered receipts for explicit output. Empty on historical text-only
    /// reports; consumers then render `output` in its existing order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selections: Vec<OutputSelection>,
    pub return_value: Option<Value>,
    pub error: Option<ExecutionError>,
    /// Requests issued before script termination, whose completions have not
    /// entered this interpreter. Only the host knows their durable outcomes.
    pub pending_request_ids: Vec<String>,
    pub metrics: ExecutionMetrics,
}

#[derive(Clone, Debug)]
pub enum ExecutionEvent {
    Request(HostRequest),
    Finished(ExecutionReport),
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("invalid execution input: {0}")]
    InvalidInput(String),
    #[error("could not start interpreter thread: {0}")]
    Thread(#[from] std::io::Error),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CompletionError {
    #[error("the interpreter has stopped")]
    Stopped,
    #[error("the completion queue is full")]
    Full,
    #[error("host completion exceeds the result byte limit")]
    TooLarge,
}

#[derive(Clone)]
pub struct CompletionSender {
    sender: std::sync::mpsc::SyncSender<HostCompletion>,
    max_result_bytes: u64,
}

impl CompletionSender {
    /// Enqueue without blocking the async executor. The queue is sized for all
    /// admitted outstanding calls; callers send each completion at most once.
    pub fn complete(&self, completion: HostCompletion) -> Result<(), CompletionError> {
        if serialized_len(&completion) > self.max_result_bytes {
            return Err(CompletionError::TooLarge);
        }
        self.sender
            .try_send(completion)
            .map_err(|error| match error {
                std::sync::mpsc::TrySendError::Full(_) => CompletionError::Full,
                std::sync::mpsc::TrySendError::Disconnected(_) => CompletionError::Stopped,
            })
    }
}

pub struct Execution {
    events: mpsc::Receiver<ExecutionEvent>,
    completions: CompletionSender,
    cancellation: Cancellation,
}

impl Execution {
    pub async fn next_event(&mut self) -> Option<ExecutionEvent> {
        self.events.recv().await
    }
    pub fn completion_sender(&self) -> CompletionSender {
        self.completions.clone()
    }
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }
}

impl Drop for Execution {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Start one fresh, non-replayable JavaScript attempt on its own native thread.
/// No interpreter value or handle crosses this API.
pub fn start(input: ExecutionInput, cancellation: Cancellation) -> Result<Execution, StartError> {
    engine::start(input, cancellation)
}

/// Count serialization without allocating another copy of a potentially large
/// host payload. Values have already crossed the caller's JSON boundary.
fn serialized_len(value: &impl Serialize) -> u64 {
    struct Counter(u64);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len() as u64);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    match serde_json::to_writer(&mut counter, value) {
        Ok(()) => counter.0,
        Err(_) => u64::MAX,
    }
}

#[cfg(test)]
mod tests;
