use api::{HostToolMode, RunStatus, SessionStatus, SkillActivationScope};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

pub(crate) const GATEWAY_WORLD_ID: &str = "gateway";
pub(crate) const DEFAULT_CHAT_PROVIDER: &str = "openai";
pub(crate) const DEFAULT_CHAT_API_KIND: &str = "openai:responses";
pub(crate) const DEFAULT_CHAT_MODEL: &str = "gpt-5.5";
pub(crate) const DEFAULT_CHAT_REASONING_EFFORT: ReasoningEffort = ReasoningEffort::High;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatDraftSettings {
    pub provider: String,
    pub api_kind: String,
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub max_tokens: Option<u32>,
    pub web_search: Option<bool>,
    pub web_fetch: Option<bool>,
    pub host_tools: Option<HostToolMode>,
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
            provider: std::env::var("LIGHTSPEED_CHAT_PROVIDER")
                .unwrap_or_else(|_| DEFAULT_CHAT_PROVIDER.into()),
            api_kind: std::env::var("LIGHTSPEED_CHAT_API_KIND")
                .unwrap_or_else(|_| DEFAULT_CHAT_API_KIND.into()),
            model: std::env::var("LIGHTSPEED_CHAT_MODEL").unwrap_or_else(|_| DEFAULT_CHAT_MODEL.into()),
            reasoning_effort: default_reasoning_effort_from_env(),
            max_tokens: std::env::var("LIGHTSPEED_CHAT_MAX_TOKENS")
                .ok()
                .and_then(|value| value.parse::<u32>().ok()),
            web_search: None,
            web_fetch: None,
            host_tools: None,
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
    SetDraftReasoningEffort {
        effort: Option<ReasoningEffort>,
    },
    SetDraftMaxTokens {
        max_tokens: Option<u32>,
    },
    ListSessions,
    ListSkills,
    ListActiveSkills,
    PickSkill {
        scope: SkillActivationScope,
    },
    ActivateSkill {
        skill_id: String,
        scope: SkillActivationScope,
    },
    DeactivateSkill {
        skill_id: String,
    },
    NewSession,
    SteerRun {
        text: String,
    },
    InterruptRun {
        reason: Option<String>,
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
        catalog_ref: Option<String>,
        skills: Vec<api::SkillListItem>,
        scope: SkillActivationScope,
    },
    SessionSelected(ChatSessionSummary),
    HistoryReset {
        session_id: String,
    },
    TranscriptDelta(ChatDelta),
    RunChanged(ChatRunView),
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatSettingsView {
    pub provider: String,
    pub api_kind: String,
    pub model: String,
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
        RunStatus::Running | RunStatus::Cancelling => ChatProgressStatus::Running,
        RunStatus::Completed => ChatProgressStatus::Succeeded,
        RunStatus::Failed => ChatProgressStatus::Failed,
        RunStatus::Cancelled => ChatProgressStatus::Cancelled,
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
}
