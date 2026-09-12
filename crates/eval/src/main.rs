mod casefile;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use api::{ContextEntryKindView, ContextEntryView, ContextMessageRoleView, RunStatus};
use api_projection::{
    CoreAgentProjector, MAX_EVENT_PAGE_LIMIT, ProjectRun, read_all_session_entries, started_run_id,
};
use async_trait::async_trait;
use casefile::{EvalCase, FileExpectation, SetupFile, load_cases};
use clap::{Parser, Subcommand};
use engine::{
    ContextConfig, ContextEntryInput, ContextEntryKey, ContextEntryKind, ContextMessageRole,
    CoreAgentCommand, GenerationConfig, LimitsConfig, ModelSelection, ProviderApiKind, RunConfig,
    SessionConfig, SessionId, ToolName, ToolSpec,
    storage::{BlobStore, CreateSession, InMemoryBlobStore, InMemorySessionStore, SessionStore},
};
use llm_clients::openai::{completions as oai_c, responses as oai};
use llm_clients::{ApiResponse, anthropic::messages as anthropic};
use llm_runtime::{
    AnthropicMessagesApi, AnthropicMessagesLlmAdapter, LlmAdapterRegistry, LlmRuntime,
    OpenAiCompletionsApi, OpenAiCompletionsLlmAdapter, OpenAiResponsesApi,
    OpenAiResponsesLlmAdapter,
};
use tempfile::TempDir;
use test_support::{
    DriveCommand, DriveOutcome, DriveSession, RunnerQuiescence, RunnerStores, SessionRunner,
};
use tools::{
    builtin::BuiltinToolOperation,
    environment::EnvironmentToolContext,
    fs::{FsPath, FsToolContext, ScopedLocalFileSystem},
    limits::ToolLimits,
    runtime::InlineToolRuntime,
    toolset::{BuiltinToolsetConfig, RegisteredToolset, ToolsetConfig, register_toolset},
};

const CASES_ROOT: &str = "crates/eval/cases";
const DEFAULT_OPENAI_MODEL: &str = "gpt-5.5";
const DEFAULT_ANTHROPIC_MODEL: &str = "claude-opus-5";
const DEFAULT_PROVIDER_ID: &str = "openai";
const EVAL_INSTRUCTIONS: &str = "\
You are running inside the Lightspeed agent eval harness.
Use the registered tools when the prompt asks for filesystem work.
Do not claim a file or tool action succeeded unless the corresponding tool result shows it.
Keep final answers concise and follow exact-answer instructions literally.";

#[derive(Parser, Debug)]
#[command(
    name = "eval",
    version,
    about = "Run prompt-level Lightspeed agent tool evals"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    #[arg(long, global = true, default_value = DEFAULT_PROVIDER_ID)]
    provider: String,

    #[arg(long, global = true, help = "Override the provider's default model")]
    model: Option<String>,

    #[arg(long, global = true, help = "Override number of runs per case")]
    runs: Option<u32>,

    #[arg(long, global = true, help = "Read cases from another directory")]
    cases_dir: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// List available eval cases.
    List,
    /// Run a single case by id.
    Case { id: String },
    /// Run all cases.
    All,
}

#[derive(Debug, Clone)]
struct ProviderRuntime {
    provider_id: String,
    api_kind: ProviderApiKind,
    model: String,
    api_key: String,
    base_url: Option<String>,
    organization: Option<String>,
    project: Option<String>,
}

#[derive(Debug, Clone)]
struct CaseRunSummary {
    case_id: String,
    passed_runs: u32,
    total_runs: u32,
    min_pass_rate: f64,
}

#[derive(Debug, Clone)]
struct AttemptOutcome {
    passed: bool,
    failures: Vec<String>,
    used_tools: BTreeSet<String>,
    tool_arguments: Vec<String>,
    assistant_text: String,
    tool_outputs_text: String,
    workdir: PathBuf,
}

#[derive(Debug, Default)]
struct ConversationObservations {
    assistant_text: String,
    used_tools: BTreeSet<String>,
    tool_outputs: Vec<String>,
    tool_arguments: Vec<String>,
    tool_errors: Vec<String>,
}

#[derive(Debug, Default)]
struct LlmDiagnostics {
    calls: Mutex<Vec<LlmCallDiagnostic>>,
}

#[derive(Debug, Clone)]
struct LlmCallDiagnostic {
    request: String,
    outcome: String,
}

struct DiagnosticOpenAiResponsesApi {
    inner: oai::Client,
    diagnostics: Arc<LlmDiagnostics>,
}

struct DiagnosticOpenAiCompletionsApi {
    inner: oai_c::Client,
    diagnostics: Arc<LlmDiagnostics>,
}

struct DiagnosticAnthropicMessagesApi {
    inner: anthropic::Client,
    diagnostics: Arc<LlmDiagnostics>,
}

#[async_trait]
impl AnthropicMessagesApi for DiagnosticAnthropicMessagesApi {
    async fn create(
        &self,
        request: anthropic::CreateMessageRequest,
        auth: Option<llm_clients::RequestAuth<'_>>,
    ) -> Result<ApiResponse<anthropic::Message>, llm_clients::LlmApiError> {
        let request_text = serde_json::to_string(&request)
            .unwrap_or_else(|error| format!("failed to encode request: {error}"));
        let result = self.inner.create_with_auth(request, auth).await;
        let outcome = match &result {
            Ok(response) => format!(
                "http_status={} response_id={} stop_reason={:?} raw={}",
                response.status,
                response.parsed.id,
                response.parsed.stop_reason,
                serde_json::to_string(&response.raw_json).unwrap_or_default()
            ),
            Err(error) => format!("api_error={error}"),
        };
        self.diagnostics.record(LlmCallDiagnostic {
            request: request_text,
            outcome,
        });
        result
    }
}

#[async_trait]
impl OpenAiResponsesApi for DiagnosticOpenAiResponsesApi {
    async fn create(
        &self,
        request: oai::CreateResponseRequest,
        auth: Option<llm_clients::RequestAuth<'_>>,
        endpoint: Option<&llm_clients::EndpointOverride>,
    ) -> Result<ApiResponse<oai::Response>, llm_clients::LlmApiError> {
        let request_text = serde_json::to_string(&request)
            .unwrap_or_else(|error| format!("failed to encode request: {error}"));
        let result = self
            .inner
            .create_with_transport(request, auth, endpoint)
            .await;
        let outcome = match &result {
            Ok(response) => format!(
                "http_status={} response_status={:?} raw={}",
                response.status,
                response.parsed.status,
                serde_json::to_string(&response.raw_json).unwrap_or_default()
            ),
            Err(error) => format!("api_error={error}"),
        };
        self.diagnostics.record(LlmCallDiagnostic {
            request: request_text,
            outcome,
        });
        result
    }

    async fn compact(
        &self,
        request: oai::CompactResponseRequest,
        auth: Option<llm_clients::RequestAuth<'_>>,
        endpoint: Option<&llm_clients::EndpointOverride>,
    ) -> Result<ApiResponse<oai::CompactResponse>, llm_clients::LlmApiError> {
        let request_text = serde_json::to_string(&request)
            .unwrap_or_else(|error| format!("failed to encode compact request: {error}"));
        let result = self
            .inner
            .compact_with_transport(request, auth, endpoint)
            .await;
        let outcome = match &result {
            Ok(response) => format!(
                "http_status={} raw={}",
                response.status,
                serde_json::to_string(&response.raw_json).unwrap_or_default()
            ),
            Err(error) => format!("api_error={error}"),
        };
        self.diagnostics.record(LlmCallDiagnostic {
            request: request_text,
            outcome,
        });
        result
    }
}

#[async_trait]
impl OpenAiCompletionsApi for DiagnosticOpenAiCompletionsApi {
    async fn create(
        &self,
        request: oai_c::CreateCompletionRequest,
        auth: Option<llm_clients::RequestAuth<'_>>,
        endpoint: Option<&llm_clients::EndpointOverride>,
    ) -> Result<ApiResponse<oai_c::Completion>, llm_clients::LlmApiError> {
        let request_text = serde_json::to_string(&request)
            .unwrap_or_else(|error| format!("failed to encode request: {error}"));
        let result = self
            .inner
            .create_with_transport(request, auth, endpoint)
            .await;
        let outcome = match &result {
            Ok(response) => format!(
                "http_status={} response_id={} finish_reason={:?} raw={}",
                response.status,
                response.parsed.id,
                response
                    .parsed
                    .choices
                    .first()
                    .and_then(|choice| choice.finish_reason.as_deref()),
                serde_json::to_string(&response.raw_json).unwrap_or_default()
            ),
            Err(error) => format!("api_error={error}"),
        };
        self.diagnostics.record(LlmCallDiagnostic {
            request: request_text,
            outcome,
        });
        result
    }
}

struct EvalInvocation {
    _temp: TempDir,
    workspaces_root: PathBuf,
    next_attempt: u64,
}

#[derive(Debug, Clone)]
struct AttemptWorkspace {
    root: PathBuf,
    vfs: PathBuf,
    environment: PathBuf,
}

impl EvalInvocation {
    fn new() -> Result<Self> {
        let temp = TempDir::new().context("create eval tempdir")?;
        let workspaces_root = temp.path().join("workspaces");
        fs::create_dir_all(&workspaces_root)
            .with_context(|| format!("create workspaces root {}", workspaces_root.display()))?;
        Ok(Self {
            _temp: temp,
            workspaces_root,
            next_attempt: 0,
        })
    }

    fn allocate_attempt(
        &mut self,
        case_id: &str,
        attempt_index: u32,
    ) -> Result<(String, AttemptWorkspace)> {
        self.next_attempt = self.next_attempt.saturating_add(1);
        let session_id = format!(
            "session_{}_{}",
            sanitize_case_id(case_id),
            self.next_attempt
        );
        let root = self.workspaces_root.join(format!(
            "{}-run-{}-{}",
            sanitize_case_id(case_id),
            attempt_index,
            self.next_attempt
        ));
        let vfs = root.join("vfs");
        let environment = root.join("environment");
        fs::create_dir_all(&vfs)
            .with_context(|| format!("create VFS eval workspace {}", vfs.display()))?;
        fs::create_dir_all(&environment).with_context(|| {
            format!(
                "create environment eval workspace {}",
                environment.display()
            )
        })?;
        Ok((
            session_id,
            AttemptWorkspace {
                root,
                vfs,
                environment,
            },
        ))
    }
}

impl LlmDiagnostics {
    fn record(&self, diagnostic: LlmCallDiagnostic) {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(diagnostic);
        }
    }

    fn summary(&self) -> Option<String> {
        let calls = self.calls.lock().ok()?;
        let last = calls.last()?;
        Some(format!(
            "llm_calls={} last_request={} last_outcome={}",
            calls.len(),
            preview(&last.request),
            preview(&last.outcome)
        ))
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = run_cli().await {
        eprintln!("error: {error}");
        for cause in error.chain().skip(1) {
            eprintln!("  caused by: {cause}");
        }
        std::process::exit(1);
    }
}

async fn run_cli() -> Result<()> {
    load_dotenvs();
    let cli = Cli::parse();
    let cases_dir = cli
        .cases_dir
        .unwrap_or_else(|| workspace_root().join(CASES_ROOT));
    let cases = load_cases(&cases_dir)?;

    match cli.command {
        Commands::List => {
            for case in &cases {
                let providers = if case.providers.is_empty() {
                    "openai,anthropic".to_owned()
                } else {
                    case.providers.join(",")
                };
                println!("{:<28} [{providers}] {}", case.id, case.description);
            }
            Ok(())
        }
        Commands::Case { id } => {
            let provider = resolve_provider(cli.provider, cli.model)?;
            let case = cases
                .into_iter()
                .find(|case| case.id == id)
                .ok_or_else(|| anyhow!("unknown case '{id}'"))?;
            if !supports_provider(&case, &provider.provider_id) {
                bail!(
                    "case '{}' does not support provider '{}'",
                    case.id,
                    provider.provider_id
                );
            }
            let mut invocation = EvalInvocation::new()?;
            let summary = run_case(&mut invocation, &case, &provider, cli.runs).await?;
            print_case_summary(&summary);
            enforce_summary(&summary)
        }
        Commands::All => {
            let provider = resolve_provider(cli.provider, cli.model)?;
            let mut invocation = EvalInvocation::new()?;
            let mut failures = Vec::new();
            let mut summaries = Vec::new();
            for case in &cases {
                if !supports_provider(case, &provider.provider_id) {
                    println!("\nCase {}: SKIP provider={}", case.id, provider.provider_id);
                    continue;
                }
                let summary = run_case(&mut invocation, case, &provider, cli.runs).await?;
                print_case_summary(&summary);
                if summary_pass_rate(&summary) + f64::EPSILON < summary.min_pass_rate {
                    failures.push(format!(
                        "{} ({:.2} < {:.2})",
                        summary.case_id,
                        summary_pass_rate(&summary),
                        summary.min_pass_rate
                    ));
                }
                summaries.push(summary);
            }
            print_aggregate_summary(&summaries);
            if !failures.is_empty() {
                bail!("case thresholds failed: {}", failures.join(", "));
            }
            Ok(())
        }
    }
}

async fn run_case(
    invocation: &mut EvalInvocation,
    case: &EvalCase,
    provider: &ProviderRuntime,
    runs_override: Option<u32>,
) -> Result<CaseRunSummary> {
    let runs = runs_override.or(case.eval.runs).unwrap_or(1).max(1);
    let min_pass_rate = case.eval.min_pass_rate.unwrap_or(1.0).clamp(0.0, 1.0);
    let mut passed_runs = 0_u32;

    println!("\nCase file: {}", case.source_file);
    println!("Case {}: {}", case.id, case.description);

    for attempt in 1..=runs {
        let outcome = run_attempt(invocation, case, provider, attempt).await?;
        if outcome.passed {
            passed_runs = passed_runs.saturating_add(1);
            println!("  run {:>2}: PASS tools={:?}", attempt, outcome.used_tools);
        } else {
            println!(
                "  run {:>2}: FAIL workdir={}",
                attempt,
                outcome.workdir.display()
            );
            for failure in &outcome.failures {
                println!("    - {failure}");
            }
            if !outcome.used_tools.is_empty() {
                println!("    tools={:?}", outcome.used_tools);
            }
            if !outcome.tool_arguments.is_empty() {
                println!("    tool_args={:?}", outcome.tool_arguments);
            }
            if !outcome.assistant_text.is_empty() {
                println!("    assistant={}", preview(&outcome.assistant_text));
            }
            if !outcome.tool_outputs_text.is_empty() {
                println!("    tool_output={}", preview(&outcome.tool_outputs_text));
            }
        }
    }

    Ok(CaseRunSummary {
        case_id: case.id.clone(),
        passed_runs,
        total_runs: runs,
        min_pass_rate,
    })
}

async fn run_attempt(
    invocation: &mut EvalInvocation,
    case: &EvalCase,
    provider: &ProviderRuntime,
    attempt_index: u32,
) -> Result<AttemptOutcome> {
    let (session_id, workspace) = invocation.allocate_attempt(&case.id, attempt_index)?;
    seed_files(&case.setup.files, &workspace.vfs)?;
    seed_files(&case.setup.environment_files, &workspace.environment)?;

    let runtime = build_runtime(case, provider, &workspace).await?;
    let session_id = SessionId::try_new(session_id)
        .map_err(|error| anyhow!("invalid generated session id: {error}"))?;
    runtime.start_session(&session_id).await?;
    let run = runtime.start_run(&session_id, &case.prompt).await?;

    let observations = collect_observations(&run.entries, &runtime.tool_id_by_name);
    let mut failures = Vec::new();

    if !matches!(run.status, RunStatus::Completed) {
        failures.push(format!("run status was {:?}", run.status));
        if let Some(summary) = runtime.diagnostics.summary() {
            failures.push(summary);
        }
    }
    for error in &observations.tool_errors {
        failures.push(format!("tool failed: {error}"));
    }

    for expected in &case.expect.tool_called {
        let expected = canonical_tool_id(expected);
        if !observations.used_tools.contains(&expected) {
            failures.push(format!(
                "expected tool call '{}', observed {:?}",
                expected, observations.used_tools
            ));
        }
    }

    let assistant_lower = observations.assistant_text.to_ascii_lowercase();
    for needle in &case.expect.assistant_contains {
        if !assistant_lower.contains(&needle.to_ascii_lowercase()) {
            failures.push(format!(
                "assistant text missing '{}' (assistant={})",
                needle,
                preview(&observations.assistant_text)
            ));
        }
    }

    let tool_outputs_text = observations.tool_outputs.join("\n");
    let tool_outputs_lower = tool_outputs_text.to_ascii_lowercase();
    for needle in &case.expect.tool_output_contains {
        if !tool_outputs_lower.contains(&needle.to_ascii_lowercase()) {
            failures.push(format!(
                "tool output missing '{}' (tool_output={})",
                needle,
                preview(&tool_outputs_text)
            ));
        }
    }

    for file in &case.expect.files {
        validate_file_expectation(&workspace.vfs, file, &mut failures)?;
    }
    for file in &case.expect.environment_files {
        validate_file_expectation(&workspace.environment, file, &mut failures)?;
    }

    Ok(AttemptOutcome {
        passed: failures.is_empty(),
        failures,
        used_tools: observations.used_tools,
        tool_arguments: observations.tool_arguments,
        assistant_text: observations.assistant_text,
        tool_outputs_text,
        workdir: workspace.root,
    })
}

struct EvalRuntime {
    runner: SessionRunner,
    sessions: Arc<InMemorySessionStore>,
    blobs: Arc<InMemoryBlobStore>,
    config: SessionConfig,
    instructions_ref: engine::BlobRef,
    tool_set: BTreeMap<ToolName, ToolSpec>,
    tool_id_by_name: BTreeMap<String, String>,
    diagnostics: Arc<LlmDiagnostics>,
}

struct EvalRunProjection<'a> {
    output: Option<&'a engine::ContentRef>,
    run_id: engine::RunId,
    status: engine::RunStatus,
    source: &'a engine::RunSource,
    started_at_ms: Option<u64>,
    completed_at_ms: Option<u64>,
    usage: Option<&'a engine::LlmUsage>,
}

impl EvalRuntime {
    async fn start_session(&self, session_id: &SessionId) -> Result<()> {
        self.sessions
            .create_session(CreateSession {
                metadata: Default::default(),
                session_id: session_id.clone(),
                display_name: None,
                origin: None,
                delete_after_close_ms: None,
                created_at_ms: 1,
            })
            .await
            .context("create eval session")?;
        self.drive(
            session_id.clone(),
            CoreAgentCommand::OpenSession {
                config: self.config.clone(),
            },
            10,
        )
        .await?;
        self.drive(
            session_id.clone(),
            CoreAgentCommand::UpsertContext {
                expected_revision: None,
                key: ContextEntryKey::new("instructions.000.eval"),
                entry: instruction_context_input(self.instructions_ref.clone()),
            },
            11,
        )
        .await?;
        self.drive(
            session_id.clone(),
            CoreAgentCommand::ReplaceTools {
                expected_revision: Some(0),
                tools: self.tool_set.clone(),
            },
            12,
        )
        .await?;
        Ok(())
    }

    async fn start_run(&self, session_id: &SessionId, prompt: &str) -> Result<api::RunView> {
        let input_ref = self
            .blobs
            .put_bytes(prompt.as_bytes().to_vec())
            .await
            .context("store eval prompt")?;
        let outcome = self
            .drive(
                session_id.clone(),
                CoreAgentCommand::RequestRun(engine::RunRequestCommand {
                    notify_on_terminal: Vec::new(),
                    submission_id: None,
                    source: engine::RunRequestSource::Input {
                        input: user_input(input_ref),
                    },
                    run_config: RunConfig {
                        max_turns: self.config.limits.max_turns,
                        max_tool_rounds: self.config.limits.max_tool_rounds,
                        ..Default::default()
                    },
                }),
                20,
            )
            .await?;
        let run_id = started_run_id(&outcome.emitted_entries)
            .or_else(|| outcome.state.runs.completed.last().map(|run| run.run_id))
            .or_else(|| outcome.state.runs.active.as_ref().map(|run| run.run_id))
            .ok_or_else(|| anyhow!("eval request did not start a run"))?;
        let projection = if let Some(run) = outcome
            .state
            .runs
            .completed
            .iter()
            .find(|run| run.run_id == run_id)
        {
            EvalRunProjection {
                run_id,
                status: run.status,
                source: &run.source,
                started_at_ms: run.started_at_ms,
                completed_at_ms: Some(run.completed_at_ms),
                output: run.output.as_ref(),
                usage: run.usage.as_ref(),
            }
        } else if let Some(run) = outcome
            .state
            .runs
            .active
            .as_ref()
            .filter(|run| run.run_id == run_id)
        {
            EvalRunProjection {
                run_id,
                status: run.status,
                source: &run.source,
                started_at_ms: run.started_at_ms,
                completed_at_ms: None,
                output: None,
                usage: run.usage.as_ref(),
            }
        } else {
            bail!("eval run {run_id} has no reducer metadata");
        };
        self.project_run(session_id, projection).await
    }

    async fn drive(
        &self,
        session_id: SessionId,
        command: CoreAgentCommand,
        observed_at_ms: u64,
    ) -> Result<DriveOutcome> {
        let mut outcome = self
            .runner
            .drive_command(DriveCommand {
                session_id: session_id.clone(),
                observed_at_ms,
                command,
                max_steps: Some(256),
            })
            .await
            .context("drive eval command")?;
        if let Some(rejection) = outcome.rejection.as_ref() {
            bail!("eval command rejected: {rejection}");
        }

        let mut slices = 0usize;
        while matches!(outcome.quiescence, RunnerQuiescence::IterationLimitReached) {
            if slices >= 10_000 {
                bail!("eval runner did not quiesce after repeated drive slices");
            }
            slices = slices.saturating_add(1);
            let next = self
                .runner
                .drive_until_quiescent(DriveSession {
                    session_id: session_id.clone(),
                    observed_at_ms,
                    max_steps: Some(256),
                })
                .await
                .context("continue eval drive")?;
            let made_progress = !next.emitted_entries.is_empty();
            outcome.emitted_entries.extend(next.emitted_entries);
            outcome.head = next.head;
            outcome.state = next.state;
            outcome.quiescence = next.quiescence;
            if !made_progress
                && matches!(outcome.quiescence, RunnerQuiescence::IterationLimitReached)
            {
                bail!("eval runner reached the drive step limit without making progress");
            }
        }
        Ok(outcome)
    }

    async fn project_run(
        &self,
        session_id: &SessionId,
        projection: EvalRunProjection<'_>,
    ) -> Result<api::RunView> {
        let entries = read_all_session_entries(
            self.sessions.as_ref(),
            session_id,
            MAX_EVENT_PAGE_LIMIT as usize,
        )
        .await
        .map_err(api_error)?;
        CoreAgentProjector::new(self.blobs.as_ref())
            .project_run_with_metadata(ProjectRun {
                entries: &entries,
                run_id: projection.run_id,
                status: api_projection::core_run_status_to_api_status(projection.status),
                output: projection.output,
                source: projection.source,
                started_at_ms: projection.started_at_ms,
                completed_at_ms: projection.completed_at_ms,
                usage: projection.usage,
            })
            .await
            .map_err(api_error)
    }
}

async fn build_runtime(
    case: &EvalCase,
    provider: &ProviderRuntime,
    workspace: &AttemptWorkspace,
) -> Result<EvalRuntime> {
    let blobs = Arc::new(InMemoryBlobStore::new());
    let sessions = Arc::new(InMemorySessionStore::new());
    let instructions_ref = blobs
        .put_bytes(EVAL_INSTRUCTIONS.as_bytes().to_vec())
        .await
        .context("store eval instructions")?;
    let model = ModelSelection {
        api_kind: provider.api_kind.clone(),
        provider_id: provider.provider_id.clone(),
        model: provider.model.clone(),
    };
    let default_config = session_config(case, model.clone());
    let diagnostics = Arc::new(LlmDiagnostics::default());
    let registry = match provider.api_kind {
        ProviderApiKind::OpenAiResponses => {
            let openai = DiagnosticOpenAiResponsesApi {
                inner: oai::Client::new(openai_config(provider))?,
                diagnostics: Arc::clone(&diagnostics),
            };
            LlmAdapterRegistry::new().with_generation_adapter(
                ProviderApiKind::OpenAiResponses,
                Arc::new(OpenAiResponsesLlmAdapter::new(
                    Arc::new(openai),
                    blobs.clone(),
                )),
            )
        }
        ProviderApiKind::OpenAiCompletions => {
            let openai = DiagnosticOpenAiCompletionsApi {
                inner: oai_c::Client::new(openai_completions_config(provider))?,
                diagnostics: Arc::clone(&diagnostics),
            };
            LlmAdapterRegistry::new().with_generation_adapter(
                ProviderApiKind::OpenAiCompletions,
                Arc::new(OpenAiCompletionsLlmAdapter::new(
                    Arc::new(openai),
                    blobs.clone(),
                )),
            )
        }
        ProviderApiKind::AnthropicMessages => {
            let anthropic = DiagnosticAnthropicMessagesApi {
                inner: anthropic::Client::new(anthropic_config(provider))?,
                diagnostics: Arc::clone(&diagnostics),
            };
            LlmAdapterRegistry::new().with_generation_adapter(
                ProviderApiKind::AnthropicMessages,
                Arc::new(AnthropicMessagesLlmAdapter::new(
                    Arc::new(anthropic),
                    blobs.clone(),
                )),
            )
        }
    };
    let llm_executor = Arc::new(LlmRuntime::new(registry));

    let vfs = Arc::new(
        ScopedLocalFileSystem::read_write(&workspace.vfs)
            .with_context(|| format!("open VFS eval workspace '{}'", workspace.vfs.display()))?,
    );
    let vfs_ctx = FsToolContext::new(vfs, blobs.clone()).with_cwd(FsPath::root());
    let environment_fs = Arc::new(
        ScopedLocalFileSystem::read_write(&workspace.environment).with_context(|| {
            format!(
                "open environment eval workspace '{}'",
                workspace.environment.display()
            )
        })?,
    );
    let environment_cwd = case
        .setup
        .environment_cwd
        .as_deref()
        .map(FsPath::new)
        .transpose()
        .context("parse setup.environment_cwd")?
        .unwrap_or_else(FsPath::root);
    let environment_fs_ctx =
        FsToolContext::new(environment_fs, blobs.clone()).with_cwd(environment_cwd.clone());
    let environment_ctx = EnvironmentToolContext::new(None, blobs.clone())
        .with_environment_id("eval-environment")
        .with_filesystem(environment_fs_ctx)
        .with_process_cwd(environment_cwd);
    let toolset = register_eval_toolset(case)?;
    let catalog = tools::runtime::ToolCatalog::from_registrations(
        &toolset.tools,
        &tools::runtime::ToolTarget::from(&model),
    )?;
    let tool_id_by_name = catalog
        .bindings()
        .map(|binding| {
            (
                binding.tool_name.as_str().to_string(),
                binding.logical_id.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let tool_executor = Arc::new(InlineToolRuntime::with_contexts_and_blob_store(
        Some(vfs_ctx),
        Some(environment_ctx),
        blobs.clone(),
        ToolLimits::default(),
        catalog,
    ));
    let stores = RunnerStores::new(sessions.clone(), blobs.clone());
    let runner = SessionRunner::new(stores, llm_executor).with_tools(tool_executor);
    Ok(EvalRuntime {
        runner,
        sessions,
        blobs,
        config: default_config,
        instructions_ref,
        tool_set: toolset.tools,
        tool_id_by_name,
        diagnostics,
    })
}

fn register_eval_toolset(case: &EvalCase) -> Result<RegisteredToolset> {
    let mut config = ToolsetConfig::workspace();
    if let Some(allowed) = case.run.allowed_tools.as_ref() {
        if allowed.is_empty() {
            bail!("case '{}' has empty run.allowed_tools", case.id);
        }
        let mut vfs_operations = Vec::new();
        let mut environment_operations = Vec::new();
        for id in allowed {
            let operation = builtin_operation_for_id(id)
                .ok_or_else(|| anyhow!("case '{}' references unknown tool '{id}'", case.id))?;
            if canonical_tool_id(id).starts_with("vfs.") {
                vfs_operations.push(operation);
            } else {
                environment_operations.push(operation);
            }
        }
        let mut builtin = BuiltinToolsetConfig::vfs_from_operations(vfs_operations)?;
        builtin.environment =
            BuiltinToolsetConfig::from_operations(environment_operations).environment;
        config.builtin = builtin;
    }
    register_toolset(&config).context("build eval tools")
}

fn session_config(case: &EvalCase, model: ModelSelection) -> SessionConfig {
    SessionConfig {
        model,
        generation: GenerationConfig {
            max_output_tokens: case.run.max_tokens,
            ..Default::default()
        },
        limits: LimitsConfig {
            max_turns: Some(case.run.max_turns.unwrap_or(12)),
            max_tool_rounds: Some(case.run.max_tool_rounds.unwrap_or(8)),
        },
        context: ContextConfig { compaction: None },
        features: Default::default(),
    }
}

fn user_input(content_ref: engine::BlobRef) -> Vec<ContextEntryInput> {
    vec![ContextEntryInput {
        kind: ContextEntryKind::Message {
            role: ContextMessageRole::User,
        },
        content: engine::ContentRef {
            content_ref,
            media_type: None,
            provider_kind: None,
        },
        preview: None,
        origin: None,
        provenance_ref: None,
        token_estimate: None,
    }]
}

fn instruction_context_input(content_ref: engine::BlobRef) -> ContextEntryInput {
    ContextEntryInput {
        kind: ContextEntryKind::Instructions,
        content: engine::ContentRef::text(content_ref),
        preview: None,
        origin: None,
        provenance_ref: None,
        token_estimate: None,
    }
}

fn collect_observations(
    entries: &[ContextEntryView],
    tool_id_by_name: &BTreeMap<String, String>,
) -> ConversationObservations {
    let mut observations = ConversationObservations::default();
    for entry in entries {
        match &entry.kind {
            ContextEntryKindView::Message {
                role: ContextMessageRoleView::Assistant,
            } => {
                if let Some(text) = entry.text.as_deref()
                    && !text.trim().is_empty()
                {
                    observations.assistant_text.push_str(text.trim());
                    observations.assistant_text.push('\n');
                }
            }
            ContextEntryKindView::ToolCall { name, .. } => {
                let tool_id = tool_id_by_name
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| canonical_tool_id(name));
                observations.used_tools.insert(tool_id.clone());
                if let Some(arguments) = entry.text.as_deref() {
                    observations
                        .tool_arguments
                        .push(format!("{tool_id} {}", compact_json_text(arguments)));
                }
            }
            ContextEntryKindView::ToolResult { is_error, .. } => {
                if let Some(output) = entry.text.as_deref() {
                    observations.tool_outputs.push(output.to_owned());
                    if *is_error {
                        observations.tool_errors.push(output.to_owned());
                    }
                } else if *is_error {
                    observations
                        .tool_errors
                        .push("missing tool error output".into());
                }
            }
            _ => {}
        }
    }
    observations.assistant_text = observations.assistant_text.trim().to_string();
    observations.tool_outputs.sort();
    observations.tool_outputs.dedup();
    observations.tool_arguments.sort();
    observations.tool_arguments.dedup();
    observations.tool_errors.sort();
    observations.tool_errors.dedup();
    observations
}

fn seed_files(files: &[SetupFile], root: &Path) -> Result<()> {
    for file in files {
        let path = safe_case_path(root, &file.path)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create parent dirs for {}", path.display()))?;
        }
        fs::write(&path, file.content.as_bytes())
            .with_context(|| format!("seed file {}", path.display()))?;
    }
    Ok(())
}

fn validate_file_expectation(
    workdir: &Path,
    expectation: &FileExpectation,
    failures: &mut Vec<String>,
) -> Result<()> {
    let path = safe_case_path(workdir, &expectation.path)?;
    let exists = path.exists();

    if let Some(expected_exists) = expectation.exists
        && expected_exists != exists
    {
        failures.push(format!(
            "file '{}' existence mismatch: expected {}, got {}",
            expectation.path, expected_exists, exists
        ));
    }

    if expectation.equals.is_none() && expectation.contains.is_none() {
        return Ok(());
    }

    if !exists {
        failures.push(format!(
            "expected file '{}' for content assertion, but it does not exist",
            expectation.path
        ));
        return Ok(());
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("read assertion file {}", path.display()))?;

    if let Some(expected) = expectation.equals.as_ref()
        && &content != expected
    {
        failures.push(format!(
            "file '{}' mismatch: expected {:?}, got {:?}",
            expectation.path, expected, content
        ));
    }

    if let Some(needle) = expectation.contains.as_ref()
        && !content.contains(needle)
    {
        failures.push(format!(
            "file '{}' missing substring {:?}; content={:?}",
            expectation.path, needle, content
        ));
    }

    Ok(())
}

fn safe_case_path(root: &Path, path: &str) -> Result<PathBuf> {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        bail!("case path must be relative: {path}");
    }
    if candidate
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        bail!("case path must not contain '..': {path}");
    }
    Ok(root.join(candidate))
}

fn builtin_operation_for_id(id: &str) -> Option<BuiltinToolOperation> {
    let id = canonical_tool_id(id);
    Some(match id.as_str() {
        "vfs.read_file" => BuiltinToolOperation::ReadFile,
        "vfs.write_file" => BuiltinToolOperation::WriteFile,
        "vfs.edit_file" => BuiltinToolOperation::EditFile,
        "vfs.apply_patch" => BuiltinToolOperation::ApplyPatch,
        "vfs.grep" => BuiltinToolOperation::Grep,
        "vfs.glob" => BuiltinToolOperation::Glob,
        "vfs.list_dir" => BuiltinToolOperation::ListDir,
        "env.read_file" => BuiltinToolOperation::ReadFile,
        "env.write_file" => BuiltinToolOperation::WriteFile,
        "env.edit_file" => BuiltinToolOperation::EditFile,
        "env.apply_patch" => BuiltinToolOperation::ApplyPatch,
        "env.grep" => BuiltinToolOperation::Grep,
        "env.glob" => BuiltinToolOperation::Glob,
        "env.list_dir" => BuiltinToolOperation::ListDir,
        "env.run_process" => BuiltinToolOperation::RunProcess,
        "env.continue_process" => BuiltinToolOperation::ContinueProcess,
        _ => return None,
    })
}

fn canonical_tool_id(value: &str) -> String {
    value.trim().to_owned()
}

fn openai_config(provider: &ProviderRuntime) -> oai::Config {
    let mut config = oai::Config::new(provider.api_key.clone());
    if let Some(base_url) = provider.base_url.clone() {
        config.base_url = base_url;
    }
    config.organization = provider.organization.clone();
    config.project = provider.project.clone();
    config
}

fn openai_completions_config(provider: &ProviderRuntime) -> oai_c::Config {
    let mut config = oai_c::Config::new(provider.api_key.clone());
    if let Some(base_url) = provider.base_url.clone() {
        config.base_url = base_url;
    }
    config.organization = provider.organization.clone();
    config.project = provider.project.clone();
    config
}

fn anthropic_config(provider: &ProviderRuntime) -> anthropic::Config {
    let mut config = anthropic::Config::new(provider.api_key.clone());
    if let Some(base_url) = provider.base_url.clone() {
        config.base_url = base_url;
    }
    config
}

fn resolve_provider(provider_id: String, model: Option<String>) -> Result<ProviderRuntime> {
    match provider_id.as_str() {
        "openai" => {
            let api_key = env_var("OPENAI_API_KEY")
                .ok_or_else(|| anyhow!("missing OPENAI_API_KEY (env or .env)"))?;
            Ok(ProviderRuntime {
                provider_id,
                api_kind: ProviderApiKind::OpenAiResponses,
                model: model
                    .or_else(|| env_var("OPENAI_RESPONSES_MODEL"))
                    .or_else(|| env_var("OPENAI_LIVE_MODEL"))
                    .unwrap_or_else(|| DEFAULT_OPENAI_MODEL.to_owned()),
                api_key,
                base_url: env_var("OPENAI_BASE_URL"),
                organization: env_var("OPENAI_ORG_ID"),
                project: env_var("OPENAI_PROJECT_ID"),
            })
        }
        "openai-completions" => {
            let api_key = env_var("OPENAI_API_KEY")
                .ok_or_else(|| anyhow!("missing OPENAI_API_KEY (env or .env)"))?;
            Ok(ProviderRuntime {
                provider_id: "openai".to_owned(),
                api_kind: ProviderApiKind::OpenAiCompletions,
                model: model
                    .or_else(|| env_var("OPENAI_COMPLETIONS_MODEL"))
                    .or_else(|| env_var("OPENAI_LIVE_MODEL"))
                    .unwrap_or_else(|| DEFAULT_OPENAI_MODEL.to_owned()),
                api_key,
                base_url: env_var("OPENAI_BASE_URL"),
                organization: env_var("OPENAI_ORG_ID"),
                project: env_var("OPENAI_PROJECT_ID"),
            })
        }
        "anthropic" => {
            let api_key = env_var("ANTHROPIC_API_KEY")
                .ok_or_else(|| anyhow!("missing ANTHROPIC_API_KEY (env or .env)"))?;
            Ok(ProviderRuntime {
                provider_id,
                api_kind: ProviderApiKind::AnthropicMessages,
                model: model
                    .or_else(|| env_var("ANTHROPIC_MESSAGES_MODEL"))
                    .or_else(|| env_var("ANTHROPIC_LIVE_MODEL"))
                    .unwrap_or_else(|| DEFAULT_ANTHROPIC_MODEL.to_owned()),
                api_key,
                base_url: env_var("ANTHROPIC_BASE_URL"),
                organization: None,
                project: None,
            })
        }
        _ => bail!(
            "eval supports --provider openai, --provider openai-completions, or --provider anthropic"
        ),
    }
}

fn supports_provider(case: &EvalCase, provider_id: &str) -> bool {
    case.providers.is_empty()
        || case
            .providers
            .iter()
            .any(|provider| provider == provider_id)
}

fn load_dotenvs() {
    for path in dotenv_candidates() {
        let _ = dotenvy::from_path(path);
    }
}

fn dotenv_candidates() -> Vec<PathBuf> {
    vec![
        workspace_root().join(".env"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".env"),
        PathBuf::from(".env"),
    ]
}

fn env_var(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn print_case_summary(summary: &CaseRunSummary) {
    println!(
        "  summary: {}/{} pass ({:.2}), threshold={:.2}",
        summary.passed_runs,
        summary.total_runs,
        summary_pass_rate(summary),
        summary.min_pass_rate
    );
}

fn print_aggregate_summary(summaries: &[CaseRunSummary]) {
    let total_runs = summaries
        .iter()
        .map(|summary| summary.total_runs)
        .sum::<u32>();
    let passed_runs = summaries
        .iter()
        .map(|summary| summary.passed_runs)
        .sum::<u32>();
    let pass_rate = if total_runs == 0 {
        0.0
    } else {
        passed_runs as f64 / total_runs as f64
    };
    println!(
        "\nAggregate: passed_runs={}/{} ({:.2})",
        passed_runs, total_runs, pass_rate
    );
}

fn enforce_summary(summary: &CaseRunSummary) -> Result<()> {
    let pass_rate = summary_pass_rate(summary);
    if pass_rate + f64::EPSILON < summary.min_pass_rate {
        bail!(
            "case '{}' below threshold: {:.2} < {:.2}",
            summary.case_id,
            pass_rate,
            summary.min_pass_rate
        );
    }
    Ok(())
}

fn summary_pass_rate(summary: &CaseRunSummary) -> f64 {
    if summary.total_runs == 0 {
        0.0
    } else {
        summary.passed_runs as f64 / summary.total_runs as f64
    }
}

fn compact_json_text(value: &str) -> String {
    serde_json::from_str::<serde_json::Value>(value)
        .ok()
        .and_then(|json| serde_json::to_string(&json).ok())
        .unwrap_or_else(|| value.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn preview(text: &str) -> String {
    let trimmed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.len() <= 800 {
        trimmed
    } else {
        format!("{}...", &trimmed[..800])
    }
}

fn sanitize_case_id(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "case".to_string()
    } else {
        trimmed.to_string()
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("parent of crate dir")
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn api_error(error: api::AgentApiError) -> anyhow::Error {
    anyhow!("{error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_entry(id: &str, kind: ContextEntryKindView, text: Option<&str>) -> ContextEntryView {
        ContextEntryView {
            id: id.to_owned(),
            key: None,
            kind,
            content: api::ContentRefView {
                content_ref: "sha256:entry".to_owned(),
                media_type: None,
                provider_kind: None,
                media_handle: None,
            },
            origin: None,
            provenance_ref: None,
            preview: None,
            provider_item_id: None,
            token_estimate: None,
            text: text.map(str::to_owned),
            text_truncated: false,
            display: None,
            citations: Vec::new(),
            source: None,
            supersedes: None,
            superseded_by: None,
        }
    }

    #[test]
    fn canonical_tool_id_preserves_explicit_domain_ids_without_aliases() {
        assert_eq!(canonical_tool_id(" vfs.read_file "), "vfs.read_file");
        assert_eq!(canonical_tool_id("env.run_process"), "env.run_process");
    }

    #[test]
    fn collect_observations_maps_tool_names_to_logical_ids() {
        let mut tool_ids = BTreeMap::new();
        tool_ids.insert("vfs_read_file".to_string(), "vfs.read_file".to_string());
        let observations = collect_observations(
            &[
                test_entry(
                    "item_1",
                    ContextEntryKindView::ToolCall {
                        call_id: "call_1".into(),
                        name: "vfs_read_file".into(),
                    },
                    Some(r#"{"path":"notes/fruit.txt"}"#),
                ),
                test_entry(
                    "item_2",
                    ContextEntryKindView::ToolResult {
                        call_id: "call_1".into(),
                        is_error: false,
                    },
                    Some("pear-9421"),
                ),
                test_entry(
                    "item_3",
                    ContextEntryKindView::Message {
                        role: ContextMessageRoleView::Assistant,
                    },
                    Some("FRUIT=pear-9421"),
                ),
            ],
            &tool_ids,
        );

        assert!(observations.used_tools.contains("vfs.read_file"));
        assert!(
            observations
                .tool_arguments
                .iter()
                .any(|item| item.contains("notes/fruit.txt"))
        );
        assert_eq!(observations.tool_outputs, vec!["pear-9421"]);
        assert_eq!(observations.assistant_text, "FRUIT=pear-9421");
        assert!(observations.tool_errors.is_empty());
    }

    #[test]
    fn compact_json_text_normalizes_argument_json() {
        assert_eq!(
            compact_json_text("{\n  \"path\": \"a.txt\"\n}"),
            r#"{"path":"a.txt"}"#
        );
    }

    #[test]
    fn safe_case_path_rejects_absolute_and_parent_paths() {
        let root = Path::new("/tmp/root");
        assert!(safe_case_path(root, "/etc/passwd").is_err());
        assert!(safe_case_path(root, "../outside").is_err());
        assert_eq!(
            safe_case_path(root, "notes/file.txt").expect("safe"),
            root.join("notes/file.txt")
        );
    }

    #[test]
    fn builtin_operation_for_id_resolves_supported_tools() {
        assert_eq!(
            builtin_operation_for_id("vfs.grep"),
            Some(BuiltinToolOperation::Grep)
        );
        assert_eq!(
            builtin_operation_for_id("env.list_dir"),
            Some(BuiltinToolOperation::ListDir)
        );
        assert!(builtin_operation_for_id("fs.read_file").is_none());
    }

    #[test]
    fn provider_allowlist_defaults_to_both_and_can_narrow() {
        let mut case: EvalCase = serde_json::from_value(serde_json::json!({
            "id": "provider-test",
            "prompt": "test"
        }))
        .expect("case");
        assert!(supports_provider(&case, "openai"));
        assert!(supports_provider(&case, "anthropic"));

        case.providers = vec!["openai".to_owned()];
        assert!(supports_provider(&case, "openai"));
        assert!(!supports_provider(&case, "anthropic"));
    }
}
