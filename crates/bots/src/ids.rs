//! Every identity the bots subsystem derives. Deterministic by
//! construction: retries and duplicate fires converge on one row, one
//! delivery, one run submission. Event handles shown to models are `#N`
//! counters; session instructions also expose the concrete session id.

use api::{BotId, BotSessionKind, BotTriggerId};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

/// Federation loop bound: an event whose hop count would exceed this is
/// refused with `loop_cut`.
pub const MAX_BOT_HOPS: u32 = 8;

/// Session id prefix every session of a bot shares (`bot:v1:{bot_id}`).
pub const BOT_SESSION_PREFIX: &str = "bot:v1:";

/// Workflow-id segments reserved for system workflows; ordinary session ids
/// must not start with these so `{universe}/{session}` and
/// `{universe}/bot-…` can never collide.
pub const RESERVED_SESSION_ID_PREFIXES: [&str; 5] =
    ["bot-", "botfire-", "botsched-", "chat-", "envjob-"];

pub fn sha256_hex(value: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(value.as_ref()))
}

fn digest_prefix(value: &str, len: usize) -> String {
    let mut digest = sha256_hex(value);
    digest.truncate(len);
    digest
}

// ── Sessions ────────────────────────────────────────────────────────────────

/// Kind encoded in a controller-generated bot session id, including successors.
pub fn bot_session_kind(session_id: &str) -> BotSessionKind {
    if session_id.contains(":k-") {
        BotSessionKind::PerKey
    } else if session_id.contains(":e-") {
        BotSessionKind::PerEvent
    } else {
        BotSessionKind::Main
    }
}

/// The bot's main session: `bot:v1:{bot_id}` for generation 1,
/// `bot:v1:{bot_id}-g{n}` after a rotation.
pub fn bot_main_session_id(bot_id: &BotId, generation: u32) -> String {
    if generation <= 1 {
        format!("{BOT_SESSION_PREFIX}{bot_id}")
    } else {
        format!("{BOT_SESSION_PREFIX}{bot_id}-g{generation}")
    }
}

/// Session id for `perKey` routing: a readable slug for humans plus a digest
/// so distinct keys can never collide after slugging.
pub fn bot_keyed_session_id(bot_id: &BotId, key: &str) -> String {
    let slug = slugify(key, 40);
    let slug = if slug.is_empty() {
        "key"
    } else {
        slug.as_str()
    };
    format!(
        "{BOT_SESSION_PREFIX}{bot_id}:k-{slug}-{}",
        digest_prefix(key, 8)
    )
}

/// Session id for `perEvent` routing: one fresh session per envelope.
pub fn bot_per_event_session_id(bot_id: &BotId, event_id: &str) -> String {
    format!(
        "{BOT_SESSION_PREFIX}{bot_id}:e-{}",
        digest_prefix(event_id, 12)
    )
}

/// A rotated routed session appends `-g{n}` to its base id; the base is
/// what routing and receipts address.
pub fn routed_session_base(session_id: &str) -> &str {
    let Some((base, generation)) = session_id.rsplit_once("-g") else {
        return session_id;
    };
    if !generation.is_empty() && generation.bytes().all(|byte| byte.is_ascii_digit()) {
        base
    } else {
        session_id
    }
}

/// Session id of generation `n` for a routed base id.
pub fn routed_session_generation_id(base: &str, generation: u32) -> String {
    if generation <= 1 {
        base.to_owned()
    } else {
        format!("{base}-g{generation}")
    }
}

/// Whether a session id belongs to the bot: its main session, a generation
/// of it, or a routed session.
pub fn is_bot_session(bot_id: &BotId, session_id: &str) -> bool {
    let base = format!("{BOT_SESSION_PREFIX}{bot_id}");
    session_id == base
        || session_id
            .strip_prefix(&base)
            .is_some_and(|rest| rest.starts_with(':') || rest.starts_with("-g"))
}

/// Whether a session id is the bot's main session or a rotated generation
/// of it. Routed sessions (per conversation, per key, per event) and
/// anything that is not this bot's session are not: the answer decides
/// what only the bot's own standing context may be told, so it names the
/// main session positively instead of excluding the routed shapes.
pub fn is_bot_main_session(bot_id: &BotId, session_id: &str) -> bool {
    let base = format!("{BOT_SESSION_PREFIX}{bot_id}");
    match session_id.strip_prefix(&base) {
        Some("") => true,
        Some(rest) => rest.strip_prefix("-g").is_some_and(|generation| {
            !generation.is_empty() && generation.bytes().all(|byte| byte.is_ascii_digit())
        }),
        None => false,
    }
}

fn slugify(value: &str, max_len: usize) -> String {
    let mut slug = String::with_capacity(value.len());
    let mut pending_dash = false;
    for ch in value.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(lower);
        } else {
            pending_dash = true;
        }
    }
    slug.truncate(max_len);
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
}

// ── Workflows and schedules ─────────────────────────────────────────────────

/// The bot controller workflow: `{universe_id}/bot-{bot_id}`. The universe
/// prefix lets the worker resolve tenancy by `split_workflow_id`.
pub fn bot_controller_workflow_id(universe_id: Uuid, bot_id: &BotId) -> String {
    format!("{universe_id}/bot-{bot_id}")
}

/// Base workflow id of a trigger fire; Temporal appends the nominal fire
/// time when the Schedule starts it.
pub fn bot_trigger_fire_workflow_id(
    universe_id: Uuid,
    bot_id: &BotId,
    trigger_id: &BotTriggerId,
) -> String {
    format!("{universe_id}/botfire-{bot_id}-{trigger_id}")
}

/// Temporal Schedule id of a `schedule` or `poll` trigger.
pub fn bot_schedule_id(universe_id: Uuid, bot_id: &BotId, trigger_id: &BotTriggerId) -> String {
    format!("{universe_id}/botsched-{bot_id}-{trigger_id}")
}

/// Parse `{universe}/bot-{bot_id}` back into its parts.
pub fn split_bot_controller_workflow_id(workflow_id: &str) -> Option<(Uuid, BotId)> {
    let (universe, rest) = workflow_id.split_once('/')?;
    let universe_id = Uuid::parse_str(universe).ok()?;
    let bot_id = BotId::try_new(rest.strip_prefix("bot-")?).ok()?;
    Some((universe_id, bot_id))
}

// ── Events ──────────────────────────────────────────────────────────────────

/// One schedule fire: retries and duplicate fires of the same nominal time
/// converge on one envelope.
pub fn schedule_event_id(trigger_id: &BotTriggerId, scheduled_at_ms: i64) -> String {
    format!("schedule:{trigger_id}:{scheduled_at_ms}")
}

/// One polled item: retried fires and overlapping polls converge.
pub fn poll_event_id(trigger_id: &BotTriggerId, item_key: &str) -> String {
    format!("poll:{trigger_id}:{}", digest_prefix(item_key, 32))
}

/// One chat message at a bot: the provider message id is unique per
/// conversation, and the trigger scopes the account.
pub fn chat_message_event_id(
    trigger_id: &BotTriggerId,
    conversation_key: &str,
    provider_message_id: &str,
) -> String {
    format!(
        "chat:{trigger_id}:{}",
        digest_prefix(&format!("{conversation_key}\n{provider_message_id}"), 32)
    )
}

/// The bot's own send in a conversation, keyed by the tool invocation
/// (stable across activity retries) so one send is one row.
pub fn chat_sent_event_id(trigger_id: &BotTriggerId, invocation_id: &str) -> String {
    format!(
        "chat-sent:{trigger_id}:{}",
        digest_prefix(invocation_id, 32)
    )
}

/// A `bot_emit` event, self or addressed: one per invocation, so a retried
/// tool call never doubles an event.
pub fn bot_emit_event_id(sender_bot_id: &BotId, invocation_id: &str) -> String {
    format!("bot:{sender_bot_id}:{}", sha256_hex(invocation_id))
}

/// A `bot.reply` receipt admitted into the asking bot's inbox.
pub fn receipt_event_id(answering_bot_id: &BotId, delivery_id: &str, event_id: &str) -> String {
    format!(
        "reply:{answering_bot_id}:{}",
        sha256_hex(format!("{delivery_id}\n{event_id}"))
    )
}

/// A webhook delivery without a provider delivery id: the body digest.
pub fn webhook_body_event_id(body: &[u8]) -> String {
    format!("whk-{}", sha256_hex(body))
}

/// A manual admission without a caller-chosen id.
pub fn manual_event_id(random: Uuid) -> String {
    format!("manual-{}", random.simple())
}

/// A replay of a stored event.
pub fn replay_event_id(random: Uuid) -> String {
    format!("replay-{}", random.simple())
}

// ── Deliveries and runs ─────────────────────────────────────────────────────

/// Deterministic identity for one delivery (a single event or a coalesced
/// batch). A single-event delivery keeps the event id.
pub fn delivery_id(event_ids: &[String]) -> String {
    match event_ids {
        [] => panic!("a delivery needs at least one event"),
        [only] => only.clone(),
        many => {
            let mut sorted = many.to_vec();
            sorted.sort();
            format!("batch-{}", sha256_hex(sorted.join("\n")))
        }
    }
}

/// Submission id for a delivery's run or steering: retries converge on one. A delivery
/// id can hold a caller's event id, which need not be a valid submission id, so this
/// always hashes it.
pub fn delivery_submission_id(delivery_id: &str) -> String {
    format!("bot-event-v1-{}", sha256_hex(delivery_id))
}

/// Run-terminal notify token for a delivery.
pub fn delivery_terminal_token(delivery_id: &str) -> String {
    format!("bot-event-terminal-v1-{}", sha256_hex(delivery_id))
}

/// Context key of an appended (`whenBusy: append`) event.
pub fn appended_event_context_key(event_id: &str) -> String {
    format!("bot:event:{event_id}")
}

/// Coalescing key: one buffer per trigger and routed session.
pub fn coalesce_key(trigger_id: &BotTriggerId, routed_session_id: Option<&str>) -> String {
    format!("{trigger_id}|{}", routed_session_id.unwrap_or("main"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bot(id: &str) -> BotId {
        BotId::new(id)
    }

    #[test]
    fn main_session_generations() {
        assert_eq!(bot_main_session_id(&bot("triage"), 1), "bot:v1:triage");
        assert_eq!(bot_main_session_id(&bot("triage"), 3), "bot:v1:triage-g3");
    }

    #[test]
    fn keyed_session_slugs_and_digests() {
        let id = bot_keyed_session_id(&bot("triage"), "PR #42 / octo");
        assert!(id.starts_with("bot:v1:triage:k-pr-42-octo-"), "{id}");
        assert_eq!(id.len(), "bot:v1:triage:k-pr-42-octo-".len() + 8);
        let empty = bot_keyed_session_id(&bot("triage"), "///");
        assert!(empty.starts_with("bot:v1:triage:k-key-"), "{empty}");
        assert_ne!(
            bot_keyed_session_id(&bot("triage"), "a b"),
            bot_keyed_session_id(&bot("triage"), "a-b")
        );
    }

    #[test]
    fn routed_base_strips_generation_suffix() {
        assert_eq!(
            routed_session_base("bot:v1:t:k-x-abc-g2"),
            "bot:v1:t:k-x-abc"
        );
        assert_eq!(routed_session_base("bot:v1:t:k-x-abc"), "bot:v1:t:k-x-abc");
        assert_eq!(routed_session_base("bot:v1:t:k-x-g"), "bot:v1:t:k-x-g");
        assert_eq!(
            routed_session_generation_id("bot:v1:t:k-x", 2),
            "bot:v1:t:k-x-g2"
        );
    }

    #[test]
    fn only_the_main_session_and_its_generations_are_main() {
        let b = bot("triage");
        assert!(is_bot_main_session(&b, &bot_main_session_id(&b, 1)));
        assert!(is_bot_main_session(&b, &bot_main_session_id(&b, 3)));
        assert!(is_bot_main_session(&b, "bot:v1:triage-g12"));
        // Routed sessions, and their rotated generations.
        let keyed = bot_keyed_session_id(&b, "chat 42");
        assert!(!is_bot_main_session(&b, &keyed));
        assert!(!is_bot_main_session(
            &b,
            &routed_session_generation_id(&keyed, 2)
        ));
        assert!(!is_bot_main_session(
            &b,
            &bot_per_event_session_id(&b, "evt-1")
        ));
        // Another bot's session, an ordinary session, and near misses.
        assert!(!is_bot_main_session(&b, "bot:v1:triager"));
        assert!(!is_bot_main_session(&b, "bot:v1:other"));
        assert!(!is_bot_main_session(&b, "bot:v1:triage-g"));
        assert!(!is_bot_main_session(&b, "bot:v1:triage-gx"));
        assert!(!is_bot_main_session(&b, "bot:v1:triage-2"));
        assert!(!is_bot_main_session(&b, "ses_0123"));
        assert!(!is_bot_main_session(&b, ""));
    }

    #[test]
    fn bot_session_ownership() {
        let b = bot("triage");
        assert!(is_bot_session(&b, "bot:v1:triage"));
        assert!(is_bot_session(&b, "bot:v1:triage-g2"));
        assert!(is_bot_session(&b, "bot:v1:triage:k-a-b"));
        assert!(!is_bot_session(&b, "bot:v1:triager"));
        assert!(!is_bot_session(&b, "bot:v1:other:k-a"));
    }

    #[test]
    fn controller_workflow_id_round_trips() {
        let universe = Uuid::parse_str("6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f").unwrap();
        let id = bot_controller_workflow_id(universe, &bot("triage"));
        assert_eq!(id, "6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f/bot-triage");
        assert_eq!(
            split_bot_controller_workflow_id(&id),
            Some((universe, bot("triage")))
        );
        assert_eq!(split_bot_controller_workflow_id("nope/bot-x"), None);
        assert_eq!(
            split_bot_controller_workflow_id(&format!("{universe}/session")),
            None
        );
    }

    #[test]
    fn delivery_ids_are_stable_for_batches() {
        let single = delivery_id(&["e1".to_owned()]);
        assert_eq!(single, "e1");
        let a = delivery_id(&["e2".to_owned(), "e1".to_owned()]);
        let b = delivery_id(&["e1".to_owned(), "e2".to_owned()]);
        assert_eq!(a, b);
        assert!(a.starts_with("batch-"));
    }

    #[test]
    fn event_ids_are_deterministic() {
        let trigger = BotTriggerId::new("gh");
        assert_eq!(
            chat_message_event_id(&trigger, "k", "m"),
            chat_message_event_id(&trigger, "k", "m")
        );
        assert_ne!(
            chat_message_event_id(&trigger, "k", "m"),
            chat_message_event_id(&trigger, "k", "n")
        );
        assert_eq!(schedule_event_id(&trigger, 42), "schedule:gh:42");
        assert!(poll_event_id(&trigger, "item").starts_with("poll:gh:"));
        assert_eq!(poll_event_id(&trigger, "item").len(), "poll:gh:".len() + 32);
    }
}
