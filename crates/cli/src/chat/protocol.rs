use api::{RunStatus, SessionStatus, WorkspaceAccess};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

pub(crate) const GATEWAY_WORLD_ID: &str = "gateway";
pub(crate) const DEFAULT_CHAT_REASONING_EFFORT: ReasoningEffort = ReasoningEffort::High;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatDraftSettings {
    /// Model route: the requested one when `route_requested`, otherwise the
    /// session's resolved deployment default (empty until a session is read).
    pub provider: String,
    pub api_kind: String,
    pub model: String,
    /// The route was chosen by a flag or picker and is sent with session
    /// starts and runs; otherwise the server's deployment default applies.
    #[serde(default)]
    pub route_requested: bool,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub max_tokens: Option<u32>,
    pub web_search: Option<bool>,
    pub web_fetch: Option<bool>,
    /// Access of the workspace attached for `--mount`; file tools are
    /// derived from attachments, so `None` means the default (edit).
    pub filesystem_tools: Option<WorkspaceAccess>,
    /// Send no feature grants at all: the true secure default (model +
    /// runs only) instead of the CLI's dev feature set.
    #[serde(default)]
    pub bare: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReasoningEffort {
    Low,
    Medium,
    High,
}

impl Default for ChatDraftSettings {
    fn default() -> Self {
        Self {
            provider: String::new(),
            api_kind: String::new(),
            model: String::new(),
            route_requested: false,
            reasoning_effort: default_reasoning_effort_from_env(),
            max_tokens: std::env::var("LIGHTSPEED_CHAT_MAX_TOKENS")
                .ok()
                .and_then(|value| value.parse::<u32>().ok()),
            web_search: None,
            web_fetch: None,
            filesystem_tools: None,
            bare: false,
        }
    }
}

fn default_reasoning_effort_from_env() -> Option<ReasoningEffort> {
    match std::env::var("LIGHTSPEED_CHAT_REASONING_EFFORT") {
        Ok(value) => parse_reasoning_effort(&value).unwrap_or(Some(DEFAULT_CHAT_REASONING_EFFORT)),
        Err(_) => Some(DEFAULT_CHAT_REASONING_EFFORT),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum ChatCommand {
    SubmitUserMessage {
        text: String,
    },
    SetDraftProvider {
        provider: String,
    },
    SetDraftModel {
        model: String,
    },
    /// Provider, API kind, and model chosen together from discovery.
    SetDraftRoute {
        provider: String,
        api_kind: String,
        model: String,
    },
    /// Discover models through `models/list`, then open the picker for `purpose`.
    ListModels {
        purpose: ModelPickerPurpose,
    },
    SetDraftReasoningEffort {
        effort: Option<ReasoningEffort>,
    },
    SetDraftMaxTokens {
        max_tokens: Option<u32>,
    },
    ListSessions,
    ListSkills,
    PickSkill,
    UseSkill {
        skill_id: String,
    },
    NewSession,
    SteerRun {
        text: String,
    },
    InterruptRun {
        reason: Option<String>,
    },
    DecideApproval {
        approval_id: String,
        decision: api::ApprovalDecisionKind,
        note: Option<String>,
    },
    PauseSession,
    ResumeSession,
    SwitchSession {
        session_id: String,
    },
    Refresh,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ChatEvent {
    Connected(ChatConnectionInfo),
    SessionsListed {
        world_id: String,
        sessions: Vec<ChatSessionSummary>,
    },
    SkillsListed {
        session_id: String,
        catalogs: Vec<api::SkillCatalogView>,
    },
    /// `models/list` result that opens the picker for `purpose`.
    ModelsListed {
        purpose: ModelPickerPurpose,
        models: Vec<api::ModelView>,
        providers: Vec<api::ModelProviderDiscoveryView>,
    },
    SessionSelected(ChatSessionSummary),
    HistoryReset {
        session_id: String,
    },
    TranscriptDelta(ChatDelta),
    RunChanged(ChatRunView),
    ApprovalsPending {
        session_id: String,
        run_id: String,
        approvals: Vec<api::PendingApprovalView>,
    },
    ToolChainsChanged {
        session_id: String,
        chains: Vec<ChatToolChainView>,
    },
    CompactionsChanged {
        session_id: String,
        compactions: Vec<ChatCompactionView>,
    },
    StatusChanged(ChatStatus),
    GapObserved {
        requested_from: u64,
        retained_from: u64,
    },
    Reconnecting {
        from: u64,
        reason: String,
    },
    Error(ChatErrorView),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatConnectionInfo {
    pub world_id: String,
    pub session_id: String,
    pub journal_next_from: Option<u64>,
    pub settings: ChatSettingsView,
}

/// Which picker a `models/list` request feeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ModelPickerPurpose {
    Model,
    Provider,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatSettingsView {
    pub provider: String,
    pub api_kind: String,
    pub model: String,
    /// API kind the session is pinned to, from the last `session/read`;
    /// model choices must keep it. `None` until the session is read.
    #[serde(default)]
    pub session_api_kind: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub max_tokens: Option<u32>,
    pub provider_editable: bool,
    pub model_editable: bool,
    pub effort_editable: bool,
    pub max_tokens_editable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatSessionSummary {
    pub session_id: String,
    pub status: Option<SessionStatus>,
    pub lifecycle: Option<ChatSessionLifecycle>,
    pub updated_at_ns: Option<u64>,
    pub run_count: u64,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub active_run: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChatSessionLifecycle {
    NotLoaded,
    Idle,
    Active,
    Closed,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatStatus {
    pub session_id: String,
    pub status: String,
    pub detail: Option<String>,
    pub settings: ChatSettingsView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatErrorView {
    pub message: String,
    pub action: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ChatDelta {
    ReplaceTurns {
        session_id: String,
        turns: Vec<ChatTurn>,
    },
    AppendMessage {
        session_id: String,
        message: ChatMessageView,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatTurn {
    pub turn_id: String,
    pub user: Option<ChatMessageView>,
    #[serde(default)]
    pub assistant_reasoning: Option<ChatReasoningView>,
    pub assistant: Option<ChatMessageView>,
    pub run: Option<ChatRunView>,
    pub tool_chains: Vec<ChatToolChainView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatMessageView {
    pub id: String,
    pub role: String,
    pub content: String,
    pub ref_: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatReasoningView {
    pub id: String,
    pub content: String,
    pub ref_: Option<String>,
    pub output_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatRunView {
    pub id: String,
    pub run_seq: u64,
    pub lifecycle: RunStatus,
    pub status: ChatProgressStatus,
    pub provider: String,
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub input_refs: Vec<String>,
    pub output_ref: Option<String>,
    pub started_at_ns: u64,
    pub updated_at_ns: u64,
    /// Boxed: most run events carry none, and this keeps `ChatEvent` small.
    #[serde(default)]
    pub stats: Box<ChatRunStats>,
}

/// Per-run statistics shown under a finished turn. Every field is optional:
/// usage and duration come from the run summary, tool calls from the run
/// detail, and model calls/context only from generation events this client
/// observed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatRunStats {
    /// Provider token usage summed over the run's generations.
    pub usage: Option<api::LlmUsageView>,
    pub duration_ms: Option<u64>,
    pub tool_calls: Option<usize>,
    pub model_calls: Option<u32>,
    /// Prompt size of the run's last generation: the context window in use.
    pub context_tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatToolChainView {
    pub id: String,
    pub title: String,
    pub status: ChatProgressStatus,
    #[serde(default)]
    pub reasoning: Option<ChatReasoningView>,
    pub calls: Vec<ChatToolCallView>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatToolCallView {
    pub id: String,
    pub tool_id: Option<String>,
    pub tool_name: String,
    pub status: ChatProgressStatus,
    pub group_index: Option<u64>,
    pub parallel_safe: Option<bool>,
    pub resource_key: Option<String>,
    pub arguments_preview: Option<String>,
    pub result_preview: Option<String>,
    pub error: Option<String>,
    pub display: Option<ChatToolCallDisplayView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatToolCallDisplayView {
    pub group: ChatToolDisplayGroup,
    pub verb: String,
    pub target: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ChatToolDisplayGroup {
    Explore,
    Edit,
    Execute,
    Mcp,
    Agent,
    Bot,
    Message,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatCompactionView {
    pub id: String,
    pub status: ChatProgressStatus,
    pub reason: Option<String>,
    pub before_tokens: Option<u64>,
    pub after_tokens: Option<u64>,
    pub artifact_ref: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChatProgressStatus {
    Queued,
    Running,
    Waiting,
    Succeeded,
    Failed,
    Cancelled,
    Stale,
    Unknown,
}

pub(crate) fn parse_reasoning_effort(value: &str) -> anyhow::Result<Option<ReasoningEffort>> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "default" => Ok(Some(DEFAULT_CHAT_REASONING_EFFORT)),
        "none" | "off" => Ok(None),
        "low" => Ok(Some(ReasoningEffort::Low)),
        "medium" | "med" => Ok(Some(ReasoningEffort::Medium)),
        "high" => Ok(Some(ReasoningEffort::High)),
        other => anyhow::bail!(
            "invalid reasoning effort '{other}' (expected low, medium, high, or none)"
        ),
    }
}

pub(crate) fn reasoning_effort_label(value: Option<ReasoningEffort>) -> &'static str {
    match value {
        Some(ReasoningEffort::Low) => "low",
        Some(ReasoningEffort::Medium) => "medium",
        Some(ReasoningEffort::High) => "high",
        None => "none",
    }
}

pub(crate) fn run_status(status: RunStatus) -> ChatProgressStatus {
    match status {
        RunStatus::Queued => ChatProgressStatus::Queued,
        RunStatus::Running | RunStatus::Parked | RunStatus::Cancelling => {
            ChatProgressStatus::Running
        }
        RunStatus::Completed => ChatProgressStatus::Succeeded,
        RunStatus::Failed => ChatProgressStatus::Failed,
        RunStatus::Cancelled => ChatProgressStatus::Cancelled,
    }
}

/// One-line run summary, e.g.
/// `12.3s · in 38.1k (cache 81%: 31.0k read, 312 write) · out 850 · 3 model calls · 7 tool calls · context 14.2k`.
/// Cache reads and writes are already included in `inputTokens`.
pub(crate) fn run_stats_summary(stats: &ChatRunStats) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(ms) = stats.duration_ms {
        parts.push(compact_duration(ms));
    }
    if let Some(usage) = stats.usage.as_ref() {
        if let Some(input) = usage.input_tokens {
            let cache = [
                usage
                    .cached_input_tokens
                    .map(|n| format!("{} read", compact_count(n))),
                usage
                    .cache_write_input_tokens
                    .map(|n| format!("{} write", compact_count(n))),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            if cache.is_empty() {
                parts.push(format!("in {}", compact_count(input)));
            } else {
                let hit = usage
                    .cached_input_tokens
                    .filter(|_| input > 0)
                    .map(|read| format!(" {}%", u64::from(read) * 100 / u64::from(input)))
                    .unwrap_or_default();
                parts.push(format!(
                    "in {} (cache{hit}: {})",
                    compact_count(input),
                    cache.join(", ")
                ));
            }
        }
        if let Some(output) = usage.output_tokens {
            parts.push(format!("out {}", compact_count(output)));
        }
        if let Some(reasoning) = usage.reasoning_tokens.filter(|n| *n > 0) {
            parts.push(format!("reasoning {}", compact_count(reasoning)));
        }
    }
    if let Some(calls) = stats.model_calls {
        parts.push(plural(u64::from(calls), "model call"));
    }
    if let Some(calls) = stats.tool_calls.filter(|n| *n > 0) {
        parts.push(plural(calls as u64, "tool call"));
    }
    if let Some(context) = stats.context_tokens {
        parts.push(format!("context {}", compact_count(context)));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn plural(n: u64, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

fn compact_duration(ms: u64) -> String {
    match ms {
        0..1_000 => format!("{ms}ms"),
        1_000..60_000 => format!("{:.1}s", ms as f64 / 1_000.0),
        _ => {
            let secs = ms / 1_000;
            format!("{}m {:02}s", secs / 60, secs % 60)
        }
    }
}

fn compact_count(n: u32) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", f64::from(n) / 1_000.0),
        _ => format!("{:.1}M", f64::from(n) / 1_000_000.0),
    }
}

pub(crate) fn session_lifecycle(status: SessionStatus) -> ChatSessionLifecycle {
    match status {
        SessionStatus::NotLoaded => ChatSessionLifecycle::NotLoaded,
        SessionStatus::Idle => ChatSessionLifecycle::Idle,
        SessionStatus::Active => ChatSessionLifecycle::Active,
        SessionStatus::Closed => ChatSessionLifecycle::Closed,
        SessionStatus::Error => ChatSessionLifecycle::Error,
    }
}

#[allow(dead_code)]
pub(crate) fn session_active(lifecycle: ChatSessionLifecycle) -> bool {
    matches!(lifecycle, ChatSessionLifecycle::Active)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reasoning_effort_maps_default_to_high_and_none_to_off() {
        assert_eq!(
            parse_reasoning_effort("default").expect("default effort"),
            Some(ReasoningEffort::High)
        );
        assert_eq!(parse_reasoning_effort("none").expect("none effort"), None);
    }

    #[test]
    fn run_stats_summary_shows_cache_split_and_skips_missing_fields() {
        let stats = ChatRunStats {
            usage: Some(api::LlmUsageView {
                input_tokens: Some(38_100),
                output_tokens: Some(850),
                reasoning_tokens: Some(0),
                total_tokens: Some(38_950),
                cached_input_tokens: Some(31_000),
                cache_write_input_tokens: Some(312),
            }),
            duration_ms: Some(12_345),
            tool_calls: Some(7),
            model_calls: Some(3),
            context_tokens: Some(14_200),
        };
        assert_eq!(
            run_stats_summary(&stats).as_deref(),
            Some(
                "12.3s · in 38.1k (cache 81%: 31.0k read, 312 write) · out 850 \
                 · 3 model calls · 7 tool calls · context 14.2k"
            )
        );

        let sparse = ChatRunStats {
            usage: Some(api::LlmUsageView {
                input_tokens: Some(40),
                output_tokens: Some(2_500_000),
                ..Default::default()
            }),
            duration_ms: Some(75_000),
            tool_calls: Some(0),
            model_calls: Some(1),
            ..Default::default()
        };
        assert_eq!(
            run_stats_summary(&sparse).as_deref(),
            Some("1m 15s · in 40 · out 2.5M · 1 model call")
        );
        assert_eq!(run_stats_summary(&ChatRunStats::default()), None);
    }
}
