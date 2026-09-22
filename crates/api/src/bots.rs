//! Bots: durable event routers that own managed sessions in the core runtime.
//!
//! A bot is a universe record (brief, profile, budgets, capability grants)
//! plus triggers (schedule, webhook, poll, inbox, chat). Every event goes
//! through one admission pipeline into the bot's controller workflow, which
//! owns the bot's managed sessions. These are the wire shapes; the domain
//! crate `bots` adds records, validation, and the pure pipeline logic.

use super::*;

// ── Identity ────────────────────────────────────────────────────────────────

/// Authored, immutable bot id: lowercase ASCII alphanumerics and dashes,
/// at most 64 bytes. Every Temporal identity and session id of the bot
/// derives from it, and it is the name the model addresses the bot by.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BotId(String);

/// Authored trigger id, unique per bot; same rules as [`BotId`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BotTriggerId(String);

pub const BOT_ID_MAX_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BotIdError {
    #[error("{label} must not be empty")]
    Empty { label: &'static str },
    #[error("{label} must start with a lowercase ASCII letter or digit")]
    InvalidStart { label: &'static str },
    #[error(
        "{label} contains invalid character {ch:?} at byte {index}; allowed: lowercase ASCII letters, digits, '-'"
    )]
    InvalidCharacter {
        label: &'static str,
        index: usize,
        ch: char,
    },
    #[error("{label} must be at most {max} bytes")]
    TooLong { label: &'static str, max: usize },
}

pub fn validate_bot_name(label: &'static str, value: &str) -> Result<(), BotIdError> {
    if value.is_empty() {
        return Err(BotIdError::Empty { label });
    }
    if value.len() > BOT_ID_MAX_LEN {
        return Err(BotIdError::TooLong {
            label,
            max: BOT_ID_MAX_LEN,
        });
    }
    let first = value.chars().next().ok_or(BotIdError::Empty { label })?;
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(BotIdError::InvalidStart { label });
    }
    for (index, ch) in value.char_indices() {
        if !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-') {
            return Err(BotIdError::InvalidCharacter { label, index, ch });
        }
    }
    Ok(())
}

macro_rules! bot_name_newtype {
    ($name:ident, $label:expr) => {
        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                let value = value.into();
                Self::try_new(value)
                    .unwrap_or_else(|error| panic!("invalid {}: {error}", stringify!($name)))
            }

            pub fn try_new(value: impl Into<String>) -> Result<Self, BotIdError> {
                let value = value.into();
                validate_bot_name($label, &value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = BotIdError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::try_new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = BotIdError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::try_new(value)
            }
        }

        impl FromStr for $name {
            type Err = BotIdError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::try_new(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::try_new(value).map_err(de::Error::custom)
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
                String::json_schema(generator)
            }
        }
    };
}

bot_name_newtype!(BotId, "bot id");
bot_name_newtype!(BotTriggerId, "trigger id");

// ── Bot document ────────────────────────────────────────────────────────────

fn default_true() -> bool {
    true
}

/// Per-trigger flood breaker: a trigger that admits more than `fires`
/// events inside `window_ms` is disabled until a human re-enables it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotBreaker {
    pub fires: u32,
    pub window_ms: u64,
}

/// The mutable configuration of a bot, replaced whole with an expected
/// revision. The bot id, event counter, and lifecycle columns live on the
/// record, not here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotDocument {
    /// Mutable label for humans; the bot id stays the identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// One line other bots read in the directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub profile_id: ProfileId,
    /// Standing instructions appended to the profile's instructions — the
    /// bot's job description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<String>,
    /// Budget: runs started per UTC day (sub-agent descendants count);
    /// absent means unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs_per_day: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breaker: Option<BotBreaker>,
    /// Close routed (`perKey` / `perEvent`) sessions idle longer than this;
    /// absent keeps them open. A trigger's `sessionCloseAfterMs` overrides it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routed_session_close_after_ms: Option<u64>,
    /// Capability grant: the mutating self-configuration tools
    /// (`bot_trigger_put`, `bot_trigger_delete`, `bot_brief_put`).
    #[serde(default)]
    pub self_config: bool,
    /// Capability grant: `bot_emit` — events to itself or another bot's
    /// inbox. Emitting bots are rate-capped.
    #[serde(default)]
    pub emit: bool,
    /// Reversible pause: sessions and chat context stay, nothing is
    /// delivered.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotInput {
    pub bot_id: BotId,
    #[serde(flatten)]
    pub document: BotDocument,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotView {
    pub bot_id: BotId,
    pub revision: u64,
    #[serde(flatten)]
    pub document: BotDocument,
    /// Highest `#N` admitted so far.
    pub event_seq: u64,
    /// Terminal: set once by `bots/close`; a closed bot keeps its record
    /// and history but refuses events and cannot be re-enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at_ms: Option<i64>,
    /// Sessions the controller force-closed on the way out; `bots/delete`
    /// erases them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub closed_sessions: Vec<SessionId>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Roster row: the bot plus what the console needs at a glance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotListItem {
    #[serde(flatten)]
    pub bot: BotView,
    pub trigger_count: u32,
    /// Events whose delivery has not finished.
    pub pending_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event: Option<BotEventView>,
}

// ── Triggers ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum BotTriggerKind {
    Schedule,
    Webhook,
    Poll,
    /// The bot's inbox for events other bots address to it; at most one
    /// per bot.
    Bot,
    Chat,
}

impl BotTriggerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Schedule => "schedule",
            Self::Webhook => "webhook",
            Self::Poll => "poll",
            Self::Bot => "bot",
            Self::Chat => "chat",
        }
    }
}

impl fmt::Display for BotTriggerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How an inbound webhook proves it comes from the sender: possession of
/// the URL token, or additionally an HMAC-SHA256 over the raw body with a
/// secret leased from a retrievable grant.
#[derive(Default, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "scheme",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum WebhookVerification {
    #[default]
    Token,
    HmacSha256 {
        grant_id: String,
        header: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audience: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum WebhookPreset {
    Github,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum PollHttpMethod {
    #[default]
    Get,
    Post,
}

/// Leased credential for an HTTP poll source; the value never appears in
/// the trigger document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PollHttpAuth {
    pub grant_id: String,
    /// Header carrying the credential; default `authorization`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// Scheme prefix; default `Bearer`, empty sends the token raw.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheme: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
}

/// Where a poll trigger reads from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PollSource {
    Http {
        url: String,
        #[serde(default)]
        method: PollHttpMethod,
        /// Non-secret headers only; credentials come from `auth`.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth: Option<PollHttpAuth>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
    },
    Exec {
        /// Universe environment the command runs in (woken on use); absent
        /// runs in the bot's own environment (the profile's `existing`
        /// one), resolved at fire time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        environment_id: Option<EnvironmentId>,
        argv: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// Job wall-clock budget; also bounds the fire activity's wait.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
}

/// Dedupe discipline of a poll: an id set for unordered feeds, a watermark
/// for ordered ones.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PollCursorSpec {
    /// Dot path to each item's id.
    IdSet { id: String },
    /// Dot path to each item's monotonically increasing field.
    Watermark { field: String },
}

/// Poll cursor state: Lightspeed-owned, operator-visible, reset by a spec
/// edit. Absent until the baseline poll.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PollCursorState {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark: Option<Value>,
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baselined_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_polled_at_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ChatScope {
    Direct,
    Group,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ChatGroupActivation {
    /// Act on mentions, replies to the bot, and trigger prefixes only.
    #[default]
    Mention,
    /// Act on every message.
    Always,
}

/// When the bot acts in a conversation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChatActivation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<ChatGroupActivation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trigger_prefixes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mention_names: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ChatTurnAccess {
    /// Anyone in the conversation may take a turn.
    #[default]
    Anyone,
    /// Only the listed provider handles may take a turn.
    Listed,
}

/// Who may take a turn and who may issue control commands, by provider
/// handle (Telegram user id, WhatsApp JID). Handle allowlists on the
/// trigger replace any platform membership lookup.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChatAccess {
    #[serde(default)]
    pub turn: ChatTurnAccess,
    /// Handles allowed to take a turn when `turn` is `listed`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed: Vec<String>,
    /// Handles allowed to issue `/activation` and `/status`; empty denies
    /// control commands to everyone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controllers: Vec<String>,
}

/// Whether a conversation must present a pairing code before it connects.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ChatPairing {
    /// A conversation connects once someone sends the trigger's pairing
    /// code (minted by the server, shown to managers).
    #[default]
    Code,
    /// Every matching conversation connects implicitly.
    Open,
}

/// Per-kind trigger configuration. Routing, coalescing, filtering, and
/// delivery policy are generic trigger fields, not part of the spec.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BotTriggerSpec {
    Schedule {
        /// Classic 5-field cron or an `@macro`; exclusive with `atMs`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cron: Option<String>,
        /// One-shot instant; the trigger disables itself after firing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at_ms: Option<i64>,
        /// IANA timezone for cron evaluation; default `UTC`.
        #[serde(default = "default_timezone")]
        timezone: String,
        /// What the fired event asks the session to do.
        summary: String,
    },
    Webhook {
        #[serde(default)]
        verification: WebhookVerification,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preset: Option<WebhookPreset>,
    },
    Poll {
        source: PollSource,
        interval_ms: u64,
        /// Dot path to the item array in the payload; absent = the payload
        /// is the item list (or one item).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        items: Option<String>,
        cursor: PollCursorSpec,
    },
    /// Inbox: which bots may address this one; absent = any bot in the
    /// universe.
    Bot {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<Vec<BotId>>,
    },
    Chat {
        /// The universe's channel account this connection serves.
        account_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        match_scope: Option<ChatScope>,
        #[serde(default)]
        activation: ChatActivation,
        #[serde(default)]
        access: ChatAccess,
        #[serde(default)]
        pairing: ChatPairing,
        /// Lower wins among matching chat triggers on one account.
        #[serde(default = "default_chat_priority")]
        priority: u32,
    },
}

fn default_timezone() -> String {
    "UTC".to_owned()
}

fn default_chat_priority() -> u32 {
    100
}

impl BotTriggerSpec {
    pub fn kind(&self) -> BotTriggerKind {
        match self {
            Self::Schedule { .. } => BotTriggerKind::Schedule,
            Self::Webhook { .. } => BotTriggerKind::Webhook,
            Self::Poll { .. } => BotTriggerKind::Poll,
            Self::Bot { .. } => BotTriggerKind::Bot,
            Self::Chat { .. } => BotTriggerKind::Chat,
        }
    }
}

/// Which session a trigger's events are delivered to; absent means the
/// bot's main session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "policy",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum BotTriggerRoute {
    Bot,
    /// One session per key: a CEL expression over `{event, data, headers}`
    /// yielding the key, or the preset's key when absent.
    PerKey {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    /// One fresh session per event.
    PerEvent,
}

/// Coalescing window: events sharing a route flush as one delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotCoalescePolicy {
    pub debounce_ms: u64,
    pub max_wait_ms: u64,
    pub max_count: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum BotWhenBusy {
    /// Wait for the session to become idle.
    #[default]
    Queue,
    /// Fold the events into the running run as steering.
    Steer,
    /// Append the events as context without starting a run.
    Append,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotDeliverPolicy {
    #[serde(default)]
    pub when_busy: BotWhenBusy,
}

/// The whole configuration of one trigger, replaced with an expected
/// revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotTriggerDocument {
    #[serde(flatten)]
    pub spec: BotTriggerSpec,
    /// CEL over `{event, data, headers}`; a non-matching event is refused
    /// and never stored. Fails closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<BotTriggerRoute>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coalesce: Option<BotCoalescePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deliver: Option<BotDeliverPolicy>,
    /// Idle-close policy for sessions this trigger routes to: absent inherits the
    /// bot's `routedSessionCloseAfterMs`, `0` keeps them open indefinitely (the
    /// chat default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_close_after_ms: Option<u64>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotTriggerInput {
    pub trigger_id: BotTriggerId,
    #[serde(flatten)]
    pub document: BotTriggerDocument,
    /// Chat triggers with `pairing: code`: set a specific pairing code
    /// (8–64 chars) instead of the server-minted one. Never returned to
    /// non-managing principals.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_code: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BotTriggerDisabledReason {
    /// The flood breaker tripped.
    Breaker,
    /// Too many consecutive poll failures.
    PollFailed,
    /// A one-shot schedule fired.
    OneShot,
    /// A human turned it off.
    Operator,
    /// The bot was closed.
    BotClosed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotTriggerView {
    pub bot_id: BotId,
    pub trigger_id: BotTriggerId,
    pub revision: u64,
    #[serde(flatten)]
    pub document: BotTriggerDocument,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<BotTriggerDisabledReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_at_ms: Option<i64>,
    /// Last runtime failure of the CEL filter (the event was refused);
    /// cleared by the next match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_filter_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_filter_error_at_ms: Option<i64>,
    /// Poll triggers: the advancing runtime cursor. Named apart from the
    /// poll spec's dedupe `cursor`, which the flattened document already
    /// serializes under that key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_state: Option<PollCursorState>,
    /// Webhook triggers: the ingest path including its URL token, for
    /// managing principals only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingest_path: Option<String>,
    /// Chat triggers with `pairing: code`: the code, for managing
    /// principals only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_code: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

// ── Events ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum BotEventMediaKind {
    Image,
    Audio,
    Document,
}

/// A prepared attachment appended to the run input after the rendering;
/// bytes live in the CAS.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotEventMedia {
    pub blob_ref: String,
    pub kind: BotEventMediaKind,
    pub mime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// What came of an event: the model's decision (`handled`, `deferred`,
/// `ignored`, `blocked`) or the system's when no decision was possible.
/// `archived` rows were stored for the record and never delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BotEventOutcome {
    Handled,
    Deferred,
    Ignored,
    Blocked,
    Unresolved,
    RunFailed,
    Steered,
    Appended,
    Archived,
}

impl BotEventOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Handled => "handled",
            Self::Deferred => "deferred",
            Self::Ignored => "ignored",
            Self::Blocked => "blocked",
            Self::Unresolved => "unresolved",
            Self::RunFailed => "run_failed",
            Self::Steered => "steered",
            Self::Appended => "appended",
            Self::Archived => "archived",
        }
    }

    /// Outcomes the model may record through `bot_event_resolve`.
    pub fn is_model_decision(self) -> bool {
        matches!(
            self,
            Self::Handled | Self::Deferred | Self::Ignored | Self::Blocked
        )
    }
}

impl fmt::Display for BotEventOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The routed session an event was admitted to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotRoutedSessionView {
    pub session_id: SessionId,
    pub label: String,
}

/// Public correlation of a receipt: the asked event's `#N` at the answering
/// bot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotEventReplyRef {
    pub bot: BotId,
    pub seq: u64,
}

/// One row of the bot's event log. Grouped like the `bot_events` table:
/// identity, what arrived, the delivery plan, federation, and the
/// write-once outcome. The receiver route is private and never shown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotEventView {
    /// Per-bot sequence number: the only event handle shown to models and
    /// humans.
    pub seq: u64,
    /// Dedupe identity: provider delivery id where known, otherwise
    /// derived.
    pub event_id: String,
    /// Admitting trigger; absent for an operator admit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_id: Option<BotTriggerId>,
    /// Event kind as authored by the source (`github.push`,
    /// `schedule.fire`, `chat.message`, `bot.reply`).
    pub kind: String,
    pub summary: String,
    /// When the source says it happened.
    pub occurred_at_ms: i64,
    /// Admission time; the log is ordered by it.
    pub received_at_ms: i64,
    /// CAS ref of the full envelope document.
    pub document_ref: String,
    /// CAS ref of the model-facing rendering; what the session saw.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_ref: Option<String>,
    /// The routed session; absent means the bot's main session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<BotRoutedSessionView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<BotEventMedia>,
    /// Sending bot for bot-originated events (self or addressed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_bot_id: Option<BotId>,
    /// Bot-to-bot hops from the world.
    #[serde(default)]
    pub hops: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<BotEventReplyRef>,
    /// Absent while the delivery is pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<BotEventOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_detail: Option<String>,
    /// Run that resolved the event, when one was started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at_ms: Option<i64>,
}

/// Sender of a bot-originated event, as the receiver sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotEventSender {
    pub bot: BotId,
}

/// The envelope document stored in the CAS and shown to the session as
/// untrusted input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotEventDocument {
    pub version: u32,
    pub kind: String,
    pub source: String,
    pub occurred_at_ms: i64,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<BotEventSender>,
    #[serde(default)]
    pub hops: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<BotEventReplyRef>,
}

impl BotEventDocument {
    pub const VERSION: u32 = 1;
}

/// A manually admitted event: what an operator posts through
/// `bots/events/admit`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotEventInput {
    pub kind: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Dedupe identity; generated when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<String>,
}

// ── Controller state ────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BotControllerStatus {
    #[default]
    Initializing,
    Degraded,
    Idle,
    DeliveringEvent,
    BudgetExhausted,
    Closing,
    Closed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BotSetupStatus {
    #[default]
    Initializing,
    Degraded,
    Ready,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum BotSessionKind {
    Main,
    PerKey,
    PerEvent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotSessionSnapshot {
    pub session_id: SessionId,
    pub label: String,
    pub kind: BotSessionKind,
    pub generation: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active_at_ms: Option<i64>,
    /// A delivery lane or sidecar is running on it.
    pub busy: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotBufferSnapshot {
    pub key: String,
    pub seqs: Vec<u64>,
    pub first_at_ms: i64,
    pub last_at_ms: i64,
    pub flush_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotActiveDeliverySnapshot {
    pub delivery_id: String,
    pub seqs: Vec<u64>,
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    pub started_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotRecentDeliverySnapshot {
    pub delivery_id: String,
    pub seqs: Vec<u64>,
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    pub outcome: BotEventOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub finished_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<LlmUsageView>,
}

/// The controller's live snapshot (its `bot_state` query).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotControllerSnapshot {
    pub controller_status: BotControllerStatus,
    pub setup_status: BotSetupStatus,
    pub enabled: bool,
    pub closed: bool,
    pub main_session_id: SessionId,
    #[serde(default)]
    pub sessions: Vec<BotSessionSnapshot>,
    #[serde(default)]
    pub pending_deliveries: u32,
    #[serde(default)]
    pub buffers: Vec<BotBufferSnapshot>,
    #[serde(default)]
    pub active_deliveries: Vec<BotActiveDeliverySnapshot>,
    #[serde(default)]
    pub recent_deliveries: Vec<BotRecentDeliverySnapshot>,
    /// UTC day (`YYYY-MM-DD`) the budget counters belong to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_day: Option<String>,
    #[serde(default)]
    pub runs_today: u32,
    #[serde(default)]
    pub descendants_today: u32,
    #[serde(default)]
    pub events_processed: u64,
    #[serde(default)]
    pub duplicate_events: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_profile_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotStateView {
    /// Absent when the controller workflow is not running (a bot that never
    /// received an event, or a closed bot whose workflow completed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller: Option<BotControllerSnapshot>,
    /// Sub-agent sessions delegated under the bot's sessions.
    #[serde(default)]
    pub descendants: Vec<SessionSummaryView>,
}

// ── Methods ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotCreateParams {
    pub bot: BotInput,
    /// Triggers created with the bot in one go; a failure rolls the bot
    /// back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<BotTriggerInput>,
    /// Audience of the bot, its events and every session it creates, set
    /// atomically with its creation. Absent means universe-visible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<AccessInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotCreateResponse {
    pub bot: BotView,
    #[serde(default)]
    pub triggers: Vec<BotTriggerView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotPutParams {
    pub bot: BotInput,
    /// Checked only when the bot already exists; absent replaces (or
    /// creates) unconditionally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotPutResponse {
    pub bot: BotView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotReadParams {
    pub bot_id: BotId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotReadResponse {
    pub bot: BotView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotListParams {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotListResponse {
    #[serde(default)]
    pub bots: Vec<BotListItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotCloseParams {
    pub bot_id: BotId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotCloseResponse {
    pub bot: BotView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotDeleteParams {
    pub bot_id: BotId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotDeleteResponse {
    pub bot: BotView,
    /// Sessions deleted along with the bot.
    #[serde(default)]
    pub deleted_sessions: Vec<SessionId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotStateReadParams {
    pub bot_id: BotId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotStateReadResponse {
    pub state: BotStateView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotSessionRotateParams {
    pub bot_id: BotId,
    /// One of the bot's sessions (main or routed); the controller closes it
    /// at its next idle boundary and continues on a successor generation.
    pub session_id: SessionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotSessionRotateResponse {
    pub accepted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotTriggerPutParams {
    pub bot_id: BotId,
    pub trigger: BotTriggerInput,
    /// Checked only when the trigger already exists; absent replaces (or
    /// creates) unconditionally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotTriggerPutResponse {
    pub trigger: BotTriggerView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotTriggerReadParams {
    pub bot_id: BotId,
    pub trigger_id: BotTriggerId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotTriggerReadResponse {
    pub trigger: BotTriggerView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotTriggerListParams {
    pub bot_id: BotId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotTriggerListResponse {
    #[serde(default)]
    pub triggers: Vec<BotTriggerView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotTriggerDeleteParams {
    pub bot_id: BotId,
    pub trigger_id: BotTriggerId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotTriggerDeleteResponse {
    pub trigger: BotTriggerView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotEventAdmitParams {
    pub bot_id: BotId,
    pub event: BotEventInput,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotEventAdmitResponse {
    pub event: BotEventView,
    /// The event id was already stored; the existing row is returned and
    /// the controller is woken again.
    pub duplicate: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotEventReplayParams {
    pub bot_id: BotId,
    pub seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotEventReplayResponse {
    /// The fresh replay event, routed like the original.
    pub event: BotEventView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotEventListParams {
    pub bot_id: BotId,
    /// Page size; server-clamped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Opaque cursor from a previous page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotEventListResponse {
    /// Newest first.
    #[serde(default)]
    pub events: Vec<BotEventView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotEventReadParams {
    pub bot_id: BotId,
    pub seq: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotEventReadResponse {
    pub event: BotEventView,
    pub document: BotEventDocument,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotFilterTestParams {
    pub bot_id: BotId,
    /// CEL over `{event, data, headers}`.
    pub filter: String,
    /// A document to test instead of stored events: `{kind?, data?,
    /// headers?}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
    /// How many recent stored events to sample when no payload is given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotFilterTestResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    pub matched: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BotFilterTestResponse {
    pub sampled: u32,
    pub matched: u32,
    pub errors: u32,
    #[serde(default)]
    pub results: Vec<BotFilterTestResult>,
}
