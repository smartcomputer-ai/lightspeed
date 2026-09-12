//! Schedule and poll trigger fires. `BotTriggerFireWorkflow` runs one of
//! these per Schedule action; the trigger row is re-read at fire time, so a
//! stale Schedule can never admit stale configuration. Refusals — the
//! trigger is gone, something is disabled or closed, the breaker tripped,
//! the poll failed — are results the workflow records, not activity
//! failures; only runtime problems, and an environment still waking for an
//! exec poll, raise a retryable activity error.

use std::time::{Duration, Instant};

use api::AgentApiService as _;
use api::{
    AgentApiError, AgentApiErrorKind, AuthGrantLeaseParams, BotEventDocument,
    BotTriggerDisabledReason, BotTriggerId, BotTriggerKind, BotTriggerSpec,
    EnvironmentJobCancelParams, EnvironmentJobCreateParams, EnvironmentJobReadParams,
    PollCursorSpec, PollCursorState, PollHttpAuth, PollHttpMethod, PollSource, ProfileEnvironment,
    ProfileId, ProfileReadParams, SessionJobCancelScopeView, SessionJobHandleInput,
    SessionJobOutputStreamView, SessionJobStartSpecInput, SessionJobStatusView,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use bots::{
    BotError, BotRecord, BotRefusalCode, BotStore, BotTriggerRecord, BotTriggerStore,
    ids::{poll_event_id, schedule_event_id, sha256_hex},
    poll::{
        MAX_POLL_CONSECUTIVE_FAILURES, PollItem, diff_poll_items, extract_poll_items,
        parse_poll_payload, poll_item_summary,
    },
};
use serde_json::{Map, Value, json};
use temporal_workflow::bots::*;
use temporalio_sdk::activities::ActivityError;

use super::{
    admission::{AdmitTriggerOutcome, StoreBotEventInput},
    now_ms,
};
use crate::gateway::GatewayAgentApi;

/// Wall-clock budget of one HTTP poll request, body included.
pub const HTTP_POLL_TIMEOUT: Duration = Duration::from_secs(30);
/// Largest payload a poll source may hand back, HTTP body or job stdout.
pub const MAX_POLL_PAYLOAD_BYTES: usize = 1024 * 1024;
/// Job budget of an exec poll without a `timeoutMs`.
pub const EXEC_DEFAULT_TIMEOUT_MS: u64 = 60_000;
/// How often the fire reads an exec poll's job while it runs.
pub const EXEC_READ_INTERVAL: Duration = Duration::from_secs(2);
/// Slack past the job budget before the fire cancels the job itself.
pub const EXEC_DEADLINE_SLACK_MS: u64 = 30_000;
/// Stderr kept while an exec poll runs, and how much of it a failure
/// message shows.
const STDERR_TAIL_BYTES: usize = 2_000;
const STDERR_REPORT_CHARS: usize = 500;
/// Length of the digest in an exec poll's retry-stable job request id.
const EXEC_REQUEST_ID_DIGEST_LEN: usize = 24;

fn retryable(error: impl Into<anyhow::Error>) -> ActivityError {
    ActivityError::from(error.into())
}

// ── Refusals ────────────────────────────────────────────────────────────────

/// Why a fire delivers nothing. The reason string is what the workflow
/// records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FireRefusal {
    /// Bot or trigger gone, or the trigger is not of the fired kind.
    TriggerMissing,
    TriggerDisabled,
    BotDisabled,
    BotClosed,
    BreakerTripped,
    /// The poll source could not be fetched or parsed; the streak counted.
    PollFailed,
    /// The streak reached its cap and the trigger disabled itself.
    PollDisabled,
}

impl FireRefusal {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Self::TriggerMissing => "trigger_missing",
            Self::TriggerDisabled => "trigger_disabled",
            Self::BotDisabled => "bot_disabled",
            Self::BotClosed => "bot_closed",
            Self::BreakerTripped => "breaker_tripped",
            Self::PollFailed => "poll_failed",
            Self::PollDisabled => "poll_disabled",
        }
    }
}

/// The row-level checks every fire makes before touching anything: the
/// trigger belongs to the bot with the fired kind, and nothing is closed
/// or disabled. Closed is terminal and reads first.
pub(crate) fn fire_refusal(
    bot: &BotRecord,
    trigger: &BotTriggerRecord,
    kind: BotTriggerKind,
) -> Option<FireRefusal> {
    if trigger.bot_id != bot.bot_id || trigger.kind() != kind {
        return Some(FireRefusal::TriggerMissing);
    }
    if bot.is_closed() {
        Some(FireRefusal::BotClosed)
    } else if !trigger.enabled() {
        Some(FireRefusal::TriggerDisabled)
    } else if !bot.document.enabled {
        Some(FireRefusal::BotDisabled)
    } else {
        None
    }
}

/// Load the bot and trigger a fire names and run the shared refusals,
/// the flood breaker included (a tripped breaker disables the trigger and
/// drops its Schedule on the way).
async fn load_fire_target(
    api: &GatewayAgentApi,
    request: &BotTriggerFireRequest,
    kind: BotTriggerKind,
) -> Result<Result<(BotRecord, BotTriggerRecord), FireRefusal>, ActivityError> {
    let store = api.store();
    let bot = match store.read_bot(&request.bot_id).await {
        Ok(bot) => bot,
        Err(BotError::BotNotFound { .. }) => return Ok(Err(FireRefusal::TriggerMissing)),
        Err(error) => return Err(retryable(error)),
    };
    let trigger = match store
        .read_bot_trigger(&request.bot_id, &request.trigger_id)
        .await
    {
        Ok(trigger) => trigger,
        Err(BotError::TriggerNotFound { .. }) => return Ok(Err(FireRefusal::TriggerMissing)),
        Err(error) => return Err(retryable(error)),
    };
    if let Some(refusal) = fire_refusal(&bot, &trigger, kind) {
        return Ok(Err(refusal));
    }
    match api.check_trigger_breaker(&bot, &trigger).await {
        Ok(()) => Ok(Ok((bot, trigger))),
        Err(BotError::Refused {
            code: BotRefusalCode::BreakerTripped,
            ..
        }) => Ok(Err(FireRefusal::BreakerTripped)),
        Err(error) => Err(retryable(error)),
    }
}

/// Best-effort removal of a trigger's Schedule once the row says it must
/// not fire again; the reconciliation sweep converges anything missed.
async fn drop_schedule(api: &GatewayAgentApi, bot: &BotRecord, trigger: &BotTriggerRecord) {
    if let Err(error) = api
        .delete_bot_trigger_schedule(&bot.bot_id, &trigger.trigger_id)
        .await
    {
        tracing::warn!(
            target: "temporal_server",
            bot_id = %bot.bot_id,
            trigger_id = %trigger.trigger_id,
            %error,
            "delete trigger schedule after disable failed; reconciliation will retry"
        );
    }
}

// ── Schedule fires ──────────────────────────────────────────────────────────

/// The envelope of one schedule fire. The prompt carries everything the
/// session needs in a few lines; the machine data keeps only what filters
/// and replay can use.
pub(crate) fn schedule_event_document(
    trigger_id: &BotTriggerId,
    cron: Option<&str>,
    at_ms: Option<i64>,
    timezone: &str,
    summary: &str,
    scheduled_at_ms: i64,
) -> BotEventDocument {
    let mut data = Map::new();
    data.insert("trigger".to_owned(), json!(trigger_id));
    if let Some(cron) = cron {
        data.insert("cron".to_owned(), json!(cron));
    }
    if let Some(at_ms) = at_ms {
        data.insert("atMs".to_owned(), json!(at_ms));
    }
    data.insert("timezone".to_owned(), json!(timezone));
    data.insert("scheduledAtMs".to_owned(), json!(scheduled_at_ms));
    let summary = summary.trim();
    let summary = if summary.is_empty() {
        format!("scheduled trigger {trigger_id} fired")
    } else {
        summary.to_owned()
    };
    BotEventDocument {
        version: BotEventDocument::VERSION,
        kind: "schedule".to_owned(),
        source: format!("schedule:{trigger_id}"),
        occurred_at_ms: scheduled_at_ms,
        summary,
        data: Some(Value::Object(data)),
        headers: Default::default(),
        correlation_id: None,
        links: Vec::new(),
        sender: None,
        hops: 0,
        in_reply_to: None,
    }
}

pub async fn admit_schedule_event(
    api: &GatewayAgentApi,
    request: BotTriggerFireRequest,
) -> Result<BotScheduleFireResult, ActivityError> {
    let refused = |refusal: FireRefusal| BotScheduleFireResult::Refused {
        reason: refusal.reason().to_owned(),
    };
    let (bot, trigger) = match load_fire_target(api, &request, BotTriggerKind::Schedule).await? {
        Ok(target) => target,
        Err(refusal) => return Ok(refused(refusal)),
    };
    let BotTriggerSpec::Schedule {
        cron,
        at_ms,
        timezone,
        summary,
    } = &trigger.document.spec
    else {
        return Ok(refused(FireRefusal::TriggerMissing));
    };
    let one_shot = at_ms.is_some();
    let document = schedule_event_document(
        &trigger.trigger_id,
        cron.as_deref(),
        *at_ms,
        timezone,
        summary,
        request.scheduled_at_ms,
    );
    // A retried fire reuses the stored row's identity so #N stays stable.
    // Schedule triggers carry no filter, route, or coalescing: the event
    // goes to the main session as is.
    let event_id = schedule_event_id(&trigger.trigger_id, request.scheduled_at_ms);
    let mut input = StoreBotEventInput::new(event_id.clone(), document);
    input.trigger_id = Some(trigger.trigger_id.clone());
    let stored = match api.store_bot_event(&bot, input).await {
        Ok(stored) => stored,
        Err(BotError::Refused {
            code: BotRefusalCode::BotClosed,
            ..
        }) => return Ok(refused(FireRefusal::BotClosed)),
        Err(error) => return Err(retryable(error)),
    };
    if one_shot {
        // It has fired: the trigger reads as spent and cannot fire again.
        api.store()
            .disable_bot_trigger(
                &bot.bot_id,
                &trigger.trigger_id,
                BotTriggerDisabledReason::OneShot,
                now_ms(),
            )
            .await
            .map_err(retryable)?;
        drop_schedule(api, &bot, &trigger).await;
    }
    Ok(BotScheduleFireResult::Admitted {
        event_id,
        duplicate: stored.duplicate,
    })
}

// ── Poll fires ──────────────────────────────────────────────────────────────

/// Why a poll source produced no payload.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PollFetchError {
    /// The exec environment is waking; the activity retry absorbs it and
    /// the streak does not count it.
    EnvironmentNotReady(String),
    /// A failed fire: counted on the cursor.
    Failed(String),
}

/// The cursor after a failed fire — the streak incremented, everything
/// else kept — and whether the trigger crossed the disable threshold.
pub(crate) fn poll_failure_state(
    state: Option<&PollCursorState>,
    now_ms: i64,
) -> (PollCursorState, bool) {
    let mut next = state.cloned().unwrap_or_default();
    next.consecutive_failures = next.consecutive_failures.saturating_add(1);
    next.last_polled_at_ms = Some(now_ms);
    let disable = next.consecutive_failures >= MAX_POLL_CONSECUTIVE_FAILURES;
    (next, disable)
}

/// The credential header of an HTTP poll: `auth.header` (default
/// `authorization`) carrying the leased token under `auth.scheme` (default
/// `Bearer`; an empty scheme sends the token raw).
pub(crate) fn credential_header(auth: &PollHttpAuth, token: &str) -> (String, String) {
    let name = auth
        .header
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("authorization")
        .to_owned();
    (name, credential_header_value(token, auth.scheme.as_deref()))
}

pub(crate) fn credential_header_value(token: &str, scheme: Option<&str>) -> String {
    match scheme.map(str::trim) {
        None => format!("Bearer {token}"),
        Some("") => token.to_owned(),
        Some(scheme) => format!("{scheme} {token}"),
    }
}

/// The occurrence time of one polled item: a watermark cursor's field
/// when it is an ISO-8601 instant, otherwise the fire's nominal time.
pub(crate) fn item_occurred_at_ms(item: &Value, cursor: &PollCursorSpec, fallback_ms: i64) -> i64 {
    if let PollCursorSpec::Watermark { field } = cursor
        && let Some(Value::String(value)) = item.get(field)
        && let Ok(instant) = chrono::DateTime::parse_from_rfc3339(value)
    {
        return instant.timestamp_millis();
    }
    fallback_ms
}

/// The envelope of one newly seen poll item.
pub(crate) fn poll_item_document(
    trigger_id: &BotTriggerId,
    entry: &PollItem,
    cursor: &PollCursorSpec,
    fallback_ms: i64,
) -> BotEventDocument {
    BotEventDocument {
        version: BotEventDocument::VERSION,
        kind: "poll.item".to_owned(),
        source: format!("poll:{trigger_id}"),
        occurred_at_ms: item_occurred_at_ms(&entry.item, cursor, fallback_ms),
        summary: poll_item_summary(&entry.item, &entry.key),
        data: Some(entry.item.clone()),
        headers: Default::default(),
        correlation_id: None,
        links: Vec::new(),
        sender: None,
        hops: 0,
        in_reply_to: None,
    }
}

/// Retry-stable job request id of an exec poll: retried fires of one
/// nominal time converge on one job.
pub(crate) fn exec_request_id(trigger_id: &BotTriggerId, scheduled_at_ms: i64) -> String {
    let digest = sha256_hex(format!("{trigger_id}:{scheduled_at_ms}"));
    format!("poll-{}", &digest[..EXEC_REQUEST_ID_DIGEST_LEN])
}

/// Whether a job status is final.
pub(crate) fn is_terminal_job_status(status: SessionJobStatusView) -> bool {
    !matches!(
        status,
        SessionJobStatusView::Accepted
            | SessionJobStatusView::Queued
            | SessionJobStatusView::Running
            | SessionJobStatusView::CancelRequested
    )
}

/// The wire name of a job status (`timedOut`, `failed`, …).
pub(crate) fn job_status_label(status: SessionJobStatusView) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{status:?}"))
}

/// Append to a bounded tail buffer, keeping its last `cap` bytes.
pub(crate) fn append_tail(tail: &mut Vec<u8>, data: &[u8], cap: usize) {
    tail.extend_from_slice(data);
    let excess = tail.len().saturating_sub(cap);
    if excess > 0 {
        tail.drain(..excess);
    }
}

/// The last few hundred characters of stderr as a failure suffix.
fn stderr_suffix(tail: &[u8]) -> String {
    let text = String::from_utf8_lossy(tail);
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    let chars = text.chars().count();
    let shown: String = text
        .chars()
        .skip(chars.saturating_sub(STDERR_REPORT_CHARS))
        .collect();
    format!(": {shown}")
}

pub async fn poll_trigger(
    api: &GatewayAgentApi,
    request: BotTriggerFireRequest,
) -> Result<BotPollFireResult, ActivityError> {
    let refused = |refusal: FireRefusal| BotPollFireResult::Refused {
        reason: refusal.reason().to_owned(),
    };
    let (bot, trigger) = match load_fire_target(api, &request, BotTriggerKind::Poll).await? {
        Ok(target) => target,
        Err(refusal) => return Ok(refused(refusal)),
    };
    let BotTriggerSpec::Poll {
        source,
        items: items_path,
        cursor,
        ..
    } = &trigger.document.spec
    else {
        return Ok(refused(FireRefusal::TriggerMissing));
    };

    let payload =
        match fetch_poll_payload(api, &bot, &trigger, source, request.scheduled_at_ms).await {
            Ok(payload) => payload,
            Err(PollFetchError::EnvironmentNotReady(message)) => {
                // A sleeping environment is not a failure: the resolver has
                // begun the wake; the activity retry absorbs the latency.
                return Err(retryable(anyhow::anyhow!(
                    "poll trigger {} waits for its environment: {message}",
                    trigger.trigger_id
                )));
            }
            Err(PollFetchError::Failed(message)) => {
                return note_poll_failure(api, &bot, &trigger, &message).await;
            }
        };
    let items = match extract_poll_items(&payload, items_path.as_deref()) {
        Ok(items) => items,
        Err(message) => return note_poll_failure(api, &bot, &trigger, &message).await,
    };
    let store = api.store();
    let diff = diff_poll_items(&items, cursor, trigger.cursor.as_ref(), now_ms());
    if diff.baselined {
        // First contact: the cursor initializes from the current payload
        // and nothing delivers — a deep history must not flood the bot.
        store
            .set_bot_trigger_cursor(&bot.bot_id, &trigger.trigger_id, Some(diff.next_state))
            .await
            .map_err(retryable)?;
        tracing::info!(
            target: "temporal_server",
            bot_id = %bot.bot_id,
            trigger_id = %trigger.trigger_id,
            items = items.len(),
            "poll trigger baselined"
        );
        return Ok(BotPollFireResult::Polled {
            baselined: true,
            admitted: 0,
            filtered: 0,
        });
    }

    let mut admitted = 0;
    let mut filtered = 0;
    for entry in &diff.new_items {
        // Retried fires and overlapping polls converge on one row per item.
        let event_id = poll_event_id(&trigger.trigger_id, &entry.key);
        let document =
            poll_item_document(&trigger.trigger_id, entry, cursor, request.scheduled_at_ms);
        let input = StoreBotEventInput::new(event_id, document);
        match api.admit_trigger_event(&bot, &trigger, input).await {
            Ok(AdmitTriggerOutcome::Admitted(_)) => admitted += 1,
            // Filtered items advance the cursor but are deliberately not
            // stored: feeds where most items filter out would bury the
            // envelope store. The per-fire count keeps it observable.
            Ok(AdmitTriggerOutcome::Filtered { .. }) => filtered += 1,
            Err(BotError::Refused {
                code: BotRefusalCode::BotClosed,
                ..
            }) => return Ok(refused(FireRefusal::BotClosed)),
            Err(error) => return Err(retryable(error)),
        }
    }
    store
        .set_bot_trigger_cursor(&bot.bot_id, &trigger.trigger_id, Some(diff.next_state))
        .await
        .map_err(retryable)?;
    Ok(BotPollFireResult::Polled {
        baselined: false,
        admitted,
        filtered,
    })
}

/// Count the failed fire on the cursor and, past the streak cap, disable
/// the trigger and drop its Schedule for a human to look at.
async fn note_poll_failure(
    api: &GatewayAgentApi,
    bot: &BotRecord,
    trigger: &BotTriggerRecord,
    message: &str,
) -> Result<BotPollFireResult, ActivityError> {
    let now = now_ms();
    let (state, disable) = poll_failure_state(trigger.cursor.as_ref(), now);
    tracing::warn!(
        target: "temporal_server",
        bot_id = %bot.bot_id,
        trigger_id = %trigger.trigger_id,
        consecutive_failures = state.consecutive_failures,
        disable,
        message = %message,
        "poll fire failed"
    );
    let store = api.store();
    store
        .set_bot_trigger_cursor(&bot.bot_id, &trigger.trigger_id, Some(state))
        .await
        .map_err(retryable)?;
    if !disable {
        return Ok(BotPollFireResult::Refused {
            reason: FireRefusal::PollFailed.reason().to_owned(),
        });
    }
    store
        .disable_bot_trigger(
            &bot.bot_id,
            &trigger.trigger_id,
            BotTriggerDisabledReason::PollFailed,
            now,
        )
        .await
        .map_err(retryable)?;
    drop_schedule(api, bot, trigger).await;
    Ok(BotPollFireResult::Refused {
        reason: FireRefusal::PollDisabled.reason().to_owned(),
    })
}

async fn fetch_poll_payload(
    api: &GatewayAgentApi,
    bot: &BotRecord,
    trigger: &BotTriggerRecord,
    source: &PollSource,
    scheduled_at_ms: i64,
) -> Result<Value, PollFetchError> {
    match source {
        PollSource::Http {
            url,
            method,
            headers,
            auth,
            body,
        } => fetch_http_payload(api, url, method, headers, auth.as_ref(), body.as_deref()).await,
        PollSource::Exec {
            environment_id,
            argv,
            cwd,
            timeout_ms,
        } => {
            fetch_exec_payload(
                api,
                &bot.document.profile_id,
                &trigger.trigger_id,
                scheduled_at_ms,
                environment_id.as_deref(),
                argv,
                cwd.as_deref(),
                *timeout_ms,
            )
            .await
        }
    }
}

// ── HTTP sources ────────────────────────────────────────────────────────────

/// Lease the poll's credential in-process through the auth broker; the
/// token never enters the trigger document or the event.
async fn lease_poll_credential(
    api: &GatewayAgentApi,
    auth: &PollHttpAuth,
) -> Result<String, PollFetchError> {
    api.lease_auth_grant(AuthGrantLeaseParams {
        grant_id: auth.grant_id.clone(),
        audience: auth.audience.clone(),
    })
    .await
    .map(|outcome| outcome.result.token)
    .map_err(|error| {
        PollFetchError::Failed(format!(
            "lease credential {}: {}",
            auth.grant_id, error.message
        ))
    })
}

fn payload_too_large(label: &str) -> PollFetchError {
    PollFetchError::Failed(format!(
        "poll {label} exceeds {MAX_POLL_PAYLOAD_BYTES} bytes"
    ))
}

/// Read a response body up to the payload cap.
async fn read_capped_body(mut response: reqwest::Response) -> Result<Vec<u8>, PollFetchError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_POLL_PAYLOAD_BYTES as u64)
    {
        return Err(payload_too_large("response body"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| PollFetchError::Failed(format!("read poll response: {error}")))?
    {
        if bytes.len() + chunk.len() > MAX_POLL_PAYLOAD_BYTES {
            return Err(payload_too_large("response body"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Fetch the HTTP source with a wall-clock timeout and a size cap. The
/// non-secret headers ride on the request as authored (credential headers
/// were refused at put time); the leased credential is added last. One
/// retry with a fresh lease after a 401/403 covers a token that expired
/// between lease and use.
async fn fetch_http_payload(
    api: &GatewayAgentApi,
    url: &str,
    method: &PollHttpMethod,
    headers: &std::collections::BTreeMap<String, String>,
    auth: Option<&PollHttpAuth>,
    body: Option<&str>,
) -> Result<Value, PollFetchError> {
    let client = reqwest::Client::builder()
        .timeout(HTTP_POLL_TIMEOUT)
        .build()
        .map_err(|error| PollFetchError::Failed(format!("build HTTP client: {error}")))?;
    let mut retried = false;
    loop {
        let mut request = match method {
            PollHttpMethod::Get => client.get(url),
            PollHttpMethod::Post => client.post(url),
        };
        for (name, value) in headers {
            request = request.header(name.as_str(), value.as_str());
        }
        if let Some(auth) = auth {
            let token = lease_poll_credential(api, auth).await?;
            let (name, value) = credential_header(auth, &token);
            request = request.header(name.as_str(), value.as_str());
        }
        if let Some(body) = body {
            request = request.body(body.to_owned());
        }
        let response = request
            .send()
            .await
            .map_err(|error| PollFetchError::Failed(format!("poll request failed: {error}")))?;
        let status = response.status();
        if auth.is_some()
            && !retried
            && matches!(
                status,
                reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
            )
        {
            retried = true;
            continue;
        }
        if !status.is_success() {
            return Err(PollFetchError::Failed(format!(
                "poll source responded {status}"
            )));
        }
        let bytes = read_capped_body(response).await?;
        return parse_poll_payload(&bytes, "response body").map_err(PollFetchError::Failed);
    }
}

// ── Exec sources ────────────────────────────────────────────────────────────

/// The environment an exec poll without an explicit `environmentId` runs
/// in: the `existing` environment of the bot's profile. A profile with
/// another intent (none or inherit) cannot run such a
/// poll — a configuration error, not a transient failure.
async fn resolve_bot_profile_environment(
    api: &GatewayAgentApi,
    profile_id: &ProfileId,
) -> Result<String, PollFetchError> {
    let profile = api
        .read_profile(ProfileReadParams {
            profile_id: profile_id.clone(),
        })
        .await
        .map_err(|error| {
            PollFetchError::Failed(format!("read profile {profile_id}: {}", error.message))
        })?
        .result
        .profile;
    match profile.document.environment {
        Some(ProfileEnvironment::Existing { environment_id }) => Ok(environment_id),
        _ => Err(PollFetchError::Failed(format!(
            "the poll names no environment and profile {profile_id} does not activate an existing one: set environmentId on the trigger, or point the profile at an existing environment"
        ))),
    }
}

/// A job API error: a waking environment propagates for the activity
/// retry; anything else is a failed fire.
fn map_job_api_error(context: &str, error: AgentApiError) -> PollFetchError {
    if error.kind == AgentApiErrorKind::EnvironmentNotReady {
        PollFetchError::EnvironmentNotReady(error.message)
    } else {
        PollFetchError::Failed(format!("{context}: {}", error.message))
    }
}

async fn cancel_job(api: &GatewayAgentApi, handle: &SessionJobHandleInput) {
    let _ = api
        .cancel_environment_jobs(EnvironmentJobCancelParams {
            jobs: vec![handle.clone()],
            scope: SessionJobCancelScopeView::Job,
            force: false,
        })
        .await;
}

/// Run the command as a one-shot environment job and parse its stdout as
/// JSON, reading output every [`EXEC_READ_INTERVAL`] and cancelling the
/// job once the budget plus slack has passed.
#[allow(clippy::too_many_arguments)]
async fn fetch_exec_payload(
    api: &GatewayAgentApi,
    profile_id: &ProfileId,
    trigger_id: &BotTriggerId,
    scheduled_at_ms: i64,
    environment_id: Option<&str>,
    argv: &[String],
    cwd: Option<&str>,
    timeout_ms: Option<u64>,
) -> Result<Value, PollFetchError> {
    let environment_id = match environment_id {
        Some(environment_id) => environment_id.to_owned(),
        None => resolve_bot_profile_environment(api, profile_id).await?,
    };
    let budget_ms = timeout_ms.unwrap_or(EXEC_DEFAULT_TIMEOUT_MS);
    let created = api
        .create_environment_jobs(EnvironmentJobCreateParams {
            environment_id,
            request_id: exec_request_id(trigger_id, scheduled_at_ms),
            jobs: vec![SessionJobStartSpecInput {
                name: Some("poll".to_owned()),
                job_id: None,
                argv: argv.to_vec(),
                cwd: cwd.map(str::to_owned),
                env: Default::default(),
                stdin: None,
                timeout_ms: Some(budget_ms),
                depends_on: Vec::new(),
                dependency_policy: Default::default(),
                queue_key: None,
            }],
        })
        .await
        .map_err(|error| map_job_api_error("start poll command", error))?;
    let started = created.result.jobs.into_iter().next().ok_or_else(|| {
        PollFetchError::Failed("environment job start returned no job".to_owned())
    })?;
    let handle = SessionJobHandleInput {
        environment_id: started.handle.environment_id,
        job_id: started.job_id,
    };
    let deadline =
        Instant::now() + Duration::from_millis(budget_ms.saturating_add(EXEC_DEADLINE_SLACK_MS));
    let mut after_seq = None;
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr_tail: Vec<u8> = Vec::new();
    loop {
        let read = api
            .read_environment_jobs(EnvironmentJobReadParams {
                jobs: vec![handle.clone()],
                output_bytes: None,
                after_seq,
                include_artifacts: false,
            })
            .await
            .map_err(|error| map_job_api_error("read poll command", error))?;
        let entry = read.result.jobs.into_iter().next().ok_or_else(|| {
            PollFetchError::Failed("environment job read returned no entry".to_owned())
        })?;
        if let Some(error) = entry.error {
            return Err(PollFetchError::Failed(format!(
                "environment job read failed: {error}"
            )));
        }
        for chunk in entry.output_chunks {
            let data = BASE64
                .decode(chunk.data_base64.as_bytes())
                .map_err(|error| {
                    PollFetchError::Failed(format!("decode poll command output: {error}"))
                })?;
            match chunk.stream {
                SessionJobOutputStreamView::Stdout => stdout.extend_from_slice(&data),
                SessionJobOutputStreamView::Stderr => {
                    append_tail(&mut stderr_tail, &data, STDERR_TAIL_BYTES)
                }
            }
        }
        if stdout.len() > MAX_POLL_PAYLOAD_BYTES {
            cancel_job(api, &handle).await;
            return Err(payload_too_large("stdout"));
        }
        after_seq = Some(entry.output_next_seq);
        if let Some(summary) = entry.summary
            && is_terminal_job_status(summary.status)
        {
            return match summary.status {
                SessionJobStatusView::Succeeded => {
                    parse_poll_payload(&stdout, "stdout").map_err(PollFetchError::Failed)
                }
                other => Err(PollFetchError::Failed(format!(
                    "poll command ended {}{}",
                    job_status_label(other),
                    stderr_suffix(&stderr_tail)
                ))),
            };
        }
        if Instant::now() > deadline {
            cancel_job(api, &handle).await;
            return Err(PollFetchError::Failed(format!(
                "poll command did not finish within {budget_ms} ms"
            )));
        }
        tokio::time::sleep(EXEC_READ_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::{BotDocument, BotId, BotTriggerDocument, ProfileId};

    const T0: i64 = 1_788_091_200_000; // 2026-08-30T12:00:00Z

    fn bot(enabled: bool, closed: bool) -> BotRecord {
        BotRecord {
            bot_id: BotId::new("triage"),
            revision: 1,
            document: BotDocument {
                display_name: None,
                description: None,
                profile_id: ProfileId::new("p"),
                brief: None,
                runs_per_day: None,
                breaker: None,
                routed_session_close_after_ms: None,
                self_config: false,
                emit: false,
                enabled,
            },
            event_seq: 0,
            closed_at_ms: closed.then_some(T0),
            closed_sessions: Vec::new(),
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    fn trigger(spec: BotTriggerSpec, enabled: bool) -> BotTriggerRecord {
        BotTriggerRecord {
            bot_id: BotId::new("triage"),
            trigger_id: BotTriggerId::new("nightly"),
            revision: 1,
            document: BotTriggerDocument {
                spec,
                filter: None,
                route: None,
                coalesce: None,
                deliver: None,
                session_close_after_ms: None,
                enabled,
            },
            secrets: Default::default(),
            disabled_reason: None,
            disabled_at_ms: None,
            last_filter_error: None,
            last_filter_error_at_ms: None,
            cursor: None,
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    fn schedule_spec() -> BotTriggerSpec {
        BotTriggerSpec::Schedule {
            cron: Some("0 3 * * *".to_owned()),
            at_ms: None,
            timezone: "UTC".to_owned(),
            summary: "Triage overnight".to_owned(),
        }
    }

    fn poll_spec() -> BotTriggerSpec {
        BotTriggerSpec::Poll {
            source: PollSource::Exec {
                environment_id: None,
                argv: vec!["ls".to_owned()],
                cwd: None,
                timeout_ms: None,
            },
            interval_ms: 60_000,
            items: None,
            cursor: PollCursorSpec::IdSet {
                id: "id".to_owned(),
            },
        }
    }

    #[test]
    fn refusals_read_closed_first_then_the_trigger_then_the_bot() {
        let schedule = BotTriggerKind::Schedule;
        assert_eq!(
            fire_refusal(&bot(true, false), &trigger(schedule_spec(), true), schedule),
            None
        );
        assert_eq!(
            fire_refusal(
                &bot(false, true),
                &trigger(schedule_spec(), false),
                schedule
            ),
            Some(FireRefusal::BotClosed)
        );
        assert_eq!(
            fire_refusal(
                &bot(false, false),
                &trigger(schedule_spec(), false),
                schedule
            ),
            Some(FireRefusal::TriggerDisabled)
        );
        assert_eq!(
            fire_refusal(
                &bot(false, false),
                &trigger(schedule_spec(), true),
                schedule
            ),
            Some(FireRefusal::BotDisabled)
        );
        // A poll fire against a schedule trigger (or a trigger of another
        // bot) is a missing trigger, whatever its state.
        assert_eq!(
            fire_refusal(
                &bot(true, false),
                &trigger(schedule_spec(), true),
                BotTriggerKind::Poll
            ),
            Some(FireRefusal::TriggerMissing)
        );
        let mut foreign = trigger(poll_spec(), true);
        foreign.bot_id = BotId::new("other");
        assert_eq!(
            fire_refusal(&bot(true, false), &foreign, BotTriggerKind::Poll),
            Some(FireRefusal::TriggerMissing)
        );
        assert_eq!(FireRefusal::TriggerMissing.reason(), "trigger_missing");
        assert_eq!(FireRefusal::BreakerTripped.reason(), "breaker_tripped");
        assert_eq!(FireRefusal::PollFailed.reason(), "poll_failed");
        assert_eq!(FireRefusal::PollDisabled.reason(), "poll_disabled");
    }

    #[test]
    fn schedule_documents_carry_the_spec_and_the_nominal_time() {
        let trigger_id = BotTriggerId::new("nightly");
        let cron = schedule_event_document(
            &trigger_id,
            Some("0 3 * * *"),
            None,
            "Europe/Berlin",
            "Triage overnight",
            T0,
        );
        assert_eq!(cron.version, BotEventDocument::VERSION);
        assert_eq!(cron.kind, "schedule");
        assert_eq!(cron.source, "schedule:nightly");
        assert_eq!(cron.occurred_at_ms, T0);
        assert_eq!(cron.summary, "Triage overnight");
        assert_eq!(
            cron.data,
            Some(json!({
                "trigger": "nightly",
                "cron": "0 3 * * *",
                "timezone": "Europe/Berlin",
                "scheduledAtMs": T0,
            }))
        );
        assert!(cron.sender.is_none());
        assert_eq!(cron.hops, 0);

        let one_shot = schedule_event_document(&trigger_id, None, Some(T0), "UTC", "  ", T0 + 5);
        let data = one_shot.data.unwrap();
        assert_eq!(data["atMs"], json!(T0));
        assert!(data.get("cron").is_none());
        assert_eq!(data["scheduledAtMs"], json!(T0 + 5));
        assert_eq!(one_shot.summary, "scheduled trigger nightly fired");
    }

    #[test]
    fn poll_failures_count_up_and_disable_at_the_cap() {
        let (first, disable) = poll_failure_state(None, T0);
        assert_eq!(first.consecutive_failures, 1);
        assert_eq!(first.last_polled_at_ms, Some(T0));
        assert!(!disable);

        let state = PollCursorState {
            ids: vec!["a".to_owned()],
            watermark: Some(json!(7)),
            consecutive_failures: MAX_POLL_CONSECUTIVE_FAILURES - 2,
            baselined_at_ms: Some(T0 - 1),
            last_polled_at_ms: Some(T0 - 1),
        };
        let (next, disable) = poll_failure_state(Some(&state), T0);
        assert_eq!(next.consecutive_failures, MAX_POLL_CONSECUTIVE_FAILURES - 1);
        assert!(!disable);
        // The cursor itself survives a failed fire.
        assert_eq!(next.ids, vec!["a".to_owned()]);
        assert_eq!(next.watermark, Some(json!(7)));
        assert_eq!(next.baselined_at_ms, Some(T0 - 1));
        assert_eq!(next.last_polled_at_ms, Some(T0));

        let (last, disable) = poll_failure_state(Some(&next), T0 + 1);
        assert_eq!(last.consecutive_failures, MAX_POLL_CONSECUTIVE_FAILURES);
        assert!(disable);
        let (past, disable) = poll_failure_state(Some(&last), T0 + 2);
        assert!(disable);
        assert_eq!(past.consecutive_failures, MAX_POLL_CONSECUTIVE_FAILURES + 1);
    }

    #[test]
    fn credential_headers_default_to_a_bearer_authorization() {
        let auth = |header: Option<&str>, scheme: Option<&str>| PollHttpAuth {
            grant_id: "grant_1".to_owned(),
            header: header.map(str::to_owned),
            scheme: scheme.map(str::to_owned),
            audience: None,
        };
        assert_eq!(
            credential_header(&auth(None, None), "tok"),
            ("authorization".to_owned(), "Bearer tok".to_owned())
        );
        assert_eq!(
            credential_header(&auth(Some("x-api-key"), Some("")), "tok"),
            ("x-api-key".to_owned(), "tok".to_owned())
        );
        assert_eq!(
            credential_header(&auth(Some(" x-token "), Some(" Token ")), "tok"),
            ("x-token".to_owned(), "Token tok".to_owned())
        );
        assert_eq!(
            credential_header(&auth(Some(""), None), "tok").0,
            "authorization"
        );
        assert_eq!(credential_header_value("tok", None), "Bearer tok");
        assert_eq!(credential_header_value("tok", Some("   ")), "tok");
        assert_eq!(credential_header_value("tok", Some("Basic")), "Basic tok");
    }

    #[test]
    fn poll_item_documents_prefer_the_watermark_instant() {
        let trigger_id = BotTriggerId::new("issues");
        let watermark = PollCursorSpec::Watermark {
            field: "updatedAt".to_owned(),
        };
        let entry = PollItem {
            key: "2026-08-30T12:00:00Z".to_owned(),
            item: json!({ "title": "Broken build", "updatedAt": "2026-08-30T12:00:00Z" }),
        };
        let document = poll_item_document(&trigger_id, &entry, &watermark, T0 + 99);
        assert_eq!(document.kind, "poll.item");
        assert_eq!(document.source, "poll:issues");
        assert_eq!(document.occurred_at_ms, T0);
        assert_eq!(document.summary, "Broken build");
        assert_eq!(document.data, Some(entry.item.clone()));
        assert!(document.sender.is_none());

        // A non-instant watermark, an id-set cursor, or a scalar item fall
        // back to the fire's nominal time.
        assert_eq!(
            item_occurred_at_ms(&json!({ "updatedAt": 5 }), &watermark, T0 + 1),
            T0 + 1
        );
        assert_eq!(
            item_occurred_at_ms(&json!({ "updatedAt": "yesterday" }), &watermark, T0 + 1),
            T0 + 1
        );
        let id_set = PollCursorSpec::IdSet {
            id: "id".to_owned(),
        };
        assert_eq!(
            item_occurred_at_ms(
                &json!({ "id": 1, "updatedAt": "2026-08-30T12:00:00Z" }),
                &id_set,
                T0 + 1
            ),
            T0 + 1
        );
        assert_eq!(
            item_occurred_at_ms(&json!("scalar"), &watermark, T0 + 1),
            T0 + 1
        );
        let keyed = PollItem {
            key: "77".to_owned(),
            item: json!({ "id": 77 }),
        };
        assert_eq!(
            poll_item_document(&trigger_id, &keyed, &id_set, T0).summary,
            "new item 77"
        );
    }

    #[test]
    fn exec_request_ids_are_stable_per_nominal_fire() {
        let trigger_id = BotTriggerId::new("issues");
        let id = exec_request_id(&trigger_id, T0);
        assert_eq!(id, exec_request_id(&trigger_id, T0));
        assert_ne!(id, exec_request_id(&trigger_id, T0 + 1));
        assert_ne!(id, exec_request_id(&BotTriggerId::new("other"), T0));
        assert!(id.starts_with("poll-"));
        assert_eq!(id.len(), "poll-".len() + EXEC_REQUEST_ID_DIGEST_LEN);
        assert!(
            id["poll-".len()..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        );
    }

    #[test]
    fn job_statuses_split_into_running_and_terminal() {
        for status in [
            SessionJobStatusView::Accepted,
            SessionJobStatusView::Queued,
            SessionJobStatusView::Running,
            SessionJobStatusView::CancelRequested,
        ] {
            assert!(!is_terminal_job_status(status), "{status:?}");
        }
        for status in [
            SessionJobStatusView::Succeeded,
            SessionJobStatusView::Failed,
            SessionJobStatusView::Cancelled,
            SessionJobStatusView::TimedOut,
            SessionJobStatusView::DependencyFailed,
            SessionJobStatusView::Interrupted,
            SessionJobStatusView::Lost,
        ] {
            assert!(is_terminal_job_status(status), "{status:?}");
        }
        assert_eq!(job_status_label(SessionJobStatusView::TimedOut), "timedOut");
        assert_eq!(job_status_label(SessionJobStatusView::Failed), "failed");
    }

    #[test]
    fn stderr_tails_keep_the_end_and_report_briefly() {
        let mut tail = Vec::new();
        append_tail(&mut tail, b"abcdef", 4);
        assert_eq!(tail, b"cdef");
        append_tail(&mut tail, b"gh", 4);
        assert_eq!(tail, b"efgh");
        append_tail(&mut tail, b"", 4);
        assert_eq!(tail, b"efgh");

        assert_eq!(stderr_suffix(b"   "), "");
        assert_eq!(stderr_suffix(b"  boom \n"), ": boom");
        let long = "x".repeat(STDERR_REPORT_CHARS + 10);
        let suffix = stderr_suffix(long.as_bytes());
        assert_eq!(suffix.len(), ": ".len() + STDERR_REPORT_CHARS);
    }

    #[test]
    fn job_api_errors_split_waking_environments_from_failures() {
        let waking = map_job_api_error(
            "start poll command",
            AgentApiError::environment_not_ready("booting"),
        );
        assert_eq!(
            waking,
            PollFetchError::EnvironmentNotReady("booting".to_owned())
        );
        let failed =
            map_job_api_error("start poll command", AgentApiError::rejected("no such env"));
        assert_eq!(
            failed,
            PollFetchError::Failed("start poll command: no such env".to_owned())
        );
        assert_eq!(
            payload_too_large("stdout"),
            PollFetchError::Failed(format!(
                "poll stdout exceeds {MAX_POLL_PAYLOAD_BYTES} bytes"
            ))
        );
    }
}
