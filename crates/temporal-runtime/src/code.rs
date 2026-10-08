//! One-attempt JavaScript host bridge to session-owned tool execution.
//!
//! The caller supplies an already admitted scope and universe-scoped blob store.
//! This adapter never retries source, restores session state, or schedules tool
//! activities itself. Durable orchestration and recovery belong to its caller.

use std::{collections::BTreeSet, sync::Arc, time::Duration};

pub use codemode::Cancellation;
use codemode::{ExecutionEvent, HostCompletion, HostError, HostRequest};
use futures_util::{StreamExt, stream::FuturesUnordered};
use harness::{BlobRef, storage::BlobStore};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use temporal_workflow::{
    CloseCodeToolScopeRequest, CodeExecutionDescriptor, CodeExecutionLimits, CodeToolCallOutcome,
    CodeToolCallStatus, CodeToolClient, CodeToolScopeReport, CodeToolScopeReportRequest,
    InvokeCodeToolRequest,
};
use temporalio_client::Client;
use tokio::{sync::Semaphore, time::Instant};

/// Minimal execution manifest. Model-facing schemas are rendered separately;
/// the interpreter needs names and pinned handles, not copies of tool schemas.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeToolCatalog {
    pub version: u32,
    pub bindings: Vec<codemode::ToolBinding>,
}

impl CodeToolCatalog {
    pub fn from_scope(scope: &CodeToolScopeReport) -> Self {
        Self {
            version: 1,
            bindings: scope
                .bindings
                .iter()
                .map(|(binding_id, name)| codemode::ToolBinding {
                    name: name.as_str().to_owned(),
                    binding_id: binding_id.clone(),
                })
                .collect(),
        }
    }

    fn validate(&self, scope: &CodeToolScopeReport) -> Result<(), CodeRunError> {
        if self.version != 1 {
            return Err(CodeRunError::Catalog("unsupported catalog version".into()));
        }
        let mut names = BTreeSet::new();
        let mut handles = BTreeSet::new();
        for binding in &self.bindings {
            if binding.name.is_empty()
                || !names.insert(&binding.name)
                || !handles.insert(&binding.binding_id)
                || scope
                    .bindings
                    .get(&binding.binding_id)
                    .is_none_or(|name| name.as_str() != binding.name)
            {
                return Err(CodeRunError::Catalog(
                    "catalog must contain unique names and handles matched to the admitted scope"
                        .into(),
                ));
            }
        }
        Ok(())
    }
}

/// Script output and the session's authoritative per-call outcomes are separate:
/// an effect may have succeeded even if its output could not enter JavaScript.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CodeRunReport {
    pub execution: codemode::ExecutionReport,
    /// Latest confirmed snapshot. It may be incomplete when cleanup failed.
    pub scope: Option<CodeToolScopeReport>,
    /// A missing final receipt means unfinished effect outcomes are unknown.
    pub cleanup_error: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum CodeRunError {
    #[error("invalid code runner configuration: {0}")]
    Configuration(String),
    #[error(transparent)]
    Descriptor(#[from] temporal_workflow::CodeExecutionValidationError),
    #[error("invalid code tool catalog: {0}")]
    Catalog(String),
    #[error("code input or scope unavailable: {0}")]
    Preparation(String),
    #[error("code execution cancelled before interpreter startup")]
    Cancelled,
    #[error("code preparation exceeded its time budget")]
    PreparationTimedOut,
    #[error(transparent)]
    Start(#[from] codemode::StartError),
    #[error("{source}; scope cleanup failed: {cleanup_error}")]
    Cleanup {
        #[source]
        source: Box<CodeRunError>,
        cleanup_error: String,
    },
    #[error("code host stopped unexpectedly; scope outcomes require reconciliation: {0}")]
    HostStopped(String),
}

#[derive(Clone)]
pub struct CodeRunner {
    client: Client,
    blobs: Arc<dyn BlobStore>,
    capacity: Arc<Semaphore>,
    cleanup_timeout: Duration,
}

impl CodeRunner {
    /// Clones share interpreter capacity. The host must reuse this runner rather
    /// than construct one per call if it wants a process-wide concurrency bound.
    pub fn new(
        client: Client,
        blobs: Arc<dyn BlobStore>,
        max_concurrent_executions: usize,
        cleanup_timeout: Duration,
    ) -> Result<Self, CodeRunError> {
        if max_concurrent_executions == 0
            || max_concurrent_executions > Semaphore::MAX_PERMITS
            || cleanup_timeout.is_zero()
            || Instant::now().checked_add(cleanup_timeout).is_none()
        {
            return Err(CodeRunError::Configuration(
                "capacity and cleanup timeout must be positive and representable".into(),
            ));
        }
        Ok(Self {
            client,
            blobs,
            capacity: Arc::new(Semaphore::new(max_concurrent_executions)),
            cleanup_timeout,
        })
    }

    /// Rebind universe-scoped storage while retaining process-wide capacity.
    pub(crate) fn with_shared_capacity(
        client: Client,
        blobs: Arc<dyn BlobStore>,
        capacity: Arc<Semaphore>,
        cleanup_timeout: Duration,
    ) -> Self {
        Self {
            client,
            blobs,
            capacity,
            cleanup_timeout,
        }
    }

    /// Run source once. The caller must schedule at most one attempt for a scope;
    /// the initial empty-scope check is a guard, not a distributed execution lock.
    /// On ordinary failure or explicit cancellation, close admission and collect
    /// terminal outcomes. Dropping the caller cancels a supervised task which
    /// retains its capacity slot through cleanup. Worker loss requires durable
    /// cleanup by the caller. Invalid descriptors and failures to verify scope
    /// ownership leave cleanup to the caller; an existing active scope is never
    /// closed merely because a second attempt tried to reuse it.
    pub async fn run_once(
        &self,
        descriptor: CodeExecutionDescriptor,
        cancellation: Cancellation,
    ) -> Result<CodeRunReport, CodeRunError> {
        struct CancelOnDrop(Cancellation);
        impl Drop for CancelOnDrop {
            fn drop(&mut self) {
                self.0.cancel();
            }
        }
        let _guard = CancelOnDrop(cancellation.clone());
        let runner = self.clone();
        tokio::spawn(async move { runner.run_attempt(descriptor, cancellation).await })
            .await
            .map_err(|error| CodeRunError::HostStopped(error.to_string()))?
    }

    async fn run_attempt(
        &self,
        descriptor: CodeExecutionDescriptor,
        cancellation: Cancellation,
    ) -> Result<CodeRunReport, CodeRunError> {
        descriptor.validate()?;
        let preparation_deadline = Instant::now()
            .checked_add(Duration::from_millis(descriptor.limits.timeout_ms))
            .ok_or_else(|| CodeRunError::Configuration("timeout is too large".into()))?;
        let code_tools =
            CodeToolClient::new(self.client.clone(), descriptor.session_workflow_id.clone());
        // Establish ownership before fallible artifact loading. Once this
        // succeeds, all ordinary exits close this otherwise unused scope.
        let scope = cancellable_preparation(preparation_deadline, &cancellation, async {
            code_tools
                .report(
                    CodeToolScopeReportRequest {
                        execution_id: descriptor.execution_id.clone(),
                    },
                    Default::default(),
                )
                .await
                .map_err(|error| CodeRunError::Preparation(error.to_string()))?
                .map_err(|error| CodeRunError::Preparation(error.to_string()))
        })
        .await?;
        if scope.execution_id != descriptor.execution_id || scope.closed || !scope.calls.is_empty()
        {
            return Err(CodeRunError::Preparation(
                "execution requires its own open, unused scope".into(),
            ));
        }
        let prepare = async {
            let permit = self
                .capacity
                .acquire()
                .await
                .map_err(|error| CodeRunError::Preparation(error.to_string()))?;
            let source = read_bounded(
                self.blobs.as_ref(),
                &descriptor.source_ref,
                descriptor.limits.max_source_bytes,
            )
            .await
            .map_err(|error| CodeRunError::Preparation(error.message))?;
            let source = String::from_utf8(source)
                .map_err(|error| CodeRunError::Preparation(error.to_string()))?;
            let catalog = read_bounded(
                self.blobs.as_ref(),
                &descriptor.catalog_ref,
                descriptor.limits.max_catalog_bytes,
            )
            .await
            .map_err(|error| CodeRunError::Preparation(error.message))?;
            let catalog: CodeToolCatalog = serde_json::from_slice(&catalog)
                .map_err(|error| CodeRunError::Catalog(error.to_string()))?;
            catalog.validate(&scope)?;
            if cancellation.is_cancelled() {
                return Err(CodeRunError::Cancelled);
            }
            Ok((permit, source, catalog))
        };
        let prepared = cancellable_preparation(preparation_deadline, &cancellation, prepare).await;
        let (_permit, source, catalog) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                return Err(self
                    .preparation_failed(&code_tools, &descriptor.execution_id, scope, error)
                    .await);
            }
        };
        // Capacity waits and CAS loading consume the same attempt budget as
        // JavaScript. Otherwise a saturated worker could spend the full timeout
        // waiting and then start a second full timeout inside the interpreter.
        let mut limits = engine_limits(&descriptor.limits);
        limits.timeout_ms = preparation_deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        if limits.timeout_ms == 0 {
            return Err(self
                .preparation_failed(
                    &code_tools,
                    &descriptor.execution_id,
                    scope,
                    CodeRunError::PreparationTimedOut,
                )
                .await);
        }
        let mut execution = match codemode::start(
            codemode::ExecutionInput {
                source,
                bindings: catalog.bindings,
                limits,
            },
            cancellation,
        ) {
            Ok(execution) => execution,
            Err(error) => {
                return Err(self
                    .preparation_failed(&code_tools, &descriptor.execution_id, scope, error.into())
                    .await);
            }
        };
        let completions = execution.completion_sender();
        let mut requests = FuturesUnordered::new();
        let mut bridge_failure = None;
        let report = loop {
            tokio::select! {
                event = execution.next_event() => match event {
                    Some(ExecutionEvent::Request(request)) => {
                        requests.push(self.dispatch(&code_tools, &descriptor, request));
                    }
                    Some(ExecutionEvent::Finished(mut report)) => {
                        if let Some(error) = bridge_failure {
                            report.error = Some(error);
                        }
                        break report;
                    }
                    None => break codemode::ExecutionReport {
                        output: Vec::new(), return_value: None,
                        error: Some(codemode::ExecutionError {
                            kind: codemode::ExecutionErrorKind::Internal,
                            message: "interpreter stopped without a report; consult scope outcomes".into(),
                        }),
                        pending_request_ids: Vec::new(), metrics: Default::default(),
                    },
                },
                Some(completion) = requests.next(), if !requests.is_empty() => {
                    let request_id = completion.request_id.clone();
                    let result = match completions.complete(completion) {
                        Err(codemode::CompletionError::TooLarge) => completions.complete(HostCompletion {
                            request_id,
                            outcome: Err(host_error("payload_too_large", "host completion exceeds result budget")),
                        }),
                        result => result,
                    };
                    if let Err(error) = result
                        && error != codemode::CompletionError::Stopped
                    {
                        bridge_failure = Some(codemode::ExecutionError {
                            kind: if error == codemode::CompletionError::TooLarge {
                                codemode::ExecutionErrorKind::LimitExceeded
                            } else { codemode::ExecutionErrorKind::Internal },
                            message: format!("cannot deliver host completion: {error}"),
                        });
                        execution.cancel();
                    }
                }
            }
        };
        // Dropping RPC waiters is not cancellation of admitted effects. Close
        // the owning scope before reporting what actually completed.
        drop(requests);
        drop(execution);
        let (scope, cleanup_error) = self
            .cleanup(&code_tools, &descriptor.execution_id, Some(scope))
            .await;
        Ok(CodeRunReport {
            execution: report,
            scope,
            cleanup_error,
        })
    }

    async fn preparation_failed(
        &self,
        code_tools: &CodeToolClient,
        execution_id: &str,
        scope: CodeToolScopeReport,
        source: CodeRunError,
    ) -> CodeRunError {
        let (_, cleanup_error) = self.cleanup(code_tools, execution_id, Some(scope)).await;
        match cleanup_error {
            Some(cleanup_error) => CodeRunError::Cleanup {
                source: Box::new(source),
                cleanup_error,
            },
            None => source,
        }
    }

    async fn dispatch(
        &self,
        code_tools: &CodeToolClient,
        descriptor: &CodeExecutionDescriptor,
        request: HostRequest,
    ) -> HostCompletion {
        let outcome = async {
            let bytes = serde_json::to_vec(&request.arguments)
                .map_err(|error| host_error("invalid_arguments", error.to_string()))?;
            if bytes.len() as u64 > descriptor.limits.max_request_bytes {
                return Err(host_error(
                    "payload_too_large",
                    "arguments exceed request budget",
                ));
            }
            let arguments_ref = self
                .blobs
                .put_bytes(bytes)
                .await
                .map_err(|error| host_error("storage", error.to_string()))?;
            let outcome = code_tools
                .invoke(
                    InvokeCodeToolRequest {
                        execution_id: descriptor.execution_id.clone(),
                        request_id: request.request_id.clone(),
                        binding_id: request.binding_id,
                        arguments_ref,
                    },
                    Default::default(),
                )
                .await
                .map_err(|error| host_error("transport", format!("tool outcome unknown: {error}")))?
                .map_err(|error| host_error("admission_rejected", error.to_string()))?;
            materialize(
                self.blobs.as_ref(),
                &outcome,
                descriptor.limits.max_result_bytes,
            )
            .await
        }
        .await;
        HostCompletion {
            request_id: request.request_id,
            outcome,
        }
    }

    pub(crate) async fn cleanup(
        &self,
        code_tools: &CodeToolClient,
        execution_id: &str,
        mut latest: Option<CodeToolScopeReport>,
    ) -> (Option<CodeToolScopeReport>, Option<String>) {
        let cleanup = async {
            latest = Some(
                code_tools
                    .close_scope(
                        CloseCodeToolScopeRequest {
                            execution_id: execution_id.to_owned(),
                            cancel_pending: true,
                        },
                        Default::default(),
                    )
                    .await
                    .map_err(|error| error.to_string())?
                    .map_err(|error| error.to_string())?,
            );
            loop {
                if latest.as_ref().is_some_and(|scope| {
                    scope.closed && scope.calls.values().all(|call| call.status.is_terminal())
                }) {
                    return Ok::<_, String>(());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
                // Update replies are cached; only queries provide fresh progress.
                latest = Some(
                    code_tools
                        .report(
                            CodeToolScopeReportRequest {
                                execution_id: execution_id.to_owned(),
                            },
                            Default::default(),
                        )
                        .await
                        .map_err(|error| error.to_string())?
                        .map_err(|error| error.to_string())?,
                );
            }
        };
        let error = match tokio::time::timeout(self.cleanup_timeout, cleanup).await {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error),
            Err(_) => {
                Some("scope cleanup deadline exceeded; pending outcomes remain unknown".into())
            }
        };
        (latest, error)
    }
}

async fn cancellable_preparation<T>(
    deadline: Instant,
    cancellation: &Cancellation,
    work: impl Future<Output = Result<T, CodeRunError>>,
) -> Result<T, CodeRunError> {
    tokio::select! {
        biased;
        () = wait_for_cancellation(cancellation) => Err(CodeRunError::Cancelled),
        result = tokio::time::timeout_at(deadline, work) => {
            result.unwrap_or(Err(CodeRunError::PreparationTimedOut))
        }
    }
}

async fn wait_for_cancellation(cancellation: &Cancellation) {
    while !cancellation.is_cancelled() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn engine_limits(limits: &CodeExecutionLimits) -> codemode::ExecutionLimits {
    codemode::ExecutionLimits {
        timeout_ms: limits.timeout_ms,
        max_memory_bytes: limits.max_memory_bytes,
        max_stack_bytes: limits.max_stack_bytes,
        max_source_bytes: limits.max_source_bytes,
        max_catalog_bytes: limits.max_catalog_bytes,
        max_request_bytes: limits.max_request_bytes,
        max_result_bytes: limits.max_result_bytes,
        max_output_bytes: limits.max_output_bytes,
        max_tool_calls: limits.max_tool_calls,
        max_outstanding_tool_calls: limits.max_outstanding_tool_calls,
    }
}

fn host_error(kind: &str, message: impl Into<String>) -> HostError {
    HostError {
        kind: kind.into(),
        message: message.into(),
        value: None,
    }
}

/// Check metadata before allocating, then verify exact length and CAS digest.
/// Large persistent blobs are read in bounded ranges rather than buffered by a
/// convenience store API before checking the guest's budget.
pub(crate) async fn read_bounded(
    blobs: &dyn BlobStore,
    reference: &BlobRef,
    limit: u64,
) -> Result<Vec<u8>, HostError> {
    let info = blobs
        .stat_blob(reference)
        .await
        .map_err(|error| host_error("storage", error.to_string()))?;
    if info.byte_len > limit || usize::try_from(info.byte_len).is_err() {
        return Err(host_error(
            "payload_too_large",
            "blob exceeds execution payload budget",
        ));
    }
    let mut bytes = Vec::new();
    while (bytes.len() as u64) < info.byte_len {
        let remaining = (info.byte_len - bytes.len() as u64).min(256 * 1024) as usize;
        let chunk = blobs
            .read_blob_range(reference, bytes.len() as u64, remaining)
            .await
            .map_err(|error| host_error("storage", error.to_string()))?;
        if chunk.is_empty() || chunk.len() > remaining {
            return Err(host_error(
                "storage",
                "blob store returned invalid range length",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    if BlobRef::from_bytes(&bytes) != *reference {
        return Err(host_error("storage", "blob content digest mismatch"));
    }
    Ok(bytes)
}

async fn materialize(
    blobs: &dyn BlobStore,
    outcome: &CodeToolCallOutcome,
    limit: u64,
) -> Result<Value, HostError> {
    let value = match &outcome.output_ref {
        Some(reference) => {
            let bytes = read_bounded(blobs, reference, limit).await?;
            Some(serde_json::from_slice(&bytes).map_err(|error| {
                host_error(
                    "invalid_output",
                    format!("tool output is not JSON: {error}"),
                )
            })?)
        }
        None => None,
    };
    if outcome.status == CodeToolCallStatus::Succeeded {
        return Ok(value.unwrap_or(Value::Null));
    }
    let kind = match outcome.status {
        CodeToolCallStatus::Failed => "tool_failed",
        CodeToolCallStatus::Cancelled => "tool_cancelled",
        CodeToolCallStatus::Unavailable => "tool_unavailable",
        _ => "invalid_output",
    };
    let message = match &outcome.error_ref {
        Some(reference) => String::from_utf8(read_bounded(blobs, reference, limit).await?)
            .map_err(|error| {
                host_error(
                    "invalid_output",
                    format!("tool error is not UTF-8: {error}"),
                )
            })?,
        None => format!("tool completed with status {:?}", outcome.status),
    };
    Err(HostError {
        kind: kind.into(),
        message,
        value,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness::{
        ToolCallId, ToolName,
        storage::{BlobInfo, BlobStoreError, InMemoryBlobStore},
    };
    use serde_json::json;

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_interrupts_a_stalled_ownership_query_promptly() {
        let cancellation = Cancellation::default();
        let request = cancellable_preparation(
            Instant::now() + Duration::from_secs(600),
            &cancellation,
            std::future::pending::<Result<(), CodeRunError>>(),
        );
        let cancel = async {
            tokio::task::yield_now().await;
            cancellation.cancel();
        };
        let (_, result) = tokio::join!(
            cancel,
            tokio::time::timeout(Duration::from_secs(1), request)
        );
        assert!(matches!(
            result.expect("cancellation must not wait for the query deadline"),
            Err(CodeRunError::Cancelled)
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_preparation_does_not_start_its_work() {
        let cancellation = Cancellation::default();
        cancellation.cancel();
        let mut work_started = false;
        let result = cancellable_preparation(
            Instant::now() + Duration::from_secs(1),
            &cancellation,
            async {
                work_started = true;
                Ok(())
            },
        )
        .await;
        assert!(matches!(result, Err(CodeRunError::Cancelled)));
        assert!(!work_started);
    }

    fn scope() -> CodeToolScopeReport {
        CodeToolScopeReport {
            execution_id: "code-test".into(),
            closed: false,
            cancel_requested: false,
            bindings: [
                ("binding-a".into(), ToolName::new("mcp.read")),
                ("binding-b".into(), ToolName::new("timer_sleep")),
            ]
            .into(),
            calls: Default::default(),
        }
    }

    fn outcome(
        status: CodeToolCallStatus,
        output_ref: Option<BlobRef>,
        error_ref: Option<BlobRef>,
    ) -> CodeToolCallOutcome {
        CodeToolCallOutcome {
            request_id: "call-1".into(),
            call_id: ToolCallId::new("code-tool-1"),
            status,
            output_ref,
            error_ref,
            attachments: Vec::new(),
        }
    }

    #[test]
    fn catalog_can_narrow_but_cannot_substitute_or_duplicate_admitted_bindings() {
        let scope = scope();
        let mut catalog = CodeToolCatalog::from_scope(&scope);
        catalog.validate(&scope).expect("matching catalog");
        catalog.bindings.pop();
        catalog.validate(&scope).expect("narrowed catalog");
        catalog.bindings[0].name = "other_tool".into();
        assert!(matches!(
            catalog.validate(&scope),
            Err(CodeRunError::Catalog(_))
        ));
        catalog = CodeToolCatalog::from_scope(&scope);
        catalog.bindings.push(catalog.bindings[0].clone());
        assert!(matches!(
            catalog.validate(&scope),
            Err(CodeRunError::Catalog(_))
        ));
        catalog = CodeToolCatalog::from_scope(&scope);
        catalog.version = 2;
        assert!(matches!(
            catalog.validate(&scope),
            Err(CodeRunError::Catalog(_))
        ));
    }

    #[tokio::test]
    async fn materialization_preserves_mcp_envelopes_and_structured_tool_failures() {
        let blobs = InMemoryBlobStore::new();
        let envelope = json!({"content":[{"type":"text","text":"complete"}],
            "structuredContent":{"items":[1,2]},"isError":false});
        let reference = blobs
            .put_bytes(serde_json::to_vec(&envelope).unwrap())
            .await
            .unwrap();
        let success = outcome(CodeToolCallStatus::Succeeded, Some(reference.clone()), None);
        assert_eq!(materialize(&blobs, &success, 4096).await.unwrap(), envelope);
        let error_ref = blobs.put_bytes(b"request failed".to_vec()).await.unwrap();
        let failure = outcome(CodeToolCallStatus::Failed, Some(reference), Some(error_ref));
        assert_eq!(
            materialize(&blobs, &failure, 4096).await.unwrap_err(),
            HostError {
                kind: "tool_failed".into(),
                message: "request failed".into(),
                value: Some(envelope),
            }
        );
    }

    #[tokio::test]
    async fn oversized_and_invalid_json_results_fail_at_the_guest_boundary() {
        let blobs = InMemoryBlobStore::new();
        let reference = blobs.put_bytes(b"{\"value\":123}".to_vec()).await.unwrap();
        let success = outcome(CodeToolCallStatus::Succeeded, Some(reference), None);
        assert_eq!(
            materialize(&blobs, &success, 4).await.unwrap_err().kind,
            "payload_too_large"
        );
        assert_eq!(success.status, CodeToolCallStatus::Succeeded);
        let invalid = blobs.put_bytes(b"not json".to_vec()).await.unwrap();
        assert_eq!(
            materialize(
                &blobs,
                &outcome(CodeToolCallStatus::Succeeded, Some(invalid), None),
                1024
            )
            .await
            .unwrap_err()
            .kind,
            "invalid_output"
        );
    }

    #[tokio::test]
    async fn metadata_limit_is_checked_before_reading_blob_contents() {
        struct MetadataOnlyStore;
        #[async_trait::async_trait]
        impl BlobStore for MetadataOnlyStore {
            async fn put_bytes(&self, _: Vec<u8>) -> Result<BlobRef, BlobStoreError> {
                panic!("unexpected write")
            }
            async fn read_bytes(&self, _: &BlobRef) -> Result<Vec<u8>, BlobStoreError> {
                panic!("oversized blob must not be read")
            }
            async fn has_blob(&self, _: &BlobRef) -> Result<bool, BlobStoreError> {
                Ok(true)
            }
            async fn stat_blob(&self, reference: &BlobRef) -> Result<BlobInfo, BlobStoreError> {
                Ok(BlobInfo {
                    blob_ref: reference.clone(),
                    byte_len: 1_000_000_000,
                })
            }
        }
        assert_eq!(
            read_bounded(&MetadataOnlyStore, &BlobRef::from_bytes(b"large"), 1024)
                .await
                .unwrap_err()
                .kind,
            "payload_too_large"
        );
    }
}
