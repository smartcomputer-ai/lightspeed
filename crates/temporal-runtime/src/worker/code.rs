//! Code-role activities. Session authority stays behind the generic code-tool
//! protocol; these activities consume only the invocation's pinned CAS context.

use std::{sync::Arc, time::Duration};

use harness::{
    BlobRef, PromiseResolution,
    storage::{BlobStore, BlobStoreError, record_contains_edges},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use temporal_workflow::{
    ACTIVITY_CODE_FINALIZE, ACTIVITY_CODE_PREPARE, ACTIVITY_CODE_RUN, CodeExecutionDescriptor,
    CodeExecutionInterruption, CodeExecutionLimits, CodeExecutionTerminal,
    CodeFinalizeActivityRequest, CodePrepareActivityRequest, CodePrepareActivityResult,
    CodeRunActivityResult, CodeToolClient, CodeToolRejectionKind, CodeToolScopeReport,
    CodeToolScopeReportRequest, OpenCodeToolScopeRequest, WorkflowToolStartArgs,
};
use temporalio_macros::activities;
use temporalio_sdk::{
    ApplicationFailure,
    activities::{ActivityContext, ActivityError},
};
use tokio::sync::Semaphore;
use tools::code::{
    CODE_EXECUTE_WORKFLOW_SEMANTIC_TYPE, CODE_EXECUTE_WORKFLOW_TOOL_ID, CodeExecutionContextV1,
};

use super::universes::WorkerUniverses;
use crate::{
    code::{Cancellation, CodeRunReport, CodeRunner, CodeToolCatalog, read_bounded},
    gateway::GatewayAgentApi,
    universe::UniverseRuntime,
};

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(10);
const PREPARE_TIMEOUT: Duration = Duration::from_secs(45);
const PINNED_CONTEXT_MAX_BYTES: u64 = 1024 * 1024;
const REPORT_MAX_BYTES: u64 = 64 * 1024 * 1024;

pub struct CodeWorkerActivities {
    universes: WorkerUniverses,
    /// Shared across all universes, including when this role shares a process.
    capacity: Arc<Semaphore>,
}

impl CodeWorkerActivities {
    pub fn for_universe(
        universe_id: uuid::Uuid,
        api: Arc<GatewayAgentApi>,
        max_concurrent_executions: usize,
    ) -> anyhow::Result<Self> {
        Self::new(
            WorkerUniverses::Fixed { universe_id, api },
            max_concurrent_executions,
        )
    }

    pub fn with_runtime(
        runtime: Arc<UniverseRuntime>,
        max_concurrent_executions: usize,
    ) -> anyhow::Result<Self> {
        Self::new(WorkerUniverses::Runtime(runtime), max_concurrent_executions)
    }

    fn new(universes: WorkerUniverses, capacity: usize) -> anyhow::Result<Self> {
        anyhow::ensure!(
            capacity > 0 && capacity <= Semaphore::MAX_PERMITS,
            "code interpreter capacity must be positive and representable"
        );
        Ok(Self {
            universes,
            capacity: Arc::new(Semaphore::new(capacity)),
        })
    }

    fn runner(&self, api: &GatewayAgentApi) -> CodeRunner {
        CodeRunner::with_shared_capacity(
            api.temporal_client().clone(),
            api.store().clone(),
            self.capacity.clone(),
            CLEANUP_TIMEOUT,
        )
    }
}

#[activities]
impl CodeWorkerActivities {
    #[activity(name = ACTIVITY_CODE_PREPARE)]
    pub async fn code_prepare(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: CodePrepareActivityRequest,
    ) -> Result<CodePrepareActivityResult, ActivityError> {
        require_activity_owner(&ctx, &request.start.execution_id)?;
        let work = async {
            let api = self.universes.api_for(request.start.universe_id).await?;
            let result = tokio::time::timeout(PREPARE_TIMEOUT, prepare(&api, &request.start))
                .await
                .map_err(|_| retryable("code preparation timed out"))?;
            match result {
                Ok(descriptor) => Ok(CodePrepareActivityResult::Prepared { descriptor }),
                Err(PreparationError::Invalid(message)) => {
                    let error_ref = api
                        .store()
                        .put_bytes(message.into_bytes())
                        .await
                        .map_err(retryable)?;
                    Ok(CodePrepareActivityResult::Rejected { error_ref })
                }
                Err(error) => Err(retryable(error)),
            }
        };
        tokio::pin!(work);
        let heartbeat = heartbeat(&ctx);
        tokio::pin!(heartbeat);
        tokio::select! {
            result = &mut work => result,
            () = &mut heartbeat => unreachable!("heartbeats run until activity exits"),
            () = ctx.cancelled() => Err(ActivityError::cancelled()),
        }
    }

    #[activity(name = ACTIVITY_CODE_RUN)]
    pub async fn code_run(
        self: Arc<Self>,
        ctx: ActivityContext,
        descriptor: CodeExecutionDescriptor,
    ) -> Result<CodeRunActivityResult, ActivityError> {
        // Never replay source after a worker failure, even if an incorrectly
        // configured caller supplies a retry policy.
        validate_attempt(ctx.info().attempt)?;
        descriptor.validate().map_err(non_retryable)?;
        require_activity_owner(&ctx, &descriptor.execution_id)?;
        let (universe_id, _) =
            temporal_workflow::split_workflow_id(&descriptor.session_workflow_id)
                .ok_or_else(|| non_retryable("invalid parent session identity"))?;
        let cancellation = Cancellation::default();
        let work = async {
            let api = self.universes.api_for(universe_id).await?;
            let runner = self.runner(&api);
            let report = runner
                .run_once(descriptor.clone(), cancellation.clone())
                .await
                .map_err(non_retryable)?;
            let succeeded = report.execution.error.is_none() && report.cleanup_error.is_none();
            let report_ref = store_run_report(api.store().as_ref(), &descriptor, &report).await?;
            Ok(CodeRunActivityResult {
                report_ref,
                succeeded,
            })
        };
        tokio::pin!(work);
        let heartbeats = heartbeat(&ctx);
        tokio::pin!(heartbeats);
        let mut cancellation_received = false;
        loop {
            tokio::select! {
                result = &mut work => return result,
                () = &mut heartbeats => unreachable!("heartbeats run until activity exits"),
                () = ctx.cancelled(), if !cancellation_received => {
                    cancellation_received = true;
                    cancellation.cancel();
                    // Continue heartbeating through cleanup and report storage.
                    // The workflow retains this receipt but records cancellation.
                }
            }
        }
    }

    #[activity(name = ACTIVITY_CODE_FINALIZE)]
    pub async fn code_finalize(
        self: Arc<Self>,
        ctx: ActivityContext,
        request: CodeFinalizeActivityRequest,
    ) -> Result<PromiseResolution, ActivityError> {
        require_activity_owner(&ctx, &request.start.execution_id)?;
        let work = async {
            let api = self.universes.api_for(request.start.universe_id).await?;
            let runner = self.runner(&api);
            finalize(&api, &runner, request).await
        };
        tokio::pin!(work);
        let heartbeats = heartbeat(&ctx);
        tokio::pin!(heartbeats);
        tokio::select! {
            result = &mut work => result,
            () = &mut heartbeats => unreachable!("heartbeats run until activity exits"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum PreparationError {
    #[error("invalid code execution admission: {0}")]
    Invalid(String),
    #[error(transparent)]
    Blob(#[from] BlobStoreError),
    #[error("code scope transport failed: {0}")]
    Transport(String),
}

fn validate_start(start: &WorkflowToolStartArgs) -> Result<(), PreparationError> {
    if start.execution_id.is_empty()
        || start.universe_id != start.invocation.session_universe_id
        || start.holder_workflow_id
            != temporal_workflow::compose_workflow_id(
                start.universe_id,
                &start.invocation.session_id,
            )
        || start.invocation.tool_id.as_str() != CODE_EXECUTE_WORKFLOW_TOOL_ID
        || start.invocation.semantic_type != CODE_EXECUTE_WORKFLOW_SEMANTIC_TYPE
        || start.invocation.schema_revision != 1
    {
        return Err(PreparationError::Invalid(
            "workflow invocation identity or tool type does not match code execution".into(),
        ));
    }
    Ok(())
}

async fn pinned_context(
    blobs: &dyn BlobStore,
    start: &WorkflowToolStartArgs,
) -> Result<CodeExecutionContextV1, PreparationError> {
    validate_start(start)?;
    let reference = start
        .invocation
        .execution_context_ref
        .as_ref()
        .ok_or_else(|| PreparationError::Invalid("missing pinned execution context".into()))?;
    let bytes = read_bounded(blobs, reference, PINNED_CONTEXT_MAX_BYTES)
        .await
        .map_err(preparation_blob_error)?;
    let context: CodeExecutionContextV1 = serde_json::from_slice(&bytes)
        .map_err(|error| PreparationError::Invalid(error.to_string()))?;
    if context.version != CodeExecutionContextV1::VERSION
        || context.parent_session_id != start.invocation.session_id.as_str()
        || context.parent_run_id != start.invocation.run_id.as_u64()
    {
        return Err(PreparationError::Invalid(
            "pinned execution context identity mismatch".into(),
        ));
    }
    context
        .validate()
        .map_err(|error| PreparationError::Invalid(error.to_string()))?;
    execution_limits(&context)?
        .validate()
        .map_err(|error| PreparationError::Invalid(error.to_string()))?;
    Ok(context)
}

fn preparation_blob_error(error: codemode::HostError) -> PreparationError {
    if error.kind == "storage" {
        PreparationError::Transport(error.message)
    } else {
        PreparationError::Invalid(error.message)
    }
}

fn execution_limits(
    context: &CodeExecutionContextV1,
) -> Result<CodeExecutionLimits, PreparationError> {
    // Both public configurations are JSON records, kept independent of the JS
    // engine. This boundary rejects missing or unknown admitted budget fields.
    serde_json::from_value(
        serde_json::to_value(context.limits)
            .map_err(|error| PreparationError::Invalid(error.to_string()))?,
    )
    .map_err(|error| PreparationError::Invalid(error.to_string()))
}

fn scope_request(
    start: &WorkflowToolStartArgs,
    context: &CodeExecutionContextV1,
) -> OpenCodeToolScopeRequest {
    OpenCodeToolScopeRequest {
        execution_id: start.execution_id.clone(),
        parent_invocation_id: start.invocation.invocation_id.clone(),
        allowed_tools: context.allowed_tools.clone(),
        max_calls: context.limits.max_tool_calls,
        max_in_flight: context.limits.max_outstanding_tool_calls,
    }
}

async fn prepare(
    api: &GatewayAgentApi,
    start: &WorkflowToolStartArgs,
) -> Result<CodeExecutionDescriptor, PreparationError> {
    let blobs = api.store().as_ref();
    let context = pinned_context(blobs, start).await?;
    let limits = execution_limits(&context)?;
    // Verify source exists, is bounded and immutable before opening admission.
    let source = read_bounded(blobs, &context.source_ref, limits.max_source_bytes)
        .await
        .map_err(preparation_blob_error)?;
    std::str::from_utf8(&source).map_err(|error| PreparationError::Invalid(error.to_string()))?;
    let code_tools = CodeToolClient::new(
        api.temporal_client().clone(),
        start.holder_workflow_id.clone(),
    );
    let scope = code_tools
        .open_scope(scope_request(start, &context), Default::default())
        .await
        .map_err(|error| PreparationError::Transport(error.to_string()))?
        .map_err(|error| PreparationError::Invalid(error.to_string()))?;
    if scope.closed || !scope.calls.is_empty() {
        return Err(PreparationError::Invalid(
            "code scope is closed or already executed".into(),
        ));
    }
    let catalog = serde_json::to_vec(&CodeToolCatalog::from_scope(&scope))
        .map_err(|error| PreparationError::Invalid(error.to_string()))?;
    if catalog.len() as u64 > limits.max_catalog_bytes {
        return Err(PreparationError::Invalid(
            "admitted tool catalog exceeds its byte budget".into(),
        ));
    }
    let catalog_ref = blobs.put_bytes(catalog).await?;
    Ok(CodeExecutionDescriptor {
        execution_id: start.execution_id.clone(),
        session_workflow_id: start.holder_workflow_id.clone(),
        source_ref: context.source_ref,
        catalog_ref,
        limits,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DetailedCodeReport {
    version: u32,
    execution_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    descriptor: Option<CodeExecutionDescriptor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    execution: Option<codemode::ExecutionReport>,
    scope: Option<CodeToolScopeReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    interruption: Option<CodeExecutionInterruption>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cleanup_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    report_unavailable: Option<String>,
}

async fn store_run_report(
    store: &store_pg::PgStore,
    descriptor: &CodeExecutionDescriptor,
    report: &CodeRunReport,
) -> Result<BlobRef, ActivityError> {
    store_report(
        store,
        &DetailedCodeReport {
            version: 1,
            execution_id: descriptor.execution_id.clone(),
            descriptor: Some(descriptor.clone()),
            execution: Some(report.execution.clone()),
            scope: report.scope.clone(),
            interruption: None,
            cleanup_error: report.cleanup_error.clone(),
            report_unavailable: None,
        },
    )
    .await
}

async fn store_report(
    store: &store_pg::PgStore,
    report: &DetailedCodeReport,
) -> Result<BlobRef, ActivityError> {
    let bytes = serde_json::to_vec(report).map_err(retryable)?;
    let reference = store.put_bytes(bytes).await.map_err(retryable)?;
    record_contains_edges(Some(store), &reference, report_children(report))
        .await
        .map_err(retryable)?;
    Ok(reference)
}

fn report_children(report: &DetailedCodeReport) -> Vec<BlobRef> {
    let mut children = Vec::new();
    if let Some(descriptor) = &report.descriptor {
        children.extend([
            descriptor.source_ref.clone(),
            descriptor.catalog_ref.clone(),
        ]);
    }
    if let Some(scope) = &report.scope {
        for call in scope.calls.values() {
            children.extend(call.output_ref.iter().cloned());
            children.extend(call.error_ref.iter().cloned());
            children.extend(
                call.attachments
                    .iter()
                    .flat_map(harness::Attachment::blob_refs),
            );
        }
    }
    children
}

async fn finalize(
    api: &GatewayAgentApi,
    runner: &CodeRunner,
    request: CodeFinalizeActivityRequest,
) -> Result<PromiseResolution, ActivityError> {
    let start = &request.start;
    validate_start(start).map_err(non_retryable)?;
    let stored_result = match &request.terminal {
        CodeExecutionTerminal::Completed { result } => Some(result),
        CodeExecutionTerminal::Interrupted { result, .. } => result.as_ref(),
        CodeExecutionTerminal::Rejected { .. } => None,
    };
    let mut report = DetailedCodeReport {
        version: 1,
        execution_id: start.execution_id.clone(),
        descriptor: request.descriptor.clone(),
        execution: None,
        scope: None,
        interruption: None,
        cleanup_error: None,
        report_unavailable: None,
    };
    if let Some(result) = stored_result {
        let load = async {
            let bytes = read_bounded(api.store().as_ref(), &result.report_ref, REPORT_MAX_BYTES)
                .await
                .map_err(|error| error.message)?;
            let loaded: DetailedCodeReport =
                serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
            if loaded.version != 1
                || loaded.execution_id != start.execution_id
                || loaded.descriptor != request.descriptor
            {
                return Err("code report identity mismatch".to_owned());
            }
            Ok::<_, String>(loaded)
        };
        match tokio::time::timeout(Duration::from_secs(5), load).await {
            Ok(Ok(loaded)) => report = loaded,
            Ok(Err(error)) => {
                report.report_unavailable = Some(error);
                report.interruption = Some(CodeExecutionInterruption::ActivityFailed);
            }
            Err(_) => {
                report.report_unavailable =
                    Some("code report could not be loaded before deadline".into());
                report.interruption = Some(CodeExecutionInterruption::ActivityFailed);
            }
        }
    }
    if let CodeExecutionTerminal::Interrupted { reason, .. } = &request.terminal {
        report.interruption = Some(*reason);
    }
    let code_tools = CodeToolClient::new(
        api.temporal_client().clone(),
        start.holder_workflow_id.clone(),
    );
    let establish = async {
        // Closing an existing scope does not depend on reading the report or
        // pinned context successfully. Those artifacts can be unavailable after
        // a worker or storage failure while already admitted effects still run.
        let queried = code_tools
            .report(
                CodeToolScopeReportRequest {
                    execution_id: start.execution_id.clone(),
                },
                Default::default(),
            )
            .await
            .map_err(|error| error.to_string())?;
        match queried {
            Ok(scope) => return Ok::<_, String>(Some(scope)),
            Err(error) if error.kind == CodeToolRejectionKind::UnknownScope => {}
            Err(error) => return Err(error.to_string()),
        }
        // With a lost prepare receipt, establish the same scope then close it.
        // A delayed prepare can never reopen this closed admission boundary.
        let context = match pinned_context(api.store().as_ref(), start).await {
            Ok(context) => context,
            Err(PreparationError::Invalid(_)) => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let opened = code_tools
            .open_scope(scope_request(start, &context), Default::default())
            .await
            .map_err(|error| error.to_string())?;
        match opened {
            Ok(scope) => Ok(Some(scope)),
            // The holder no longer admits this parent, so a delayed prepare
            // cannot create a scope either.
            Err(error)
                if matches!(
                    error.kind,
                    CodeToolRejectionKind::SessionNotReady | CodeToolRejectionKind::UnknownScope
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error.to_string()),
        }
    };
    match tokio::time::timeout(Duration::from_secs(5), establish).await {
        Ok(Ok(Some(scope))) => report.scope = Some(scope),
        Ok(Ok(None)) => {}
        Ok(Err(error)) => {
            report.cleanup_error = Some(format!("could not establish cleanup scope: {error}"))
        }
        Err(_) => {
            report.cleanup_error = Some("could not establish cleanup scope before deadline".into())
        }
    }
    // Even if the initial query or context load failed, try closing by identity.
    // A last known scope snapshot remains useful if this second RPC also fails.
    if report.scope.as_ref().is_some_and(|scope| {
        scope.closed && scope.calls.values().all(|call| call.status.is_terminal())
    }) {
        // Closed terminal snapshots cannot gain later calls. They remain valid
        // even after the session prunes the scope in a later run.
        report.cleanup_error = None;
    } else if report.scope.is_some() || report.cleanup_error.is_some() {
        let (scope, error) = runner
            .cleanup(&code_tools, &start.execution_id, report.scope.take())
            .await;
        report.scope = scope;
        report.cleanup_error = error;
    }
    // Admission failures retain an ordinary failed promise. Its JSON diagnostic
    // still names the durable cleanup report when scope preparation ran partly.
    if let CodeExecutionTerminal::Rejected { error_ref } = request.terminal {
        let report_ref = store_report(api.store().as_ref(), &report).await?;
        let error_bytes = read_bounded(api.store().as_ref(), &error_ref, PINNED_CONTEXT_MAX_BYTES)
            .await
            .map_err(|error| retryable(error.message))?;
        let diagnostic = serde_json::json!({
            "status": "rejected", "error": String::from_utf8_lossy(&error_bytes), "report_ref": report_ref,
        });
        let reference = api
            .store()
            .put_bytes(serde_json::to_vec(&diagnostic).map_err(retryable)?)
            .await
            .map_err(retryable)?;
        record_contains_edges(Some(api.store().as_ref()), &reference, [report_ref])
            .await
            .map_err(retryable)?;
        return Ok(PromiseResolution::Failed {
            error_ref: Some(reference),
        });
    }
    let report_ref = store_report(api.store().as_ref(), &report).await?;
    let model = model_report(&report, &report_ref);
    let payload_ref = api
        .store()
        .put_bytes(serde_json::to_vec(&model).map_err(retryable)?)
        .await
        .map_err(retryable)?;
    record_contains_edges(Some(api.store().as_ref()), &payload_ref, [report_ref])
        .await
        .map_err(retryable)?;
    Ok(PromiseResolution::Resolved {
        payload_ref: Some(payload_ref),
    })
}

fn model_report(report: &DetailedCodeReport, report_ref: &BlobRef) -> Value {
    let status = match report.interruption {
        Some(
            CodeExecutionInterruption::HolderCancelled
            | CodeExecutionInterruption::WorkflowCancelled,
        ) => "cancelled",
        Some(_) => "interrupted",
        None if report
            .execution
            .as_ref()
            .is_some_and(|execution| execution.error.is_none())
            && report.cleanup_error.is_none() =>
        {
            "succeeded"
        }
        None => "failed",
    };
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    if let Some(scope) = &report.scope {
        for call in scope.calls.values() {
            let status = match call.status {
                temporal_workflow::CodeToolCallStatus::Succeeded => "succeeded",
                temporal_workflow::CodeToolCallStatus::Failed => "failed",
                temporal_workflow::CodeToolCallStatus::Cancelled => "cancelled",
                temporal_workflow::CodeToolCallStatus::Unavailable => "unavailable",
                temporal_workflow::CodeToolCallStatus::Pending
                | temporal_workflow::CodeToolCallStatus::Waiting => "pending",
            };
            *counts.entry(status).or_default() += 1;
        }
    }
    serde_json::json!({
        "status": status,
        "output_available": report.execution.is_some(),
        "calls": counts,
        "output": report.execution.as_ref().map(|execution| &execution.output).cloned().unwrap_or_default(),
        "return_value": report.execution.as_ref().and_then(|execution| execution.return_value.as_ref()),
        "error": report.execution.as_ref().and_then(|execution| execution.error.as_ref()),
        "interruption": report.interruption,
        "cleanup_error": report.cleanup_error,
        "report_unavailable": report.report_unavailable,
        "report_ref": report_ref,
    })
}

fn require_activity_owner(ctx: &ActivityContext, execution_id: &str) -> Result<(), ActivityError> {
    if ctx.info().workflow_id.as_deref() != Some(execution_id) {
        return Err(non_retryable("code activity workflow identity mismatch"));
    }
    Ok(())
}

fn validate_attempt(attempt: u32) -> Result<(), ActivityError> {
    if attempt != 1 {
        return Err(non_retryable(
            "JavaScript execution cannot be retried; prior effect outcomes may be unknown",
        ));
    }
    Ok(())
}

async fn heartbeat(ctx: &ActivityContext) {
    let mut ticks =
        tokio::time::interval(temporal_workflow::ACTIVITY_CANCELLATION_HEARTBEAT_INTERVAL);
    loop {
        ticks.tick().await;
        ctx.record_heartbeat(())
            .await
            .expect("unit heartbeat serializes");
    }
}

fn retryable(error: impl std::fmt::Display) -> ActivityError {
    ActivityError::application(ApplicationFailure::new(anyhow::anyhow!(error.to_string())))
}

fn non_retryable(error: impl std::fmt::Display) -> ActivityError {
    ActivityError::application(ApplicationFailure::non_retryable(anyhow::anyhow!(
        error.to_string()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_activity_defensively_rejects_source_retries() {
        validate_attempt(1).expect("first attempt");
        for attempt in [0, 2, 10] {
            let ActivityError::Application(error) =
                validate_attempt(attempt).expect_err("retry denied")
            else {
                panic!("expected application failure");
            };
            assert!(error.is_non_retryable());
        }
    }

    #[test]
    fn model_report_preserves_output_and_reports_unknown_interruption() {
        let report = DetailedCodeReport {
            version: 1,
            execution_id: "code-test".into(),
            descriptor: None,
            execution: Some(codemode::ExecutionReport {
                output: vec![serde_json::json!({"saved": true})],
                return_value: None,
                error: None,
                pending_request_ids: vec!["call-2".into()],
                metrics: Default::default(),
            }),
            scope: None,
            interruption: Some(CodeExecutionInterruption::ActivityTimedOut),
            cleanup_error: Some("pending effects have unknown outcomes".into()),
            report_unavailable: None,
        };
        let value = model_report(&report, &BlobRef::from_bytes(b"details"));
        assert_eq!(value["status"], "interrupted");
        assert_eq!(value["output"], serde_json::json!([{"saved": true}]));
        assert_eq!(value["output_available"], true);
        assert_eq!(value["interruption"], "activity_timed_out");
        assert!(value.get("scope").is_none());
    }

    #[test]
    fn detailed_report_tracks_result_and_attachment_blobs() {
        let output_ref = BlobRef::from_bytes(b"tool output");
        let error_ref = BlobRef::from_bytes(b"tool error");
        let file_ref = BlobRef::from_bytes(b"file");
        let report = DetailedCodeReport {
            version: 1,
            execution_id: "code-test".into(),
            descriptor: None,
            execution: None,
            scope: Some(CodeToolScopeReport {
                execution_id: "code-test".into(),
                closed: true,
                cancel_requested: true,
                bindings: Default::default(),
                calls: [(
                    "call-1".into(),
                    temporal_workflow::CodeToolCallOutcome {
                        request_id: "call-1".into(),
                        call_id: harness::ToolCallId::new("tool-call-1"),
                        status: temporal_workflow::CodeToolCallStatus::Failed,
                        output_ref: Some(output_ref.clone()),
                        error_ref: Some(error_ref.clone()),
                        attachments: vec![harness::Attachment::File(harness::FileAttachment::new(
                            file_ref.clone(),
                            "file.txt".into(),
                            None,
                        ))],
                    },
                )]
                .into(),
            }),
            interruption: None,
            cleanup_error: None,
            report_unavailable: None,
        };
        assert_eq!(
            report_children(&report),
            vec![output_ref, error_ref, file_ref]
        );
        let model = model_report(&report, &BlobRef::from_bytes(b"details"));
        assert_eq!(model["calls"]["failed"], 1);
        assert_eq!(model["output_available"], false);
        assert_eq!(model["output"], serde_json::json!([]));
    }
    #[test]
    fn activity_names_match_workflow_contract() {
        assert_eq!(
            CodeWorkerActivities::code_prepare.name(),
            temporal_workflow::WorkflowActivities::code_prepare.name()
        );
        assert_eq!(
            CodeWorkerActivities::code_run.name(),
            temporal_workflow::WorkflowActivities::code_run.name()
        );
        assert_eq!(
            CodeWorkerActivities::code_finalize.name(),
            temporal_workflow::WorkflowActivities::code_finalize.name()
        );
    }
}
