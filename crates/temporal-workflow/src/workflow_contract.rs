//! Machine-readable export of the workflow-side contract — what a receiver
//! workflow (a lifecycle controller or a workflow-tool plugin) needs to speak
//! the fixed `deliver_emission` transport with a session.
//!
//! Renders three artifacts: a draft-07 JSON Schema bundle of every envelope
//! and start-on-call type, a manifest of protocol constants plus known-answer
//! vectors for every hash derivation, and a Markdown integrator reference.
//! The committed copies under `crates/temporal-workflow/contract/` are kept
//! current by the `workflow_contract` integration test; regenerate them with
//! `cargo run -p temporal-workflow --bin export-workflow-contract`.
//!
//! The schema is derived from the serde types themselves, so it cannot drift
//! from the wire shape. The vectors cover the one thing a schema cannot
//! express — the digest framing of emission ids and recipe fingerprints —
//! and every generated consumer asserts them.

use std::collections::BTreeMap;

use engine::{
    BlobRef, EMISSION_HASH_DOMAIN, EMISSION_ID_PREFIX, EmissionEnvelope, EmissionId, EventSeq,
    PromiseId, PromiseResolution, REPLY_COMPLETION_KEY, RunId, RunStatus, SessionId, ToolBatchId,
    ToolCallId, TurnId, WORKFLOW_TOOL_EXECUTION_KIND, WorkflowToolId, WorkflowToolInvocation,
    WorkflowToolInvocationId,
};
use schemars::generate::SchemaSettings;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    WORKFLOW_TOOL_RECIPE_FINGERPRINT_PREFIX, WORKFLOW_TOOL_RECIPE_FORMAT_V1,
    WORKFLOW_TOOL_RECOVERY_QUERY, WorkflowToolRecipeV1, WorkflowToolRecoveryResult,
    WorkflowToolStartArgs, compose_environment_job_workflow_id, compose_workflow_id,
    split_workflow_id, workflow_tool_recipe_fingerprint,
};

/// Layout version of the exported manifest (not of the protocol).
pub const WORKFLOW_CONTRACT_VERSION: u32 = 2;

/// The one signal every holder and receiver handles. The workflow attributes
/// declare it as a literal (`#[signal(name = "deliver_emission")]`); this
/// constant is what the exported manifest and generated clients carry.
pub const DELIVER_EMISSION_SIGNAL: &str = "deliver_emission";

/// Root types of the schema bundle; everything else is reachable from them.
pub const WORKFLOW_CONTRACT_ROOTS: [&str; 12] = [
    "EmissionEnvelope",
    "WorkflowToolStartArgs",
    "WorkflowToolRecoveryResult",
    "WorkflowToolRecipeV1",
    "ConversationStart",
    "ChannelDeliveryCommand",
    "ChannelDeliveryResult",
    "PrepareChannelMediaInput",
    "PrepareChannelMediaResult",
    "TranscriptionWorkflowArgs",
    "TranscriptionSnapshot",
    "TranscriptionActivityResult",
];

pub struct ExportedWorkflowContract {
    /// Draft-07 JSON Schema bundle: every contract type under `definitions`.
    pub schema_bundle: Value,
    /// Protocol constants, id-derivation rules, and known-answer vectors.
    pub manifest: Value,
    /// Integrator reference rendered from the same constants.
    pub reference: String,
}

pub fn export() -> ExportedWorkflowContract {
    let mut generator = SchemaSettings::draft07().into_generator();
    let _ = generator.subschema_for::<EmissionEnvelope>();
    let _ = generator.subschema_for::<WorkflowToolStartArgs>();
    let _ = generator.subschema_for::<WorkflowToolRecoveryResult>();
    let _ = generator.subschema_for::<WorkflowToolRecipeV1>();
    let _ = generator.subschema_for::<channels::inbound::ConversationStart>();
    let _ = generator.subschema_for::<channels::delivery::ChannelDeliveryCommand>();
    let _ = generator.subschema_for::<channels::delivery::ChannelDeliveryResult>();
    let _ = generator.subschema_for::<channels::media::PrepareChannelMediaInput>();
    let _ = generator.subschema_for::<channels::media::PrepareChannelMediaResult>();
    let _ = generator.subschema_for::<channels::media::MaintainChannelTypingInput>();
    let _ = generator.subschema_for::<crate::TranscriptionWorkflowArgs>();
    let _ = generator.subschema_for::<crate::TranscriptionSnapshot>();
    let _ = generator.subschema_for::<crate::TranscriptionActivityResult>();
    let definitions: BTreeMap<String, Value> =
        generator.take_definitions(true).into_iter().collect();
    for root in WORKFLOW_CONTRACT_ROOTS {
        assert!(
            definitions.contains_key(root),
            "workflow contract root {root} is missing from the schema definitions"
        );
    }
    let reference = reference(&definitions);
    let schema_bundle = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "Lightspeed Workflow Contract",
        "description": "Envelope and start-on-call types of the fixed deliver_emission transport between sessions and receiver workflows.",
        "definitions": definitions,
    });
    ExportedWorkflowContract {
        schema_bundle,
        manifest: manifest(),
        reference,
    }
}

// Fixed vector inputs. Changing any of them changes the committed vectors,
// which is the point: every consumer's derivation test pins the same values.
const VECTOR_UNIVERSE: &str = "6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f";
const VECTOR_SESSION: &str = "bot:v1:triage";
const VECTOR_TOKEN: &str = "terminal-token-1";
const VECTOR_PRODUCER: &str = "lightspeed.bots.v1/6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f/triage";
const VECTOR_ENVIRONMENT: &str = "env_1";
const VECTOR_JOB_GROUP: &str = "job_1";

fn manifest() -> Value {
    json!({
        "contractVersion": WORKFLOW_CONTRACT_VERSION,
        "signals": { "deliverEmission": DELIVER_EMISSION_SIGNAL },
        "queries": { "workflowToolRecovery": WORKFLOW_TOOL_RECOVERY_QUERY },
        "workflowTools": {
            "executionKind": WORKFLOW_TOOL_EXECUTION_KIND,
            "replyCompletionKey": REPLY_COMPLETION_KEY,
            "recipeFormatV1": WORKFLOW_TOOL_RECIPE_FORMAT_V1,
            "recipeFingerprintPrefix": WORKFLOW_TOOL_RECIPE_FINGERPRINT_PREFIX,
        },
        "emissionIds": {
            "prefix": EMISSION_ID_PREFIX,
            "hashDomain": EMISSION_HASH_DOMAIN,
            "framing": "sha256 over the hash domain, then the kind, then each part in order; every piece is prefixed by its byte length as an unsigned 64-bit big-endian integer. Universe ids are hashed as hyphenated lowercase UUID strings; run ids as 8-byte big-endian unsigned integers.",
            "kinds": {
                "runTerminal": { "kind": "run_terminal", "parts": ["universeId:utf8", "sessionId:utf8", "runId:u64be", "token:utf8"] },
                "sourceResolution": { "kind": "source_resolution", "parts": ["universeId:utf8", "producerWorkflowId:utf8", "holderWorkflowId:utf8", "promiseId:utf8"] },
                "invocationCancellation": { "kind": "invocation_cancellation", "parts": ["invocationId:utf8", "completionKey:utf8"] },
                "toolInvocation": { "verbatim": "invocationId" },
            },
        },
        "workflowIds": {
            "separator": "/",
            "session": "{universeId}/{sessionId}",
            "environmentJob": "{universeId}/envjob-{environmentId}-{jobGroupId}",
            "botController": "{universeId}/bot-{botId}",
            "botTriggerFire": "{universeId}/botfire-{botId}-{triggerId}",
            "chatConversation": "{universeId}/chat-{provider}-{sha256 of the conversation, 48 hex}",
        },
        "channels": {
            "inboundSignal": crate::channels::CHAT_INBOUND_SIGNAL,
            "stateQuery": crate::channels::CHAT_STATE_QUERY,
            "deliveryReceiptSignal": bots::BOT_DELIVERY_SIGNAL,
            "workflowKind": crate::channels::CHANNEL_CONVERSATION_WORKFLOW_KIND,
            "connectorActivities": {
                "deliverChannelMessage": crate::channels::ACTIVITY_CONNECTOR_DELIVER_MESSAGE,
                "prepareChannelMedia": crate::channels::ACTIVITY_CONNECTOR_PREPARE_MEDIA,
                "maintainChannelTyping": crate::channels::ACTIVITY_CONNECTOR_MAINTAIN_TYPING,
            },
            "connectorTaskQueue": "lightspeed-connector-{provider}-{24 hex of sha256(domain, universeId, provider, accountId)}",
            "hashFraming": "sha256 over the domain string, then each part in order; every piece is prefixed by its byte length as an unsigned 64-bit big-endian integer.",
            "domains": {
                "conversation": "lightspeed.channels.conversation.v1",
                "pairing": "lightspeed.channels.pairing.v1",
                "connectorTaskQueue": "lightspeed.channels.delivery-queue.v1",
            },
        },
        "roots": WORKFLOW_CONTRACT_ROOTS,
        "vectors": vectors(),
    })
}

fn vectors() -> Value {
    let universe = Uuid::parse_str(VECTOR_UNIVERSE).expect("vector universe id");
    let session = SessionId::new(VECTOR_SESSION);
    let run_id = RunId::new(7);
    let promise_id = PromiseId::from_number(1);
    let invocation_id = WorkflowToolInvocationId::new(format!("wti:sha256:{}", "a".repeat(64)));
    let session_workflow_id = compose_workflow_id(universe, &session);
    let job_workflow_id =
        compose_environment_job_workflow_id(universe, VECTOR_ENVIRONMENT, VECTOR_JOB_GROUP);
    let (split_universe, split_session) =
        split_workflow_id(&session_workflow_id).expect("composed session workflow id splits");
    let recipe = WorkflowToolRecipeV1 {
        workflow_type: "BotControllerWorkflow".to_owned(),
        task_queue: "lightspeed-bots".to_owned(),
    };
    let recipe_json = serde_json::to_string(&recipe).expect("recipe serializes");
    let invocation = WorkflowToolInvocation {
        invocation_id: invocation_id.clone(),
        tool_id: WorkflowToolId::new("lightspeed.bots.event.resolve.v1"),
        semantic_type: "lightspeed.bots.event.resolve.v1".to_owned(),
        schema_revision: 1,
        binding_fingerprint: "binding:v1:vector".to_owned(),
        session_universe_id: universe,
        session_id: session.clone(),
        run_id,
        turn_id: TurnId::new(8),
        tool_batch_id: ToolBatchId::new(9),
        tool_call_id: ToolCallId::new("call_1"),
        arguments_ref: BlobRef::from_bytes(b"{}"),
        execution_context_ref: None,
        completion_promises: Some(BTreeMap::from([(
            REPLY_COMPLETION_KEY.to_owned(),
            promise_id.clone(),
        )])),
    };
    let start_args = WorkflowToolStartArgs {
        universe_id: universe,
        holder_workflow_id: session_workflow_id.clone(),
        execution_id: "wte:vector".to_owned(),
        invocation: invocation.clone(),
    };
    let recovery = WorkflowToolRecoveryResult {
        resolutions: BTreeMap::from([(
            REPLY_COMPLETION_KEY.to_owned(),
            PromiseResolution::Resolved { payload_ref: None },
        )]),
    };
    let envelopes = [
        EmissionEnvelope::run_terminal(
            universe,
            session.clone(),
            EventSeq::new(42),
            VECTOR_TOKEN.to_owned(),
            run_id,
            RunStatus::Completed,
            Some(engine::ContentRef::text(BlobRef::from_bytes(b"output"))),
            None,
        ),
        EmissionEnvelope::source_resolution(
            universe,
            VECTOR_PRODUCER.to_owned(),
            &session_workflow_id,
            promise_id.clone(),
            PromiseResolution::Resolved { payload_ref: None },
        ),
        EmissionEnvelope::tool_invocation(
            universe,
            session.clone(),
            EventSeq::new(43),
            invocation,
            session_workflow_id.clone(),
        ),
        EmissionEnvelope::invocation_cancellation(
            universe,
            session.clone(),
            EventSeq::new(44),
            invocation_id.clone(),
            REPLY_COMPLETION_KEY.to_owned(),
            promise_id.clone(),
        ),
    ];
    json!({
        "inputs": {
            "universeId": VECTOR_UNIVERSE,
            "sessionId": VECTOR_SESSION,
            "runId": run_id.as_u64(),
            "token": VECTOR_TOKEN,
            "producerWorkflowId": VECTOR_PRODUCER,
            "holderWorkflowId": session_workflow_id,
            "promiseId": promise_id.as_str(),
            "invocationId": invocation_id.as_str(),
            "completionKey": REPLY_COMPLETION_KEY,
            "environmentId": VECTOR_ENVIRONMENT,
            "jobGroupId": VECTOR_JOB_GROUP,
            "recipeJson": recipe_json,
        },
        "emissionIds": {
            "runTerminal": EmissionId::for_run_terminal(universe, &session, run_id, VECTOR_TOKEN).as_str(),
            "sourceResolution": EmissionId::for_source_resolution(universe, VECTOR_PRODUCER, &session_workflow_id, &promise_id).as_str(),
            "toolInvocation": EmissionId::for_tool_invocation(&invocation_id).as_str(),
            "invocationCancellation": EmissionId::for_invocation_cancellation(&invocation_id, REPLY_COMPLETION_KEY).as_str(),
        },
        "workflowIds": {
            "session": session_workflow_id,
            "environmentJob": job_workflow_id,
            "split": { "universeId": split_universe.to_string(), "sessionId": split_session.as_str() },
        },
        "recipeFingerprint": workflow_tool_recipe_fingerprint(recipe_json.as_bytes()),
        "channels": channel_vectors(universe),
        "envelopes": envelopes.iter().map(|envelope| serde_json::to_value(envelope).expect("envelope serializes")).collect::<Vec<_>>(),
        "startArgs": serde_json::to_value(&start_args).expect("start args serialize"),
        "recoveryResult": serde_json::to_value(&recovery).expect("recovery result serializes"),
        "recipe": serde_json::to_value(&recipe).expect("recipe serializes"),
    })
}

/// Known-answer vectors for the connector host: the task queue it serves,
/// the conversation workflow id it never constructs but may log, and the
/// pairing key of a chat.
fn channel_vectors(universe: Uuid) -> Value {
    use channels::{ConversationRef, connector_task_queue, conversation_workflow_id};
    let account_id = api::ChannelAccountId::new("tg-main");
    let conversation = ConversationRef {
        account_id: account_id.clone(),
        chat_id: "12345".to_owned(),
        thread_id: Some("7".to_owned()),
    };
    json!({
        "inputs": {
            "universeId": universe.to_string(),
            "provider": "telegram",
            "accountId": account_id.as_str(),
            "chatId": "12345",
            "threadId": "7",
        },
        "connectorTaskQueue": connector_task_queue(universe, &api::ChannelProvider::new("telegram"), &account_id),
        "conversationWorkflowId": conversation_workflow_id(universe, &api::ChannelProvider::new("telegram"), &conversation),
        "conversationKey": conversation.key(),
    })
}

fn reference(definitions: &BTreeMap<String, Value>) -> String {
    format!(
        r#"# Lightspeed Workflow Contract

Generated from the Rust contract types in `engine` and `temporal-workflow`.
Type shapes live in `workflow.schema.json`; constants and known-answer vectors
live in `workflow.json`. Regenerate both artifacts and this reference with:

```console
cargo run -p temporal-workflow --bin export-workflow-contract
```

## Transport

The Temporal signal `{signal}` carries every cross-workflow fact in both
directions. Its sole argument is an `EmissionEnvelope`: a deterministic
`emission_id`, a `producer`, and a tagged `body`. `AgentSessionWorkflow` and
`EnvironmentJobWorkflow` handle this signal; receivers register the same
handler. Signal stable workflow ids, never run ids, so delivery survives
continue-as-new.

Delivery is at least once with bounded retries. A receiver must persistently
deduplicate by `emission_id` and treat a duplicate as a no-op. Tool-invocation
emissions reuse their invocation id as the emission id.

## Envelope bodies

- `run_terminal`: a session run reached a terminal status. Core sends this
  only to the immutable lifecycle controller and only for a run started with
  terminal notification. The token is opaque controller correlation state.
- `source_resolution`: a workflow resolved, failed, or cancelled one keyed
  Promise. The session accepts it only when the workflow producer id exactly
  matches the Promise source recorded at admission.
- `tool_invocation`: a pushed invocation for a bound receiver. Its
  `holder_workflow_id` is the session endpoint to signal with replies; do not
  reconstruct that id. Model arguments remain in CAS at `arguments_ref`.
- `invocation_cancellation`: a best-effort notice that one completion Promise
  is already cancelled. Stop the corresponding domain work when practical;
  a later reply is ignored because the Promise is terminal.

## Producer authorization

Producer identity is authority, not metadata. Session emissions carry the
universe, session id, and exact producing log sequence. Workflow emissions
carry the universe and stable producer workflow id. A holder rejects a source
resolution whose workflow id differs from the immutable Promise source.

## Push, pull, and lifecycle gates

Push dispatch controls delivery, not completion semantics: bound workflow
tools may be pushed to their receiver or pulled from the session log. A
lifecycle controller additionally gates terminal routing and managed-session
lifecycle. Tool receivers cannot branch session state by fabricating a
terminal and are not granted lifecycle authority. A self-receiver must answer
within the session's receiver deadline so the holder cannot deadlock on itself.

## Replies and keyed completion

The reserved completion key is `{reply_key}`. Joined tools expose exactly that
one receiver-visible Promise. A receiver sends a `source_resolution` envelope
to the invocation's `holder_workflow_id`; its producer workflow id must be the
workflow execution authorized by the binding. Derived ids are deterministic,
and the known-answer vectors in `workflow.json` define the cross-language hash
framing.

## Start-on-call workflows

Start-on-call bindings launch a workflow with `WorkflowToolStartArgs`. The
execution resolves its keyed completions through the same `{signal}` envelope
transport. On ambiguous start recovery, query `{recovery_query}` and consume a
`WorkflowToolRecoveryResult`. Recipes use format `{recipe_format}` and are
fingerprinted over their exact raw bytes; canonical fingerprints begin with
`{recipe_prefix}`. The execution producer kind is `{execution_kind}`.

## Schema inventory

The schema bundle contains {definition_count} definitions. Its public roots
are: {roots}.
"#,
        signal = DELIVER_EMISSION_SIGNAL,
        reply_key = REPLY_COMPLETION_KEY,
        recovery_query = WORKFLOW_TOOL_RECOVERY_QUERY,
        recipe_format = WORKFLOW_TOOL_RECIPE_FORMAT_V1,
        recipe_prefix = WORKFLOW_TOOL_RECIPE_FINGERPRINT_PREFIX,
        execution_kind = WORKFLOW_TOOL_EXECUTION_KIND,
        definition_count = definitions.len(),
        roots = WORKFLOW_CONTRACT_ROOTS.join(", "),
    )
}
