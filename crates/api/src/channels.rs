//! Channels: chat provider accounts, pairings, and the core-runtime inbound
//! seam spoken by the connector host.
//!
//! A chat connection is a bot trigger of kind `chat` ([`BotTriggerSpec`]);
//! the records here are what that trigger points at (the universe's
//! provider account) and what pairing produced. Connectors — the TypeScript
//! bridges to Telegram and WhatsApp — normalize provider messages into
//! [`ChannelInbound`] and send them through `channels/inbound/admit`; they
//! receive delivery work back as Temporal activities on their own task
//! queue, whose payloads live in the workflow contract, not here.

use super::*;

// ── Identity ────────────────────────────────────────────────────────────────

/// Authored channel account id, unique per universe; same rules as
/// [`BotId`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChannelAccountId(String);

impl ChannelAccountId {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        Self::try_new(value).unwrap_or_else(|error| panic!("invalid ChannelAccountId: {error}"))
    }

    pub fn try_new(value: impl Into<String>) -> Result<Self, BotIdError> {
        let value = value.into();
        validate_bot_name("channel account id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ChannelAccountId {
    type Error = BotIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl FromStr for ChannelAccountId {
    type Err = BotIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_new(value)
    }
}

impl fmt::Display for ChannelAccountId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for ChannelAccountId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ChannelAccountId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_new(value).map_err(de::Error::custom)
    }
}

impl JsonSchema for ChannelAccountId {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ChannelAccountId".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        String::json_schema(generator)
    }
}

/// Chat provider name: an open, authored slug (`telegram`, `whatsapp`,
/// `slack`, …). The core never enumerates providers — it routes by this
/// name and derives connector queues from it; everything provider-specific
/// lives in the connector that serves the name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChannelProvider(String);

impl ChannelProvider {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        Self::try_new(value).unwrap_or_else(|error| panic!("invalid ChannelProvider: {error}"))
    }

    pub fn try_new(value: impl Into<String>) -> Result<Self, BotIdError> {
        let value = value.into();
        validate_bot_name("channel provider", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ChannelProvider {
    type Error = BotIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl FromStr for ChannelProvider {
    type Err = BotIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_new(value)
    }
}

impl fmt::Display for ChannelProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for ChannelProvider {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ChannelProvider {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_new(value).map_err(de::Error::custom)
    }
}

impl JsonSchema for ChannelProvider {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ChannelProvider".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        String::json_schema(generator)
    }
}

// ── Accounts ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelAccountSettings {
    /// WhatsApp: print the pairing QR code on the connector's terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub print_qr: Option<bool>,
    /// Provider-specific settings the core does not interpret; a new
    /// connector reads its own keys from here without a core change.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// A provider account served by the connector host. Secret material stays
/// in the referenced grant; this document is routing identity and
/// operational configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelAccountDocument {
    pub provider: ChannelProvider,
    /// Provider-native account identity: the Telegram bot username or
    /// numeric id, the WhatsApp phone number. Unique per universe and
    /// provider.
    pub provider_account_id: String,
    pub display_name: String,
    /// Retrievable auth grant holding the provider token (a Telegram bot
    /// token); the connector host leases it. Absent for providers whose
    /// credential is a session state directory (WhatsApp).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_grant_id: Option<String>,
    #[serde(default)]
    pub settings: ChannelAccountSettings,
    #[serde(default = "channel_default_true")]
    pub enabled: bool,
}

fn channel_default_true() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelAccountInput {
    pub account_id: ChannelAccountId,
    #[serde(flatten)]
    pub document: ChannelAccountDocument,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelAccountView {
    pub account_id: ChannelAccountId,
    pub revision: u64,
    #[serde(flatten)]
    pub document: ChannelAccountDocument,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelAccountCreateParams {
    pub account: ChannelAccountInput,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelAccountCreateResponse {
    pub account: ChannelAccountView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelAccountPutParams {
    pub account: ChannelAccountInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelAccountPutResponse {
    pub account: ChannelAccountView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelAccountReadParams {
    pub account_id: ChannelAccountId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelAccountReadResponse {
    pub account: ChannelAccountView,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelAccountListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ChannelProvider>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelAccountListResponse {
    #[serde(default)]
    pub accounts: Vec<ChannelAccountView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelAccountDeleteParams {
    pub account_id: ChannelAccountId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelAccountDeleteResponse {
    pub account: ChannelAccountView,
}

// ── Inbound ─────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ChannelMediaKind {
    Image,
    Audio,
    Document,
}

/// A provider-owned attachment reference; never bytes. The connector
/// downloads it when the conversation workflow asks (`prepare_channel_media`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelInboundMedia {
    /// Provider file handle (a Telegram file id, a sealed WhatsApp locator).
    pub file_id: String,
    pub kind: ChannelMediaKind,
    pub mime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_size: Option<u64>,
}

/// One normalized provider message, as the connector host hands it to the
/// core. Provider and account come from the admitting account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelInbound {
    pub message_id: String,
    pub chat_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// Provider handle of the sender (Telegram user id, WhatsApp JID).
    pub sender_id: String,
    pub sender_name: String,
    pub timestamp_ms: i64,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<ChannelInboundMedia>,
    pub is_direct: bool,
    pub mentioned_bot: bool,
    pub is_reply_to_bot: bool,
}

/// What the connector should do after admission. Pairing replies are the
/// connector's to send, in the provider's own voice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ChannelInboundDecision {
    /// The conversation is connected; the message went to its workflow.
    Bound,
    /// This message was the pairing code; the conversation is now
    /// connected and the message itself is consumed.
    Paired,
    /// A chat trigger would serve this conversation but it has not paired;
    /// the message looked addressed to the bot, so prompt for the code.
    PairingRequired,
    /// Same as above, but the message was ambient traffic; stay silent.
    PairingPending,
    /// No enabled chat trigger serves this account and scope.
    Unbound,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelInboundAdmitParams {
    pub account_id: ChannelAccountId,
    pub inbound: ChannelInbound,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelInboundAdmitResponse {
    pub decision: ChannelInboundDecision,
    /// The bot serving the conversation, when decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot_id: Option<BotId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_id: Option<BotTriggerId>,
}

// ── Pairings ────────────────────────────────────────────────────────────────

/// How a chat got its route: claimed by an open trigger's first contact,
/// or paired by code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ChannelPairedVia {
    Open,
    Code,
}

impl ChannelPairedVia {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Code => "code",
        }
    }
}

/// A conversation's route: the chat is identified by `(accountId, chatId)`
/// and owned by the paired trigger.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelPairingView {
    pub account_id: ChannelAccountId,
    pub chat_id: String,
    pub bot_id: BotId,
    pub trigger_id: BotTriggerId,
    pub paired_via: ChannelPairedVia,
    pub paired_at_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelPairingListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<ChannelAccountId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot_id: Option<BotId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelPairingListResponse {
    #[serde(default)]
    pub pairings: Vec<ChannelPairingView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelPairingDeleteParams {
    pub account_id: ChannelAccountId,
    pub chat_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelPairingDeleteResponse {
    pub pairing: ChannelPairingView,
}

// ── Conversations ───────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelConversationReadParams {
    pub account_id: ChannelAccountId,
    pub chat_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}

/// The conversation workflow's live snapshot, for debugging.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelConversationSnapshot {
    pub bot_id: String,
    pub trigger_id: String,
    pub label: String,
    pub inbound_count: u64,
    pub duplicate_inbound_count: u64,
    pub dropped_inbound_count: u64,
    pub denied_inbound_count: u64,
    pub emitted_count: u64,
    pub delivered_count: u64,
    pub failed_delivery_count: u64,
    pub active_deliveries: u32,
    pub typing: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protocol_errors: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelConversationReadResponse {
    /// Absent when no conversation workflow exists for the chat yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<ChannelConversationSnapshot>,
}
