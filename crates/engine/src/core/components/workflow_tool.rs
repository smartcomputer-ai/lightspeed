use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    BlobRef, CodecError, CoreAgentCodec, CoreAgentEntry, CoreAgentEvent, DomainError, PromiseId,
    RunId, SessionId, ToolBatchId, ToolCallId, ToolEffect, ToolKind, ToolName, ToolSpec, TurnId,
    WorkflowToolId, WorkflowToolInvocationId, storage::StoredSessionEntry,
};

const MANAGED_TOOL_DECLARATION_VERSION: u32 = 1;
const MAX_MANAGED_TOOLS: usize = 32;
pub const MAX_WORKFLOW_TOOL_EMISSIONS_PER_RUN: u32 = 32;
pub const MAX_WORKFLOW_TOOL_EMISSIONS_PER_READ: usize =
    MAX_MANAGED_TOOLS * MAX_WORKFLOW_TOOL_EMISSIONS_PER_RUN as usize;
/// Admission-time resource guardrail on one invocation's keyed promise set,
/// not a semantic. Bindings declare their own `max_promises` at or below it.
pub const MAX_COMPLETION_PROMISES: u32 = 32;
const COMPLETION_KEY_MAX_LEN: usize = 64;
/// The single completion key request/reply tools use.
pub const REPLY_COMPLETION_KEY: &str = "reply";
const WORKFLOW_ID_MAX_LEN: usize = 512;
const WORKFLOW_KIND_MAX_LEN: usize = 128;
const SEMANTIC_TYPE_MAX_LEN: usize = 192;
const RECIPE_FINGERPRINT_MAX_LEN: usize = 256;
const BINDING_FINGERPRINT_DOMAIN: &str = "lightspeed.workflow-tool.binding.v4";
const CREATION_FINGERPRINT_DOMAIN: &str = "lightspeed.managed-session.creation.v1";
/// v4: bound dispatch is explicit and participates in binding identity.
/// Greenfield identity change.
const FINGERPRINT_ENCODING_VERSION: u32 = 4;
const INVOCATION_ID_DOMAIN: &str = "lightspeed.workflow-tool.invocation.v1";
const EXECUTION_ID_DOMAIN: &str = "lightspeed.workflow-tool.execution.v1";
/// Diagnostic endpoint kind carried by promise sources whose producer is a
/// system-derived started execution rather than an admitted bound receiver.
pub const WORKFLOW_TOOL_EXECUTION_KIND: &str = "workflow_tool.execution";
const RESERVED_RUN_TERMINAL_SEMANTIC_TYPE: &str = "lightspeed.run.terminal.v1";
pub const WORKFLOW_TOOL_EMIT_EFFECT_KIND: &str = "lightspeed.core.workflow_tool.emit";

const EFFECT_INVOCATION_ID: &str = "invocation_id";
const EFFECT_PORT_ID: &str = "tool_id";
const EFFECT_SEMANTIC_TYPE: &str = "semantic_type";
const EFFECT_SCHEMA_REVISION: &str = "schema_revision";
const EFFECT_BINDING_FINGERPRINT: &str = "binding_fingerprint";
const EFFECT_SESSION_UNIVERSE_ID: &str = "session_universe_id";
const EFFECT_SESSION_ID: &str = "session_id";
const EFFECT_RUN_ID: &str = "run_id";
const EFFECT_TURN_ID: &str = "turn_id";
const EFFECT_TOOL_BATCH_ID: &str = "tool_batch_id";
const EFFECT_TOOL_CALL_ID: &str = "tool_call_id";
const EFFECT_ARGUMENTS_REF: &str = "arguments_ref";
const EFFECT_EXECUTION_CONTEXT_REF: &str = "execution_context_ref";
const EFFECT_COMPLETION_PROMISES: &str = "completion_promises";
const EFFECT_COMPLETION_DEADLINE_MS: &str = "completion_deadline_ms";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowEndpointRef {
    pub workflow_id: String,
    pub workflow_kind: String,
}

impl WorkflowEndpointRef {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.workflow_id.is_empty() {
            return Err(DomainError::InvariantViolation(
                "workflow endpoint id must not be empty".to_owned(),
            ));
        }
        if self.workflow_id.len() > WORKFLOW_ID_MAX_LEN {
            return Err(DomainError::InvariantViolation(format!(
                "workflow endpoint id is too long: {} bytes, max {}",
                self.workflow_id.len(),
                WORKFLOW_ID_MAX_LEN
            )));
        }
        validate_component(
            "workflow endpoint kind",
            &self.workflow_kind,
            WORKFLOW_KIND_MAX_LEN,
            "ASCII letters, digits, '_', '-', '.'",
            |ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'),
        )
    }
}

/// Trusted, substrate-neutral reference to a workflow-start recipe for
/// start-on-call tools. The recipe body (workflow type, task queue, ...) is
/// CAS-backed and resolved by a workflow-substrate adapter outside `engine`.
/// `recipe_format` identifies the generic recipe codec, never a feature,
/// service, or workflow plugin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowStartRef {
    pub recipe_format: u32,
    pub revision: u32,
    pub recipe_ref: BlobRef,
    pub recipe_fingerprint: String,
}

impl WorkflowStartRef {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.recipe_format == 0 || self.revision == 0 {
            return Err(DomainError::InvariantViolation(
                "workflow start recipe format and revision must be greater than zero".to_owned(),
            ));
        }
        if self.recipe_fingerprint.is_empty()
            || self.recipe_fingerprint.len() > RECIPE_FINGERPRINT_MAX_LEN
        {
            return Err(DomainError::InvariantViolation(format!(
                "workflow start recipe fingerprint must be 1..={RECIPE_FINGERPRINT_MAX_LEN} bytes"
            )));
        }
        Ok(())
    }
}

/// Target lifecycle of a workflow tool: an execution that existed before the
/// managed session was admitted, or one started on call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum WorkflowToolTarget {
    Bound {
        receiver: WorkflowEndpointRef,
        dispatch: BoundWorkflowToolDispatch,
    },
    Start {
        start: WorkflowStartRef,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// How an invocation of a bound workflow tool reaches its receiver.
pub enum BoundWorkflowToolDispatch {
    /// The receiver consumes the invocation from the authorized session log.
    Pull,
    /// The runtime durably emits the invocation to the receiver workflow.
    Push,
}

impl WorkflowToolTarget {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Bound { receiver, .. } => receiver.validate(),
            Self::Start { start } => start.validate(),
        }
    }

    pub fn bound_receiver(&self) -> Option<&WorkflowEndpointRef> {
        match self {
            Self::Bound { receiver, .. } => Some(receiver),
            Self::Start { .. } => None,
        }
    }

    pub fn bound_dispatch(&self) -> Option<BoundWorkflowToolDispatch> {
        match self {
            Self::Bound { dispatch, .. } => Some(*dispatch),
            Self::Start { .. } => None,
        }
    }
}

/// Completion contract of a workflow tool. Conceptually completion is a set
/// of keyed promises, possibly empty: `Accepted` is the empty set,
/// request/reply is the singleton set with the reserved key `reply`.
/// Bound dispatch is declared independently on the target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum WorkflowToolCompletion {
    Accepted,
    Joined {
        /// Schema the single semantic reply payload must satisfy.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_schema_ref: Option<BlobRef>,
        /// Required non-zero hard deadline for the runtime-owned reply.
        deadline_after_ms: u64,
    },
    Promises {
        /// Schema every keyed resolution payload must satisfy.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reply_schema_ref: Option<BlobRef>,
        /// Trusted relative hard deadline applied to each created promise.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deadline_after_ms: Option<u64>,
        /// Per-invocation key-count guardrail (>= 1). Request/reply tools
        /// declare 1.
        max_promises: u32,
        /// Declarative derivation of the keyed Promise set from validated
        /// arguments. The session worker interprets this generic vocabulary
        /// without compiling plugin-specific code.
        key_source: WorkflowToolCompletionKeySource,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum WorkflowToolCompletionKeySource {
    /// The singleton request/reply shape using the reserved `reply` key.
    Reply,
    /// A JSON Pointer naming an array of unique completion-key strings.
    StringArray { pointer: String },
    /// A JSON Pointer naming an array of objects; the named string field
    /// of every item is its completion key, so the model's own name for a
    /// work item (a job id) keys the item's promise.
    ArrayItemField { pointer: String, field: String },
    ArrayIndices {
        /// JSON Pointer to a schema-validated array. Every array item creates
        /// one completion key.
        pointer: String,
        /// Prefix joined directly with the zero-based item index. For
        /// example, `job-` produces `job-0`, `job-1`, ... .
        prefix: String,
    },
}

impl WorkflowToolCompletionKeySource {
    fn validate(&self, max_promises: u32) -> Result<(), DomainError> {
        match self {
            Self::Reply => {
                if max_promises != 1 {
                    return Err(DomainError::InvariantViolation(
                        "workflow tool reply completion key source requires max_promises = 1"
                            .to_owned(),
                    ));
                }
                Ok(())
            }
            Self::StringArray { pointer } => validate_completion_pointer(pointer, max_promises),
            Self::ArrayItemField { pointer, field } => {
                validate_completion_pointer(pointer, max_promises)?;
                if field.is_empty() || field.contains('/') {
                    return Err(DomainError::InvariantViolation(
                        "workflow tool array-item completion key field must be a non-empty object key"
                            .to_owned(),
                    ));
                }
                Ok(())
            }
            Self::ArrayIndices { pointer, prefix } => {
                validate_completion_pointer(pointer, max_promises)?;
                let largest_key = format!("{prefix}{}", max_promises.saturating_sub(1));
                validate_completion_key(&largest_key).map_err(|error| {
                    DomainError::InvariantViolation(format!(
                        "workflow tool array-index completion key prefix is invalid: {error}"
                    ))
                })
            }
        }
    }
}

impl WorkflowToolCompletion {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Accepted => Ok(()),
            Self::Joined {
                deadline_after_ms, ..
            } => {
                if *deadline_after_ms == 0 {
                    return Err(DomainError::InvariantViolation(
                        "joined workflow tool completion requires a non-zero deadline_after_ms"
                            .to_owned(),
                    ));
                }
                Ok(())
            }
            Self::Promises {
                max_promises,
                key_source,
                ..
            } => {
                if *max_promises == 0 || *max_promises > MAX_COMPLETION_PROMISES {
                    return Err(DomainError::InvariantViolation(format!(
                        "workflow tool completion max_promises must be 1..={MAX_COMPLETION_PROMISES}"
                    )));
                }
                key_source.validate(*max_promises)?;
                Ok(())
            }
        }
    }

    pub fn is_promise_bearing(&self) -> bool {
        matches!(self, Self::Joined { .. } | Self::Promises { .. })
    }

    pub fn exposes_model_owned_promises(&self) -> bool {
        matches!(self, Self::Promises { .. })
    }
}

fn validate_completion_pointer(pointer: &str, max_promises: u32) -> Result<(), DomainError> {
    if pointer.is_empty() || !pointer.starts_with('/') {
        return Err(DomainError::InvariantViolation(
            "workflow tool completion key pointer must be a JSON Pointer starting with '/'"
                .to_owned(),
        ));
    }
    if max_promises < 2 {
        return Err(DomainError::InvariantViolation(
            "workflow tool completion key pointer requires max_promises >= 2".to_owned(),
        ));
    }
    Ok(())
}

pub fn validate_completion_key(key: &str) -> Result<(), DomainError> {
    validate_component(
        "workflow tool completion key",
        key,
        COMPLETION_KEY_MAX_LEN,
        "ASCII letters, digits, '_', '-', '.'",
        |ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'),
    )
}

/// Deterministic execution id for one start-on-call invocation. The
/// invocation id already covers universe, session, run, turn, batch, call,
/// and binding fingerprint; adding the recipe fingerprint makes the started
/// execution identity cover the exact trusted start recipe. The same
/// invocation always derives the same execution id, so retry treats
/// `AlreadyStarted` as success only for that exact identity.
pub fn workflow_tool_execution_id(
    invocation_id: &WorkflowToolInvocationId,
    recipe_fingerprint: &str,
) -> String {
    let digest = digest_fields(
        EXECUTION_ID_DOMAIN,
        &[
            invocation_id.as_str().as_bytes(),
            recipe_fingerprint.as_bytes(),
        ],
    );
    format!("wtx:sha256:{}", hex::encode(digest))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowToolDefinition {
    pub tool_id: WorkflowToolId,
    pub revision: u32,
    pub semantic_type: String,
    /// Complete provider-facing function tool definition. Workflow-tool
    /// routing remains separate and never appears in model arguments.
    pub tool: ToolSpec,
}

impl WorkflowToolDefinition {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.revision == 0 {
            return Err(DomainError::InvariantViolation(format!(
                "workflow tool {} revision must be greater than zero",
                self.tool_id
            )));
        }
        validate_semantic_type(&self.semantic_type)?;
        self.tool.validate()?;
        if !matches!(self.tool.kind, ToolKind::Builtin(_) | ToolKind::Function(_)) {
            return Err(DomainError::InvariantViolation(format!(
                "workflow tool {} must use a function tool",
                self.tool_id
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowToolBinding {
    /// Universe of the managed session that owns this binding. This is the
    /// emission source scope, not a claim about the receiver's scope.
    pub session_universe_id: Uuid,
    pub definition: WorkflowToolDefinition,
    pub target: WorkflowToolTarget,
    pub completion: WorkflowToolCompletion,
    pub binding_fingerprint: String,
}

/// Bounded durable record of one successful workflow-tool tool call.
///
/// The model arguments remain in CAS and are referenced by `arguments_ref`.
/// Receiver-specific interpretation belongs to the receiving workflow.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "contract", derive(schemars::JsonSchema))]
pub struct WorkflowToolInvocation {
    pub invocation_id: WorkflowToolInvocationId,
    pub tool_id: WorkflowToolId,
    pub semantic_type: String,
    pub schema_revision: u32,
    pub binding_fingerprint: String,
    pub session_universe_id: Uuid,
    pub session_id: SessionId,
    pub run_id: RunId,
    pub turn_id: TurnId,
    pub tool_batch_id: ToolBatchId,
    pub tool_call_id: ToolCallId,
    pub arguments_ref: BlobRef,
    /// Opaque, runtime-supplied context for the receiving workflow. This is
    /// separate from model-authored arguments so runtime state can be pinned
    /// without changing the tool's declared argument schema.
    pub execution_context_ref: Option<BlobRef>,
    /// Keyed promise set created atomically with this invocation; `None`
    /// for notify (`Accepted`) completion. Request/reply is the single
    /// reserved key `reply`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_promises: Option<BTreeMap<String, PromiseId>>,
}

#[derive(Debug, Error)]
pub enum ReadToolEmissionsError {
    #[error("invalid workflow-tool receiver endpoint: {message}")]
    InvalidReceiver { message: String },

    #[error("decode workflow-tool session entry: {0}")]
    Decode(#[from] CodecError),

    #[error("reduce workflow-tool session log: {message}")]
    InvalidSessionLog { message: String },

    #[error("invalid durable workflow-tool binding {binding_fingerprint}: {message}")]
    InvalidBinding {
        binding_fingerprint: String,
        message: String,
    },

    #[error("workflow-tool receiver is not bound to this session: {workflow_id}")]
    ReceiverNotBound { workflow_id: String },

    #[error(
        "workflow-tool invocation {invocation_id} references unknown durable binding {binding_fingerprint}"
    )]
    UnknownBinding {
        invocation_id: WorkflowToolInvocationId,
        binding_fingerprint: String,
    },

    #[error(
        "workflow-tool invocation {invocation_id} does not match its durable binding: {message}"
    )]
    InvocationBindingMismatch {
        invocation_id: WorkflowToolInvocationId,
        message: String,
    },

    #[error("workflow-tool invocation {invocation_id} does not match its event joins")]
    InvocationJoinMismatch {
        invocation_id: WorkflowToolInvocationId,
    },

    #[error("duplicate workflow-tool invocation in session log: {invocation_id}")]
    DuplicateInvocation {
        invocation_id: WorkflowToolInvocationId,
    },

    #[error("workflow-tool emission read exceeds the bounded result limit of {limit} invocations")]
    ResultLimitExceeded { limit: usize },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowToolEvent {
    Emitted {
        invocation: WorkflowToolInvocation,
    },
    DeliveryFailed {
        invocation_id: WorkflowToolInvocationId,
        error_ref: BlobRef,
    },
    /// Durable start intent for one start-on-call invocation. There is no
    /// successful `Started` event: Temporal history is the durable record
    /// of the start command, the deterministic execution id makes retry
    /// safe, and `AlreadyStarted` is the recovery path. `execution_id` is
    /// denormalized from the binding's recipe fingerprint and re-validated
    /// against the canonical derivation at apply.
    StartRequested {
        invocation: WorkflowToolInvocation,
        execution_id: String,
    },
    /// Terminal start failure: the deterministic start could not be issued
    /// after bounded retry. Fails the invocation's still-pending keyed
    /// promises in the same append.
    StartFailed {
        invocation_id: WorkflowToolInvocationId,
        error_ref: BlobRef,
    },
}

impl WorkflowToolBinding {
    pub fn admit(
        session_universe_id: Uuid,
        definition: WorkflowToolDefinition,
        target: WorkflowToolTarget,
        completion: WorkflowToolCompletion,
    ) -> Result<Self, DomainError> {
        definition.validate()?;
        target.validate()?;
        completion.validate()?;
        validate_target_completion(&definition.tool_id, &target, &completion)?;
        let binding_fingerprint =
            binding_fingerprint(session_universe_id, &definition, &target, &completion)?;
        Ok(Self {
            session_universe_id,
            definition,
            target,
            completion,
            binding_fingerprint,
        })
    }

    /// Convenience for the original bound-receiver, notify-only shape.
    pub fn admit_bound_notify(
        session_universe_id: Uuid,
        definition: WorkflowToolDefinition,
        receiver: WorkflowEndpointRef,
    ) -> Result<Self, DomainError> {
        Self::admit(
            session_universe_id,
            definition,
            WorkflowToolTarget::Bound {
                receiver,
                dispatch: BoundWorkflowToolDispatch::Pull,
            },
            WorkflowToolCompletion::Accepted,
        )
    }

    pub fn bound_receiver(&self) -> Option<&WorkflowEndpointRef> {
        self.target.bound_receiver()
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.definition.validate()?;
        self.target.validate()?;
        self.completion.validate()?;
        validate_target_completion(&self.definition.tool_id, &self.target, &self.completion)?;
        let expected = binding_fingerprint(
            self.session_universe_id,
            &self.definition,
            &self.target,
            &self.completion,
        )?;
        if self.binding_fingerprint != expected {
            return Err(DomainError::InvariantViolation(format!(
                "workflow tool {} binding fingerprint does not match its durable definition, target, and completion",
                self.definition.tool_id
            )));
        }
        Ok(())
    }
}

/// One trusted workflow-tool declaration supplied when a managed session is
/// created. The receiver is opaque to the session core and need not be the
/// lifecycle controller.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowToolDeclaration {
    pub definition: WorkflowToolDefinition,
    pub target: WorkflowToolTarget,
    pub completion: WorkflowToolCompletion,
}

impl WorkflowToolDeclaration {
    pub fn new(
        definition: WorkflowToolDefinition,
        target: WorkflowToolTarget,
        completion: WorkflowToolCompletion,
    ) -> Self {
        Self {
            definition,
            target,
            completion,
        }
    }

    /// The original shape: bound receiver, notify-only completion.
    pub fn bound_notify(definition: WorkflowToolDefinition, receiver: WorkflowEndpointRef) -> Self {
        Self::new(
            definition,
            WorkflowToolTarget::Bound {
                receiver,
                dispatch: BoundWorkflowToolDispatch::Pull,
            },
            WorkflowToolCompletion::Accepted,
        )
    }
}

fn validate_target_completion(
    tool_id: &WorkflowToolId,
    target: &WorkflowToolTarget,
    completion: &WorkflowToolCompletion,
) -> Result<(), DomainError> {
    match (target, completion) {
        (
            WorkflowToolTarget::Bound {
                dispatch: BoundWorkflowToolDispatch::Pull,
                ..
            },
            WorkflowToolCompletion::Accepted,
        )
        | (
            WorkflowToolTarget::Bound {
                dispatch: BoundWorkflowToolDispatch::Push,
                ..
            },
            WorkflowToolCompletion::Accepted
            | WorkflowToolCompletion::Joined { .. }
            | WorkflowToolCompletion::Promises { .. },
        )
        | (
            WorkflowToolTarget::Start { .. },
            WorkflowToolCompletion::Joined { .. } | WorkflowToolCompletion::Promises { .. },
        ) => Ok(()),
        (
            WorkflowToolTarget::Bound {
                dispatch: BoundWorkflowToolDispatch::Pull,
                ..
            },
            WorkflowToolCompletion::Joined { .. } | WorkflowToolCompletion::Promises { .. },
        ) => Err(DomainError::InvariantViolation(format!(
            "workflow tool {tool_id} pull dispatch supports Accepted completion only"
        ))),
        (WorkflowToolTarget::Start { .. }, WorkflowToolCompletion::Accepted) => {
            Err(DomainError::InvariantViolation(format!(
                "workflow tool {tool_id} fire-and-forget start targets are deferred; start targets require promise-bearing completion"
            )))
        }
    }
}

/// Trusted creation document supplied by a workflow plugin or other
/// authorized control-plane caller. The lifecycle controller owns the outer
/// session loop; each tool independently names its receiver.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedSessionWorkflowTools {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_controller: Option<WorkflowEndpointRef>,
    pub tools: Vec<WorkflowToolDeclaration>,
}

impl ManagedSessionWorkflowTools {
    pub fn v1(
        lifecycle_controller: Option<WorkflowEndpointRef>,
        tools: Vec<WorkflowToolDeclaration>,
    ) -> Self {
        Self {
            version: MANAGED_TOOL_DECLARATION_VERSION,
            lifecycle_controller,
            tools,
        }
    }

    pub fn admit(
        &self,
        session_universe_id: Uuid,
    ) -> Result<AdmittedManagedSessionWorkflowTools, DomainError> {
        if self.version != MANAGED_TOOL_DECLARATION_VERSION {
            return Err(DomainError::InvariantViolation(format!(
                "unsupported managed-session workflow tool declaration version {}",
                self.version
            )));
        }
        if let Some(controller) = &self.lifecycle_controller {
            controller.validate()?;
        }
        if self.tools.len() > MAX_MANAGED_TOOLS {
            return Err(DomainError::InvariantViolation(format!(
                "managed-session workflow tool declaration contains {} tools, max {}",
                self.tools.len(),
                MAX_MANAGED_TOOLS
            )));
        }

        let mut declarations = self.tools.clone();
        declarations.sort_by(|left, right| left.definition.tool_id.cmp(&right.definition.tool_id));
        let mut tool_ids = BTreeSet::new();
        let mut tool_names = BTreeSet::new();
        let mut bindings = Vec::with_capacity(declarations.len());
        for declaration in declarations {
            let definition = declaration.definition;
            if !tool_ids.insert(definition.tool_id.clone()) {
                return Err(DomainError::InvariantViolation(format!(
                    "managed-session workflow tool declaration contains duplicate tool id {}",
                    definition.tool_id
                )));
            }
            if !tool_names.insert(definition.tool.name.clone()) {
                return Err(DomainError::InvariantViolation(format!(
                    "managed-session workflow tool declaration contains duplicate tool name {}",
                    definition.tool.name
                )));
            }
            let binding = WorkflowToolBinding::admit(
                session_universe_id,
                definition,
                declaration.target,
                declaration.completion,
            )?;
            validate_controller_self_receiver(&binding, self.lifecycle_controller.as_ref())?;
            bindings.push(binding);
        }
        let creation_fingerprint = creation_fingerprint(
            session_universe_id,
            self.version,
            self.lifecycle_controller.as_ref(),
            &bindings,
        )?;
        Ok(AdmittedManagedSessionWorkflowTools {
            session_universe_id,
            version: self.version,
            lifecycle_controller: self.lifecycle_controller.clone(),
            creation_fingerprint,
            bindings,
        })
    }

    pub fn creation_fingerprint(&self, session_universe_id: Uuid) -> Result<String, DomainError> {
        Ok(self.admit(session_universe_id)?.creation_fingerprint)
    }
}

/// A lifecycle controller may receive a promise-bearing invocation from its
/// own managed session only when every created promise has a hard deadline.
/// The controller still owns the semantic progress contract (handle the push
/// independently and never re-enter the emitting run); the deadline is the
/// durable, machine-checkable backstop against a permanent self-deadlock.
fn validate_controller_self_receiver(
    binding: &WorkflowToolBinding,
    lifecycle_controller: Option<&WorkflowEndpointRef>,
) -> Result<(), DomainError> {
    if binding.bound_receiver() != lifecycle_controller || lifecycle_controller.is_none() {
        return Ok(());
    }
    match &binding.completion {
        WorkflowToolCompletion::Joined {
            deadline_after_ms, ..
        } if *deadline_after_ms > 0 => return Ok(()),
        WorkflowToolCompletion::Promises {
            deadline_after_ms: Some(deadline_after_ms),
            ..
        } if *deadline_after_ms > 0 => return Ok(()),
        _ => {}
    }
    if binding.completion.is_promise_bearing() {
        return Err(DomainError::InvariantViolation(format!(
            "workflow tool {} bound to the session's lifecycle controller requires a non-zero promise deadline_after_ms",
            binding.definition.tool_id
        )));
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmittedManagedSessionWorkflowTools {
    pub session_universe_id: Uuid,
    pub version: u32,
    pub lifecycle_controller: Option<WorkflowEndpointRef>,
    pub creation_fingerprint: String,
    pub bindings: Vec<WorkflowToolBinding>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowToolConfigEvent {
    ManagedBindingsAdmitted {
        session_universe_id: Uuid,
        declaration_version: u32,
        lifecycle_controller: Option<WorkflowEndpointRef>,
        creation_fingerprint: String,
        bindings: Vec<WorkflowToolBinding>,
    },
    /// One add-only system-owned workflow binding. This is deliberately
    /// separate from managed-session creation: it grants an implementation
    /// capability without assigning lifecycle ownership to a workflow.
    SystemBindingAdmitted { binding: Box<WorkflowToolBinding> },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowToolState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_universe_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_controller: Option<WorkflowEndpointRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_declaration_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_creation_fingerprint: Option<String>,
    #[serde(default)]
    pub bindings: BTreeMap<WorkflowToolId, WorkflowToolBinding>,
    /// Binding ids admitted by the trusted runtime after session creation.
    /// These bindings are not part of the immutable managed-session creation
    /// document or its fingerprint.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub system_binding_ids: BTreeSet<WorkflowToolId>,
    #[serde(default)]
    pub emissions: BTreeMap<WorkflowToolInvocationId, WorkflowToolInvocation>,
    #[serde(default)]
    pub delivery_failures: BTreeMap<WorkflowToolInvocationId, BlobRef>,
    /// Durable start intents for start-on-call invocations. Pending start
    /// work is recomputable from this map: an entry whose keyed promises
    /// are still pending and which has no terminal start failure must be
    /// (re-)issued; the deterministic execution id makes re-issuing safe.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub start_requests: BTreeMap<WorkflowToolInvocationId, WorkflowToolInvocation>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub start_failures: BTreeMap<WorkflowToolInvocationId, BlobRef>,
}

impl WorkflowToolState {
    pub fn matches_managed_declaration(
        &self,
        session_universe_id: Uuid,
        declaration: &ManagedSessionWorkflowTools,
    ) -> Result<bool, DomainError> {
        let expected = declaration.creation_fingerprint(session_universe_id)?;
        Ok(self.session_universe_id == Some(session_universe_id)
            && self.managed_creation_fingerprint.as_deref() == Some(expected.as_str()))
    }

    pub fn binding_for_tool_name(&self, tool_name: &ToolName) -> Option<&WorkflowToolBinding> {
        self.bindings
            .values()
            .find(|binding| &binding.definition.tool.name == tool_name)
    }

    /// Per-run/per-tool invocation count across both bound emissions and
    /// start requests; the deterministic cap covers them uniformly.
    pub fn emission_count(&self, run_id: RunId, tool_id: &WorkflowToolId) -> u32 {
        self.emissions
            .values()
            .chain(self.start_requests.values())
            .filter(|invocation| invocation.run_id == run_id && &invocation.tool_id == tool_id)
            .count()
            .try_into()
            .unwrap_or(u32::MAX)
    }
}

/// Project the workflow-tool invocations for one receiver and run from the
/// durable session log.
///
/// Results retain session-log order. Bindings are learned only from durable
/// configuration facts encountered before an invocation, so registry changes
/// cannot retarget historical emissions. Invocations inherited by a session
/// fork are ignored because their embedded session id names the source
/// session.
pub fn read_tool_emissions(
    entries: &[StoredSessionEntry],
    receiver_endpoint: &WorkflowEndpointRef,
    session_id: &SessionId,
    run_id: RunId,
) -> Result<Vec<WorkflowToolInvocation>, ReadToolEmissionsError> {
    receiver_endpoint
        .validate()
        .map_err(|error| ReadToolEmissionsError::InvalidReceiver {
            message: error.to_string(),
        })?;

    let mut projection = WorkflowToolEmissionReadProjection {
        receiver_endpoint,
        session_id,
        run_id,
        bindings: BTreeMap::new(),
        receiver_bound: false,
        seen_invocations: BTreeSet::new(),
        emissions: Vec::new(),
    };
    let mut reduced = crate::CoreAgentState::new();
    for entry in entries {
        let decoded = CoreAgentCodec.decode_entry(entry)?;
        crate::apply_event(&mut reduced, &decoded).map_err(|error| {
            ReadToolEmissionsError::InvalidSessionLog {
                message: error.to_string(),
            }
        })?;
        projection.observe(&decoded)?;
    }
    projection.finish()
}

struct WorkflowToolEmissionReadProjection<'a> {
    receiver_endpoint: &'a WorkflowEndpointRef,
    session_id: &'a SessionId,
    run_id: RunId,
    bindings: BTreeMap<String, WorkflowToolBinding>,
    receiver_bound: bool,
    seen_invocations: BTreeSet<WorkflowToolInvocationId>,
    emissions: Vec<WorkflowToolInvocation>,
}

impl WorkflowToolEmissionReadProjection<'_> {
    fn observe(&mut self, entry: &CoreAgentEntry) -> Result<(), ReadToolEmissionsError> {
        match &entry.event {
            CoreAgentEvent::WorkflowToolConfig(event) => self.observe_config(event)?,
            CoreAgentEvent::WorkflowTool(WorkflowToolEvent::Emitted { invocation })
                if invocation.session_id == *self.session_id =>
            {
                let binding = self
                    .bindings
                    .get(&invocation.binding_fingerprint)
                    .ok_or_else(|| ReadToolEmissionsError::UnknownBinding {
                        invocation_id: invocation.invocation_id.clone(),
                        binding_fingerprint: invocation.binding_fingerprint.clone(),
                    })?;
                validate_invocation_against_binding(binding, invocation)
                    .and_then(|()| require_bound_target(binding))
                    .map_err(|error| ReadToolEmissionsError::InvocationBindingMismatch {
                        invocation_id: invocation.invocation_id.clone(),
                        message: error.to_string(),
                    })?;
                let expected_id = WorkflowToolInvocationId::for_call(
                    invocation.session_universe_id,
                    &invocation.session_id,
                    invocation.run_id,
                    invocation.turn_id,
                    invocation.tool_batch_id,
                    &invocation.tool_call_id,
                    &invocation.binding_fingerprint,
                );
                if invocation.invocation_id != expected_id {
                    return Err(ReadToolEmissionsError::InvocationBindingMismatch {
                        invocation_id: invocation.invocation_id.clone(),
                        message: "invocation id is not canonical".to_owned(),
                    });
                }
                if entry.joins.run_id != Some(invocation.run_id)
                    || entry.joins.turn_id != Some(invocation.turn_id)
                    || entry.joins.tool_batch_id != Some(invocation.tool_batch_id)
                    || entry.joins.tool_call_id.as_ref() != Some(&invocation.tool_call_id)
                {
                    return Err(ReadToolEmissionsError::InvocationJoinMismatch {
                        invocation_id: invocation.invocation_id.clone(),
                    });
                }
                if !self
                    .seen_invocations
                    .insert(invocation.invocation_id.clone())
                {
                    return Err(ReadToolEmissionsError::DuplicateInvocation {
                        invocation_id: invocation.invocation_id.clone(),
                    });
                }
                if invocation.run_id == self.run_id
                    && binding.bound_receiver() == Some(self.receiver_endpoint)
                {
                    if self.emissions.len() >= MAX_WORKFLOW_TOOL_EMISSIONS_PER_READ {
                        return Err(ReadToolEmissionsError::ResultLimitExceeded {
                            limit: MAX_WORKFLOW_TOOL_EMISSIONS_PER_READ,
                        });
                    }
                    self.emissions.push(invocation.clone());
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn observe_config(
        &mut self,
        event: &WorkflowToolConfigEvent,
    ) -> Result<(), ReadToolEmissionsError> {
        match event {
            WorkflowToolConfigEvent::ManagedBindingsAdmitted {
                session_universe_id,
                declaration_version,
                lifecycle_controller,
                creation_fingerprint: observed_creation_fingerprint,
                bindings,
            } => {
                if *declaration_version != MANAGED_TOOL_DECLARATION_VERSION {
                    return Err(ReadToolEmissionsError::InvalidBinding {
                        binding_fingerprint: observed_creation_fingerprint.clone(),
                        message: format!(
                            "unsupported managed-session declaration version {declaration_version}"
                        ),
                    });
                }
                if let Some(controller) = lifecycle_controller {
                    controller.validate().map_err(|error| {
                        ReadToolEmissionsError::InvalidBinding {
                            binding_fingerprint: observed_creation_fingerprint.clone(),
                            message: error.to_string(),
                        }
                    })?;
                }
                let expected_creation_fingerprint = creation_fingerprint(
                    *session_universe_id,
                    *declaration_version,
                    lifecycle_controller.as_ref(),
                    bindings,
                )
                .map_err(|error| ReadToolEmissionsError::InvalidBinding {
                    binding_fingerprint: observed_creation_fingerprint.clone(),
                    message: error.to_string(),
                })?;
                if observed_creation_fingerprint != &expected_creation_fingerprint {
                    return Err(ReadToolEmissionsError::InvalidBinding {
                        binding_fingerprint: observed_creation_fingerprint.clone(),
                        message: "managed-session creation fingerprint does not match".to_owned(),
                    });
                }

                for binding in bindings {
                    binding
                        .validate()
                        .map_err(|error| ReadToolEmissionsError::InvalidBinding {
                            binding_fingerprint: binding.binding_fingerprint.clone(),
                            message: error.to_string(),
                        })?;
                    validate_controller_self_receiver(binding, lifecycle_controller.as_ref())
                        .map_err(|error| ReadToolEmissionsError::InvalidBinding {
                            binding_fingerprint: binding.binding_fingerprint.clone(),
                            message: error.to_string(),
                        })?;
                    if binding.session_universe_id != *session_universe_id {
                        return Err(ReadToolEmissionsError::InvalidBinding {
                            binding_fingerprint: binding.binding_fingerprint.clone(),
                            message: "binding source universe differs from its managed-session declaration"
                                .to_owned(),
                        });
                    }
                    if binding.bound_receiver() == Some(self.receiver_endpoint) {
                        self.receiver_bound = true;
                    }
                    match self
                        .bindings
                        .insert(binding.binding_fingerprint.clone(), binding.clone())
                    {
                        Some(existing) if existing != *binding => {
                            return Err(ReadToolEmissionsError::InvalidBinding {
                                binding_fingerprint: binding.binding_fingerprint.clone(),
                                message: "fingerprint identifies more than one durable binding"
                                    .to_owned(),
                            });
                        }
                        _ => {}
                    }
                }
            }
            WorkflowToolConfigEvent::SystemBindingAdmitted { binding } => {
                binding
                    .validate()
                    .map_err(|error| ReadToolEmissionsError::InvalidBinding {
                        binding_fingerprint: binding.binding_fingerprint.clone(),
                        message: error.to_string(),
                    })?;
                if binding.bound_receiver() == Some(self.receiver_endpoint) {
                    self.receiver_bound = true;
                }
                match self.bindings.insert(
                    binding.binding_fingerprint.clone(),
                    binding.as_ref().clone(),
                ) {
                    Some(existing) if existing != **binding => {
                        return Err(ReadToolEmissionsError::InvalidBinding {
                            binding_fingerprint: binding.binding_fingerprint.clone(),
                            message: "fingerprint identifies more than one durable binding"
                                .to_owned(),
                        });
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<Vec<WorkflowToolInvocation>, ReadToolEmissionsError> {
        if !self.receiver_bound {
            return Err(ReadToolEmissionsError::ReceiverNotBound {
                workflow_id: self.receiver_endpoint.workflow_id.clone(),
            });
        }
        Ok(self.emissions)
    }
}

pub(crate) fn apply_config_event(
    state: &mut crate::CoreAgentState,
    event: &WorkflowToolConfigEvent,
) -> Result<(), DomainError> {
    match event {
        WorkflowToolConfigEvent::ManagedBindingsAdmitted {
            session_universe_id,
            declaration_version,
            lifecycle_controller,
            creation_fingerprint: observed_creation_fingerprint,
            bindings,
        } => {
            if state.lifecycle.status != crate::CoreAgentStatus::Open {
                return Err(DomainError::InvariantViolation(
                    "managed-session workflow bindings can only be admitted to an open session"
                        .to_owned(),
                ));
            }
            if state.workflow_tools.session_universe_id.is_some()
                || state.workflow_tools.lifecycle_controller.is_some()
                || state.workflow_tools.managed_creation_fingerprint.is_some()
                || !state.workflow_tools.bindings.is_empty()
            {
                return Err(DomainError::InvariantViolation(
                    "managed-session workflow bindings are immutable after session creation"
                        .to_owned(),
                ));
            }
            if *declaration_version != MANAGED_TOOL_DECLARATION_VERSION {
                return Err(DomainError::InvariantViolation(format!(
                    "unsupported managed-session workflow tool declaration version {declaration_version}"
                )));
            }
            if let Some(controller) = lifecycle_controller {
                controller.validate()?;
            }

            let mut previous_tool_id: Option<&WorkflowToolId> = None;
            let mut tool_names = BTreeSet::new();
            let mut binding_map = BTreeMap::new();
            for binding in bindings {
                binding.validate()?;
                validate_controller_self_receiver(binding, lifecycle_controller.as_ref())?;
                if binding.session_universe_id != *session_universe_id {
                    return Err(DomainError::InvariantViolation(format!(
                        "workflow tool {} source universe does not match the managed session",
                        binding.definition.tool_id
                    )));
                }
                if previous_tool_id.is_some_and(|previous| previous >= &binding.definition.tool_id)
                {
                    return Err(DomainError::InvariantViolation(
                        "managed-session workflow tool bindings must be unique and sorted by port id"
                            .to_owned(),
                    ));
                }
                previous_tool_id = Some(&binding.definition.tool_id);
                if !tool_names.insert(binding.definition.tool.name.clone()) {
                    return Err(DomainError::InvariantViolation(format!(
                        "managed-session workflow tool bindings contain duplicate tool name {}",
                        binding.definition.tool.name
                    )));
                }
                binding_map.insert(binding.definition.tool_id.clone(), binding.clone());
            }
            if bindings.len() > MAX_MANAGED_TOOLS {
                return Err(DomainError::InvariantViolation(format!(
                    "managed-session workflow binding event contains {} tools, max {}",
                    bindings.len(),
                    MAX_MANAGED_TOOLS
                )));
            }
            let expected_creation_fingerprint = creation_fingerprint(
                *session_universe_id,
                *declaration_version,
                lifecycle_controller.as_ref(),
                bindings,
            )?;
            if observed_creation_fingerprint != &expected_creation_fingerprint {
                return Err(DomainError::InvariantViolation(
                    "managed-session creation fingerprint does not match its durable workflow bindings"
                        .to_owned(),
                ));
            }

            state.workflow_tools.session_universe_id = Some(*session_universe_id);
            state.workflow_tools.lifecycle_controller = lifecycle_controller.clone();
            state.workflow_tools.managed_declaration_version = Some(*declaration_version);
            state.workflow_tools.managed_creation_fingerprint =
                Some(observed_creation_fingerprint.clone());
            state.workflow_tools.bindings = binding_map;
            Ok(())
        }
        WorkflowToolConfigEvent::SystemBindingAdmitted { binding } => {
            if state.lifecycle.status != crate::CoreAgentStatus::Open {
                return Err(DomainError::InvariantViolation(
                    "system workflow bindings can only be admitted to an open session".to_owned(),
                ));
            }
            binding.validate()?;
            let tool_id = &binding.definition.tool_id;
            if state.workflow_tools.bindings.values().any(|existing| {
                existing.definition.tool.name == binding.definition.tool.name
                    && existing.definition.tool_id != *tool_id
            }) {
                return Err(DomainError::InvariantViolation(format!(
                    "system workflow tool {} collides with an existing workflow tool name {}",
                    tool_id, binding.definition.tool.name
                )));
            }
            if let Some(existing) = state.workflow_tools.bindings.get(tool_id) {
                if existing == binding.as_ref()
                    && state.workflow_tools.system_binding_ids.contains(tool_id)
                {
                    return Err(DomainError::InvariantViolation(format!(
                        "duplicate system workflow binding event for {tool_id}"
                    )));
                }
                return Err(DomainError::InvariantViolation(format!(
                    "system workflow tool {tool_id} conflicts with an existing immutable binding"
                )));
            }
            state
                .workflow_tools
                .system_binding_ids
                .insert(tool_id.clone());
            state
                .workflow_tools
                .bindings
                .insert(tool_id.clone(), binding.as_ref().clone());
            Ok(())
        }
    }
}

pub(crate) fn apply_event(
    state: &mut crate::CoreAgentState,
    event: &WorkflowToolEvent,
) -> Result<(), DomainError> {
    match event {
        WorkflowToolEvent::Emitted { invocation } => {
            validate_invocation_against_state(state, invocation)?;
            require_bound_target(
                state
                    .workflow_tools
                    .bindings
                    .get(&invocation.tool_id)
                    .expect("binding was validated above"),
            )?;
            if state
                .workflow_tools
                .emissions
                .contains_key(&invocation.invocation_id)
            {
                return Err(DomainError::InvariantViolation(format!(
                    "workflow tool invocation {} was already emitted",
                    invocation.invocation_id
                )));
            }
            if state
                .workflow_tools
                .emission_count(invocation.run_id, &invocation.tool_id)
                >= MAX_WORKFLOW_TOOL_EMISSIONS_PER_RUN
            {
                return Err(DomainError::InvariantViolation(format!(
                    "workflow tool {} exceeded its per-run emission cap",
                    invocation.tool_id
                )));
            }
            state
                .workflow_tools
                .emissions
                .insert(invocation.invocation_id.clone(), invocation.clone());
            Ok(())
        }
        WorkflowToolEvent::StartRequested {
            invocation,
            execution_id,
        } => {
            validate_invocation_against_state(state, invocation)?;
            let binding = state
                .workflow_tools
                .bindings
                .get(&invocation.tool_id)
                .expect("binding was validated above");
            require_start_target(binding)?;
            let WorkflowToolTarget::Start { start } = &binding.target else {
                unreachable!("start target was required above");
            };
            let expected_execution_id =
                workflow_tool_execution_id(&invocation.invocation_id, &start.recipe_fingerprint);
            if execution_id != &expected_execution_id {
                return Err(DomainError::InvariantViolation(format!(
                    "workflow tool start intent {} execution id is not the canonical derivation",
                    invocation.invocation_id
                )));
            }
            if invocation.completion_promises.is_none() {
                return Err(DomainError::InvariantViolation(format!(
                    "workflow tool start intent {} must carry keyed completion promises",
                    invocation.invocation_id
                )));
            }
            if state
                .workflow_tools
                .start_requests
                .contains_key(&invocation.invocation_id)
            {
                return Err(DomainError::InvariantViolation(format!(
                    "workflow tool start intent {} was already recorded",
                    invocation.invocation_id
                )));
            }
            if state
                .workflow_tools
                .emission_count(invocation.run_id, &invocation.tool_id)
                >= MAX_WORKFLOW_TOOL_EMISSIONS_PER_RUN
            {
                return Err(DomainError::InvariantViolation(format!(
                    "workflow tool {} exceeded its per-run invocation cap",
                    invocation.tool_id
                )));
            }
            state
                .workflow_tools
                .start_requests
                .insert(invocation.invocation_id.clone(), invocation.clone());
            Ok(())
        }
        WorkflowToolEvent::DeliveryFailed {
            invocation_id,
            error_ref,
        } => {
            if !state.workflow_tools.emissions.contains_key(invocation_id) {
                return Err(DomainError::InvariantViolation(format!(
                    "workflow tool delivery failure references unknown invocation {invocation_id}"
                )));
            }
            match state.workflow_tools.delivery_failures.get(invocation_id) {
                Some(existing) if existing == error_ref => Ok(()),
                Some(_) => Err(DomainError::InvariantViolation(format!(
                    "workflow tool invocation {invocation_id} already has a different delivery failure"
                ))),
                None => {
                    state
                        .workflow_tools
                        .delivery_failures
                        .insert(invocation_id.clone(), error_ref.clone());
                    Ok(())
                }
            }
        }
        WorkflowToolEvent::StartFailed {
            invocation_id,
            error_ref,
        } => {
            if !state
                .workflow_tools
                .start_requests
                .contains_key(invocation_id)
            {
                return Err(DomainError::InvariantViolation(format!(
                    "workflow tool start failure references unknown start intent {invocation_id}"
                )));
            }
            match state.workflow_tools.start_failures.get(invocation_id) {
                Some(existing) if existing == error_ref => Ok(()),
                Some(_) => Err(DomainError::InvariantViolation(format!(
                    "workflow tool start intent {invocation_id} already has a different terminal failure"
                ))),
                None => {
                    state
                        .workflow_tools
                        .start_failures
                        .insert(invocation_id.clone(), error_ref.clone());
                    Ok(())
                }
            }
        }
    }
}

pub fn workflow_tool_emit_effect(invocation: &WorkflowToolInvocation) -> ToolEffect {
    let mut data = BTreeMap::new();
    data.insert(
        EFFECT_INVOCATION_ID.to_owned(),
        invocation.invocation_id.as_str().to_owned(),
    );
    data.insert(
        EFFECT_PORT_ID.to_owned(),
        invocation.tool_id.as_str().to_owned(),
    );
    data.insert(
        EFFECT_SEMANTIC_TYPE.to_owned(),
        invocation.semantic_type.clone(),
    );
    data.insert(
        EFFECT_SCHEMA_REVISION.to_owned(),
        invocation.schema_revision.to_string(),
    );
    data.insert(
        EFFECT_BINDING_FINGERPRINT.to_owned(),
        invocation.binding_fingerprint.clone(),
    );
    data.insert(
        EFFECT_SESSION_UNIVERSE_ID.to_owned(),
        invocation.session_universe_id.to_string(),
    );
    data.insert(
        EFFECT_SESSION_ID.to_owned(),
        invocation.session_id.as_str().to_owned(),
    );
    data.insert(EFFECT_RUN_ID.to_owned(), invocation.run_id.to_string());
    data.insert(EFFECT_TURN_ID.to_owned(), invocation.turn_id.to_string());
    data.insert(
        EFFECT_TOOL_BATCH_ID.to_owned(),
        invocation.tool_batch_id.to_string(),
    );
    data.insert(
        EFFECT_TOOL_CALL_ID.to_owned(),
        invocation.tool_call_id.as_str().to_owned(),
    );
    data.insert(
        EFFECT_ARGUMENTS_REF.to_owned(),
        invocation.arguments_ref.as_str().to_owned(),
    );
    if let Some(context_ref) = &invocation.execution_context_ref {
        data.insert(
            EFFECT_EXECUTION_CONTEXT_REF.to_owned(),
            context_ref.as_str().to_owned(),
        );
    }
    if let Some(promises) = &invocation.completion_promises {
        let encoded: BTreeMap<&str, &str> = promises
            .iter()
            .map(|(key, promise_id)| (key.as_str(), promise_id.as_str()))
            .collect();
        data.insert(
            EFFECT_COMPLETION_PROMISES.to_owned(),
            serde_json::to_string(&encoded).expect("completion promise map encodes"),
        );
    }
    ToolEffect {
        kind: WORKFLOW_TOOL_EMIT_EFFECT_KIND.to_owned(),
        data,
    }
}

/// Attach the absolute per-promise deadline (computed by the runtime at
/// invocation from the binding's trusted relative deadline) to an emit
/// effect. The deadline lives on the created promises, not on the durable
/// invocation record.
pub fn with_completion_deadline(mut effect: ToolEffect, deadline_ms: Option<u64>) -> ToolEffect {
    if let Some(deadline_ms) = deadline_ms {
        effect.data.insert(
            EFFECT_COMPLETION_DEADLINE_MS.to_owned(),
            deadline_ms.to_string(),
        );
    }
    effect
}

pub(crate) fn completion_deadline_from_emit_effect(
    effect: &ToolEffect,
) -> Result<Option<u64>, DomainError> {
    effect
        .data
        .get(EFFECT_COMPLETION_DEADLINE_MS)
        .map(|value| {
            value.parse::<u64>().map_err(|_| {
                DomainError::InvariantViolation(
                    "workflow tool emit effect completion deadline is not a u64".to_owned(),
                )
            })
        })
        .transpose()
}

pub(crate) fn invocation_from_emit_effect(
    effect: &ToolEffect,
) -> Result<Option<WorkflowToolInvocation>, DomainError> {
    if effect.kind != WORKFLOW_TOOL_EMIT_EFFECT_KIND {
        return Ok(None);
    }
    let field = |key: &str| {
        effect.data.get(key).cloned().ok_or_else(|| {
            DomainError::InvariantViolation(format!("workflow tool emit effect is missing `{key}`"))
        })
    };
    let parse_u64 = |key: &str, value: String| {
        value.parse::<u64>().map_err(|_| {
            DomainError::InvariantViolation(format!(
                "workflow tool emit effect `{key}` is not a u64"
            ))
        })
    };
    let invocation_id =
        WorkflowToolInvocationId::try_new(field(EFFECT_INVOCATION_ID)?).map_err(|error| {
            DomainError::InvariantViolation(format!(
                "workflow tool emit effect has invalid invocation id: {error}"
            ))
        })?;
    let tool_id = WorkflowToolId::try_new(field(EFFECT_PORT_ID)?).map_err(|error| {
        DomainError::InvariantViolation(format!(
            "workflow tool emit effect has invalid tool id: {error}"
        ))
    })?;
    let session_universe_id =
        Uuid::parse_str(&field(EFFECT_SESSION_UNIVERSE_ID)?).map_err(|error| {
            DomainError::InvariantViolation(format!(
                "workflow tool emit effect has invalid source universe: {error}"
            ))
        })?;
    let session_id = SessionId::try_new(field(EFFECT_SESSION_ID)?).map_err(|error| {
        DomainError::InvariantViolation(format!(
            "workflow tool emit effect has invalid session id: {error}"
        ))
    })?;
    let tool_call_id = ToolCallId::try_new(field(EFFECT_TOOL_CALL_ID)?).map_err(|error| {
        DomainError::InvariantViolation(format!(
            "workflow tool emit effect has invalid tool call id: {error}"
        ))
    })?;
    let arguments_ref = BlobRef::parse(field(EFFECT_ARGUMENTS_REF)?).map_err(|error| {
        DomainError::InvariantViolation(format!(
            "workflow tool emit effect has invalid arguments ref: {error}"
        ))
    })?;
    let execution_context_ref = effect
        .data
        .get(EFFECT_EXECUTION_CONTEXT_REF)
        .map(|value| {
            BlobRef::parse(value.clone()).map_err(|error| {
                DomainError::InvariantViolation(format!(
                    "workflow tool emit effect has invalid execution context ref: {error}"
                ))
            })
        })
        .transpose()?;
    let completion_promises = effect
        .data
        .get(EFFECT_COMPLETION_PROMISES)
        .map(|value| {
            serde_json::from_str::<BTreeMap<String, String>>(value).map_err(|error| {
                DomainError::InvariantViolation(format!(
                    "workflow tool emit effect completion promise map is invalid: {error}"
                ))
            })
        })
        .transpose()?
        .map(|decoded| {
            decoded
                .into_iter()
                .map(|(key, promise_id)| {
                    PromiseId::try_new(promise_id)
                        .map(|promise_id| (key, promise_id))
                        .map_err(|error| {
                            DomainError::InvariantViolation(format!(
                                "workflow tool emit effect has an invalid completion promise id: {error}"
                            ))
                        })
                })
                .collect::<Result<BTreeMap<_, _>, _>>()
        })
        .transpose()?;

    Ok(Some(WorkflowToolInvocation {
        invocation_id,
        tool_id,
        semantic_type: field(EFFECT_SEMANTIC_TYPE)?,
        schema_revision: parse_u64(EFFECT_SCHEMA_REVISION, field(EFFECT_SCHEMA_REVISION)?)?
            .try_into()
            .map_err(|_| {
                DomainError::InvariantViolation(
                    "workflow tool emit effect schema revision exceeds u32".to_owned(),
                )
            })?,
        binding_fingerprint: field(EFFECT_BINDING_FINGERPRINT)?,
        session_universe_id,
        session_id,
        run_id: RunId::new(parse_u64(EFFECT_RUN_ID, field(EFFECT_RUN_ID)?)?),
        turn_id: TurnId::new(parse_u64(EFFECT_TURN_ID, field(EFFECT_TURN_ID)?)?),
        tool_batch_id: ToolBatchId::new(parse_u64(
            EFFECT_TOOL_BATCH_ID,
            field(EFFECT_TOOL_BATCH_ID)?,
        )?),
        tool_call_id,
        arguments_ref,
        execution_context_ref,
        completion_promises,
    }))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_emit_effect(
    state: &crate::CoreAgentState,
    expected_session_id: &SessionId,
    expected_run_id: RunId,
    expected_turn_id: TurnId,
    expected_batch_id: ToolBatchId,
    expected_call_id: &ToolCallId,
    invocation: &WorkflowToolInvocation,
    pending_emissions_for_port: u32,
) -> Result<(), DomainError> {
    if &invocation.session_id != expected_session_id
        || invocation.run_id != expected_run_id
        || invocation.turn_id != expected_turn_id
        || invocation.tool_batch_id != expected_batch_id
        || &invocation.tool_call_id != expected_call_id
    {
        return Err(DomainError::InvariantViolation(
            "workflow tool emit effect does not match its session/run/turn/batch/call joins"
                .to_owned(),
        ));
    }
    validate_invocation_binding(state, invocation)?;
    let active_run = state.runs.active.as_ref().ok_or_else(|| {
        DomainError::InvariantViolation(
            "workflow tool emit effect requires an active run".to_owned(),
        )
    })?;
    let batch = active_run
        .tool_batches
        .get(&expected_batch_id)
        .ok_or_else(|| {
            DomainError::InvariantViolation(format!(
                "workflow tool emit effect references missing tool batch {expected_batch_id}"
            ))
        })?;
    let call = batch
        .calls
        .iter()
        .find(|call| &call.call.call_id == expected_call_id)
        .ok_or_else(|| {
            DomainError::InvariantViolation(format!(
                "workflow tool emit effect references missing tool call {expected_call_id}"
            ))
        })?;
    let binding = state
        .workflow_tools
        .bindings
        .get(&invocation.tool_id)
        .expect("binding was validated above");
    if call.call.tool_id.as_ref() != Some(&binding.definition.tool.name)
        || call.call.arguments_ref != invocation.arguments_ref
    {
        return Err(DomainError::InvariantViolation(
            "workflow tool emit effect does not match its admitted tool identity and arguments"
                .to_owned(),
        ));
    }
    let expected_id = WorkflowToolInvocationId::for_call(
        invocation.session_universe_id,
        &invocation.session_id,
        invocation.run_id,
        invocation.turn_id,
        invocation.tool_batch_id,
        &invocation.tool_call_id,
        &invocation.binding_fingerprint,
    );
    if invocation.invocation_id != expected_id {
        return Err(DomainError::InvariantViolation(
            "workflow tool invocation id does not match its durable call identity".to_owned(),
        ));
    }
    let existing = state
        .workflow_tools
        .emission_count(invocation.run_id, &invocation.tool_id);
    if existing.saturating_add(pending_emissions_for_port) >= MAX_WORKFLOW_TOOL_EMISSIONS_PER_RUN {
        return Err(DomainError::InvariantViolation(format!(
            "workflow tool {} exceeded its per-run emission cap",
            invocation.tool_id
        )));
    }
    Ok(())
}

fn validate_invocation_binding(
    state: &crate::CoreAgentState,
    invocation: &WorkflowToolInvocation,
) -> Result<(), DomainError> {
    let binding = state
        .workflow_tools
        .bindings
        .get(&invocation.tool_id)
        .ok_or_else(|| {
            DomainError::InvariantViolation(format!(
                "workflow tool invocation references unknown tool {}",
                invocation.tool_id
            ))
        })?;
    validate_invocation_against_binding(binding, invocation)
}

fn validate_invocation_against_binding(
    binding: &WorkflowToolBinding,
    invocation: &WorkflowToolInvocation,
) -> Result<(), DomainError> {
    if invocation.session_universe_id != binding.session_universe_id
        || invocation.semantic_type != binding.definition.semantic_type
        || invocation.schema_revision != binding.definition.revision
        || invocation.binding_fingerprint != binding.binding_fingerprint
        || invocation.tool_id != binding.definition.tool_id
    {
        return Err(DomainError::InvariantViolation(format!(
            "workflow tool invocation {} does not match its durable binding",
            invocation.invocation_id
        )));
    }
    validate_completion_promises(binding, invocation)
}

/// Bound-receiver emissions and start intents each require the matching
/// target lifecycle on the durable binding.
fn require_bound_target(binding: &WorkflowToolBinding) -> Result<(), DomainError> {
    if !matches!(binding.target, WorkflowToolTarget::Bound { .. }) {
        return Err(DomainError::InvariantViolation(format!(
            "workflow tool {} start-on-call targets do not produce bound-receiver emissions",
            binding.definition.tool_id
        )));
    }
    Ok(())
}

fn require_start_target(binding: &WorkflowToolBinding) -> Result<(), DomainError> {
    if !matches!(binding.target, WorkflowToolTarget::Start { .. }) {
        return Err(DomainError::InvariantViolation(format!(
            "workflow tool {} bound targets do not produce start intents",
            binding.definition.tool_id
        )));
    }
    Ok(())
}

/// The completion promise map must exactly match the binding's declared
/// completion mode: absent for `Accepted`, the single reserved key for
/// `Joined`, and for `Promises` a non-empty keyed set within the cap with
/// a distinct promise id per key. Promise ids are session counters minted
/// by the executor from the batch's base; the drive checks them against
/// the batch when it turns the effect into events.
fn validate_completion_promises(
    binding: &WorkflowToolBinding,
    invocation: &WorkflowToolInvocation,
) -> Result<(), DomainError> {
    match (&binding.completion, &invocation.completion_promises) {
        (WorkflowToolCompletion::Accepted, None) => Ok(()),
        (WorkflowToolCompletion::Accepted, Some(_)) => Err(DomainError::InvariantViolation(
            "notify-only workflow tool invocation must not include completion promises".to_owned(),
        )),
        (WorkflowToolCompletion::Joined { .. } | WorkflowToolCompletion::Promises { .. }, None) => {
            Err(DomainError::InvariantViolation(
                "promise-bearing workflow tool invocation is missing its completion promises"
                    .to_owned(),
            ))
        }
        (WorkflowToolCompletion::Joined { .. }, Some(promises)) => {
            if promises.len() != 1 || !promises.contains_key(REPLY_COMPLETION_KEY) {
                return Err(DomainError::InvariantViolation(format!(
                    "joined workflow tool invocation must use the single reserved completion key `{REPLY_COMPLETION_KEY}`"
                )));
            }
            Ok(())
        }
        (
            WorkflowToolCompletion::Promises {
                max_promises,
                key_source,
                ..
            },
            Some(promises),
        ) => {
            if promises.is_empty() {
                return Err(DomainError::InvariantViolation(
                    "promise-bearing workflow tool invocation must create at least one promise"
                        .to_owned(),
                ));
            }
            if promises.len() as u32 > *max_promises {
                return Err(DomainError::InvariantViolation(format!(
                    "workflow tool invocation creates {} promises, binding allows {max_promises}",
                    promises.len()
                )));
            }
            if matches!(key_source, WorkflowToolCompletionKeySource::Reply)
                && (promises.len() != 1 || !promises.contains_key(REPLY_COMPLETION_KEY))
            {
                return Err(DomainError::InvariantViolation(format!(
                    "workflow tool reply key source must use the single reserved completion key `{REPLY_COMPLETION_KEY}`"
                )));
            }
            let mut seen = BTreeSet::new();
            for (key, promise_id) in promises {
                validate_completion_key(key)?;
                if !seen.insert(promise_id) {
                    return Err(DomainError::InvariantViolation(format!(
                        "workflow tool completion promise {promise_id} is used for more than one key"
                    )));
                }
            }
            Ok(())
        }
    }
}

fn validate_invocation_against_state(
    state: &crate::CoreAgentState,
    invocation: &WorkflowToolInvocation,
) -> Result<(), DomainError> {
    validate_invocation_binding(state, invocation)?;
    let expected_id = WorkflowToolInvocationId::for_call(
        invocation.session_universe_id,
        &invocation.session_id,
        invocation.run_id,
        invocation.turn_id,
        invocation.tool_batch_id,
        &invocation.tool_call_id,
        &invocation.binding_fingerprint,
    );
    if invocation.invocation_id != expected_id {
        return Err(DomainError::InvariantViolation(
            "workflow tool emitted event has a non-canonical invocation id".to_owned(),
        ));
    }
    let active_run = state.runs.active.as_ref().ok_or_else(|| {
        DomainError::InvariantViolation(
            "workflow tool invocation can only be emitted for an active run".to_owned(),
        )
    })?;
    if active_run.run_id != invocation.run_id {
        return Err(DomainError::InvariantViolation(
            "workflow tool invocation does not match the active run".to_owned(),
        ));
    }
    let batch = active_run
        .tool_batches
        .get(&invocation.tool_batch_id)
        .ok_or_else(|| {
            DomainError::InvariantViolation(format!(
                "workflow tool invocation references missing tool batch {}",
                invocation.tool_batch_id
            ))
        })?;
    if batch.turn_id != invocation.turn_id {
        return Err(DomainError::InvariantViolation(
            "workflow tool invocation does not match its tool batch turn".to_owned(),
        ));
    }
    let call = batch
        .calls
        .iter()
        .find(|call| call.call.call_id == invocation.tool_call_id)
        .ok_or_else(|| {
            DomainError::InvariantViolation(format!(
                "workflow tool invocation references missing tool call {}",
                invocation.tool_call_id
            ))
        })?;
    let binding = state
        .workflow_tools
        .bindings
        .get(&invocation.tool_id)
        .expect("binding was validated above");
    if call.call.tool_id.as_ref() != Some(&binding.definition.tool.name)
        || call.call.arguments_ref != invocation.arguments_ref
    {
        return Err(DomainError::InvariantViolation(
            "workflow tool invocation does not match its durable tool call".to_owned(),
        ));
    }
    match &binding.completion {
        WorkflowToolCompletion::Joined { .. } => {
            if call.status != crate::ToolCallStatus::Pending {
                return Err(DomainError::InvariantViolation(
                    "joined workflow tool invocation requires its original call to remain pending"
                        .to_owned(),
                ));
            }
            let Some(parked) = active_run.parked_tool_batch.as_ref() else {
                return Err(DomainError::InvariantViolation(
                    "joined workflow tool invocation requires a parked tool batch".to_owned(),
                ));
            };
            let crate::ToolBatchSuspension::JoinedWorkflowCalls { calls, .. } = &parked.suspension
            else {
                return Err(DomainError::InvariantViolation(
                    "joined workflow tool invocation requires a joined-workflow suspension"
                        .to_owned(),
                ));
            };
            if parked.batch_id != invocation.tool_batch_id
                || !calls.iter().any(|joined| {
                    joined.call_id == invocation.tool_call_id
                        && joined.invocation_id == invocation.invocation_id
                        && invocation
                            .completion_promises
                            .as_ref()
                            .and_then(|promises| promises.get(REPLY_COMPLETION_KEY))
                            == Some(&joined.promise_id)
                })
            {
                return Err(DomainError::InvariantViolation(
                    "joined workflow tool invocation is missing its durable parked mapping"
                        .to_owned(),
                ));
            }
        }
        WorkflowToolCompletion::Accepted | WorkflowToolCompletion::Promises { .. } => {
            if call.status != crate::ToolCallStatus::Succeeded {
                return Err(DomainError::InvariantViolation(
                    "workflow tool invocation does not match a successful durable tool call"
                        .to_owned(),
                ));
            }
        }
    }
    if let Some(promises) = &invocation.completion_promises {
        // Promise::Created events precede Emitted in the same append, so
        // every keyed completion promise must already exist with the exact
        // producer-authorized workflow source.
        for (key, promise_id) in promises {
            let promise = state.promises.promises.get(promise_id).ok_or_else(|| {
                DomainError::InvariantViolation(format!(
                    "workflow tool invocation references missing completion promise for key `{key}`"
                ))
            })?;
            let expected_source = completion_promise_source(binding, invocation, key)?;
            let expected_ownership =
                if matches!(binding.completion, WorkflowToolCompletion::Joined { .. }) {
                    crate::PromiseOwnership::Runtime
                } else {
                    crate::PromiseOwnership::Model
                };
            if promise.source != expected_source || promise.ownership != expected_ownership {
                return Err(DomainError::InvariantViolation(format!(
                    "workflow tool completion promise for key `{key}` has mismatched source or ownership"
                )));
            }
        }
    }
    Ok(())
}

/// The canonical promise source for one keyed completion promise: for bound
/// targets the admitted receiver endpoint, for start targets the
/// system-derived execution — in both cases the only producer authorized to
/// resolve it.
pub fn completion_promise_source(
    binding: &WorkflowToolBinding,
    invocation: &WorkflowToolInvocation,
    key: &str,
) -> Result<crate::PromiseSource, DomainError> {
    let (producer_workflow_id, producer_workflow_kind) = match &binding.target {
        WorkflowToolTarget::Bound { receiver, .. } => {
            (receiver.workflow_id.clone(), receiver.workflow_kind.clone())
        }
        WorkflowToolTarget::Start { start } => (
            workflow_tool_execution_id(&invocation.invocation_id, &start.recipe_fingerprint),
            WORKFLOW_TOOL_EXECUTION_KIND.to_owned(),
        ),
    };
    Ok(crate::PromiseSource::Workflow {
        producer_workflow_id,
        producer_workflow_kind,
        invocation_id: invocation.invocation_id.as_str().to_owned(),
        completion_key: key.to_owned(),
    })
}

impl WorkflowToolInvocationId {
    #[allow(clippy::too_many_arguments)]
    pub fn for_call(
        session_universe_id: Uuid,
        session_id: &SessionId,
        run_id: RunId,
        turn_id: TurnId,
        tool_batch_id: ToolBatchId,
        tool_call_id: &ToolCallId,
        binding_fingerprint: &str,
    ) -> Self {
        let digest = digest_fields(
            INVOCATION_ID_DOMAIN,
            &[
                session_universe_id.as_bytes(),
                session_id.as_str().as_bytes(),
                &run_id.as_u64().to_be_bytes(),
                &turn_id.as_u64().to_be_bytes(),
                &tool_batch_id.as_u64().to_be_bytes(),
                tool_call_id.as_str().as_bytes(),
                binding_fingerprint.as_bytes(),
            ],
        );
        Self::new(format!("wti:sha256:{}", hex::encode(digest)))
    }
}

fn validate_semantic_type(value: &str) -> Result<(), DomainError> {
    validate_component(
        "workflow tool semantic type",
        value,
        SEMANTIC_TYPE_MAX_LEN,
        "ASCII letters, digits, '_', '-', '.'",
        |ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'),
    )?;
    let segments: Vec<_> = value.split('.').collect();
    let version = segments.last().copied().unwrap_or_default();
    if segments.len() < 3
        || segments.iter().any(|segment| segment.is_empty())
        || version.len() < 2
        || !version.starts_with('v')
        || !version[1..].bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(DomainError::InvariantViolation(
            "workflow tool semantic type must be a dotted identifier ending in '.v<digits>'"
                .to_owned(),
        ));
    }
    if value == RESERVED_RUN_TERMINAL_SEMANTIC_TYPE {
        return Err(DomainError::InvariantViolation(format!(
            "workflow tool semantic type {value} is reserved by the emission substrate"
        )));
    }
    Ok(())
}

fn validate_component(
    kind: &'static str,
    value: &str,
    max_len: usize,
    allowed: &'static str,
    allowed_char: impl Fn(char) -> bool,
) -> Result<(), DomainError> {
    if value.is_empty() {
        return Err(DomainError::InvariantViolation(format!(
            "{kind} must not be empty"
        )));
    }
    if value.len() > max_len {
        return Err(DomainError::InvariantViolation(format!(
            "{kind} is too long: {} bytes, max {max_len}",
            value.len()
        )));
    }
    if !value
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
    {
        return Err(DomainError::InvariantViolation(format!(
            "{kind} must start with an ASCII letter or digit"
        )));
    }
    for (index, ch) in value.char_indices() {
        if !allowed_char(ch) {
            return Err(DomainError::InvariantViolation(format!(
                "{kind} contains invalid character {ch:?} at byte {index}; allowed: {allowed}"
            )));
        }
    }
    Ok(())
}

fn binding_fingerprint(
    session_universe_id: Uuid,
    definition: &WorkflowToolDefinition,
    target: &WorkflowToolTarget,
    completion: &WorkflowToolCompletion,
) -> Result<String, DomainError> {
    let mut hasher = canonical_fingerprint_hasher(BINDING_FINGERPRINT_DOMAIN);
    update_digest_part(&mut hasher, session_universe_id.as_bytes());
    update_definition_fingerprint(&mut hasher, definition)?;
    update_target_fingerprint(&mut hasher, target);
    update_completion_fingerprint(&mut hasher, completion);
    Ok(format!("wtb:sha256:{}", hex::encode(hasher.finalize())))
}

fn update_target_fingerprint(hasher: &mut Sha256, target: &WorkflowToolTarget) {
    match target {
        WorkflowToolTarget::Bound { receiver, dispatch } => {
            update_digest_part(hasher, b"bound");
            update_endpoint_fingerprint(hasher, receiver);
            update_digest_part(
                hasher,
                match dispatch {
                    BoundWorkflowToolDispatch::Pull => b"pull",
                    BoundWorkflowToolDispatch::Push => b"push",
                },
            );
        }
        WorkflowToolTarget::Start { start } => {
            update_digest_part(hasher, b"start");
            update_digest_part(hasher, &start.recipe_format.to_be_bytes());
            update_digest_part(hasher, &start.revision.to_be_bytes());
            update_digest_part(hasher, start.recipe_ref.as_str().as_bytes());
            update_digest_part(hasher, start.recipe_fingerprint.as_bytes());
        }
    }
}

fn update_completion_fingerprint(hasher: &mut Sha256, completion: &WorkflowToolCompletion) {
    match completion {
        WorkflowToolCompletion::Accepted => {
            update_digest_part(hasher, b"accepted");
        }
        WorkflowToolCompletion::Joined {
            reply_schema_ref,
            deadline_after_ms,
        } => {
            update_digest_part(hasher, b"joined");
            update_optional_part(
                hasher,
                reply_schema_ref
                    .as_ref()
                    .map(|blob_ref| blob_ref.as_str().as_bytes()),
            );
            update_digest_part(hasher, &deadline_after_ms.to_be_bytes());
        }
        WorkflowToolCompletion::Promises {
            reply_schema_ref,
            deadline_after_ms,
            max_promises,
            key_source,
        } => {
            update_digest_part(hasher, b"promises");
            update_optional_part(
                hasher,
                reply_schema_ref
                    .as_ref()
                    .map(|blob_ref| blob_ref.as_str().as_bytes()),
            );
            update_optional_part(
                hasher,
                deadline_after_ms
                    .map(u64::to_be_bytes)
                    .as_ref()
                    .map(|bytes| bytes.as_slice()),
            );
            update_digest_part(hasher, &max_promises.to_be_bytes());
            match key_source {
                WorkflowToolCompletionKeySource::Reply => {
                    update_digest_part(hasher, b"reply");
                }
                WorkflowToolCompletionKeySource::StringArray { pointer } => {
                    update_digest_part(hasher, b"string_array");
                    update_digest_part(hasher, pointer.as_bytes());
                }
                WorkflowToolCompletionKeySource::ArrayItemField { pointer, field } => {
                    update_digest_part(hasher, b"array_item_field");
                    update_digest_part(hasher, pointer.as_bytes());
                    update_digest_part(hasher, field.as_bytes());
                }
                WorkflowToolCompletionKeySource::ArrayIndices { pointer, prefix } => {
                    update_digest_part(hasher, b"array_indices");
                    update_digest_part(hasher, pointer.as_bytes());
                    update_digest_part(hasher, prefix.as_bytes());
                }
            }
        }
    }
}

fn update_optional_part(hasher: &mut Sha256, value: Option<&[u8]>) {
    match value {
        Some(bytes) => {
            update_digest_part(hasher, b"some");
            update_digest_part(hasher, bytes);
        }
        None => update_digest_part(hasher, b"none"),
    }
}

fn creation_fingerprint(
    session_universe_id: Uuid,
    version: u32,
    controller: Option<&WorkflowEndpointRef>,
    bindings: &[WorkflowToolBinding],
) -> Result<String, DomainError> {
    let mut hasher = canonical_fingerprint_hasher(CREATION_FINGERPRINT_DOMAIN);
    update_digest_part(&mut hasher, session_universe_id.as_bytes());
    update_digest_part(&mut hasher, &version.to_be_bytes());
    update_optional_endpoint_fingerprint(&mut hasher, controller);
    update_digest_part(&mut hasher, &(bindings.len() as u64).to_be_bytes());
    for binding in bindings {
        binding.validate()?;
        update_digest_part(&mut hasher, binding.binding_fingerprint.as_bytes());
    }
    Ok(format!("msc:sha256:{}", hex::encode(hasher.finalize())))
}

fn canonical_fingerprint_hasher(domain: &str) -> Sha256 {
    let mut hasher = Sha256::new();
    update_digest_part(&mut hasher, domain.as_bytes());
    update_digest_part(&mut hasher, &FINGERPRINT_ENCODING_VERSION.to_be_bytes());
    hasher
}

fn update_definition_fingerprint(
    hasher: &mut Sha256,
    definition: &WorkflowToolDefinition,
) -> Result<(), DomainError> {
    update_digest_part(hasher, definition.tool_id.as_str().as_bytes());
    update_digest_part(hasher, &definition.revision.to_be_bytes());
    update_digest_part(hasher, definition.semantic_type.as_bytes());
    update_digest_part(hasher, definition.tool.name.as_str().as_bytes());

    if let ToolKind::Builtin(builtin) = &definition.tool.kind {
        update_digest_part(hasher, b"builtin");
        update_digest_part(
            hasher,
            &serde_json::to_vec(builtin).map_err(|error| {
                DomainError::InvariantViolation(format!("encode built-in settings: {error}"))
            })?,
        );
    } else if let ToolKind::Function(function) = &definition.tool.kind {
        update_digest_part(hasher, b"function");
        update_optional_text(
            hasher,
            function.description_ref.as_ref().map(BlobRef::as_str),
        );
        update_digest_part(hasher, function.input_schema_ref.as_str().as_bytes());
        update_optional_text(
            hasher,
            function.output_schema_ref.as_ref().map(BlobRef::as_str),
        );
        update_optional_bool(hasher, function.strict);
        update_optional_text(
            hasher,
            function.provider_options_ref.as_ref().map(BlobRef::as_str),
        );
    } else {
        return Err(DomainError::InvariantViolation(
            "workflow tool fingerprint requires a function definition".to_owned(),
        ));
    }
    update_digest_part(
        hasher,
        match definition.tool.parallelism {
            crate::ToolParallelism::Exclusive => b"exclusive",
            crate::ToolParallelism::ParallelSafe => b"parallel_safe",
        },
    );
    update_digest_part(hasher, b"target_free");
    Ok(())
}

fn update_endpoint_fingerprint(hasher: &mut Sha256, endpoint: &WorkflowEndpointRef) {
    update_digest_part(hasher, endpoint.workflow_id.as_bytes());
    update_digest_part(hasher, endpoint.workflow_kind.as_bytes());
}

fn update_optional_endpoint_fingerprint(
    hasher: &mut Sha256,
    endpoint: Option<&WorkflowEndpointRef>,
) {
    match endpoint {
        Some(endpoint) => {
            update_digest_part(hasher, b"some");
            update_endpoint_fingerprint(hasher, endpoint);
        }
        None => update_digest_part(hasher, b"none"),
    }
}

fn update_optional_text(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            update_digest_part(hasher, b"some");
            update_digest_part(hasher, value.as_bytes());
        }
        None => update_digest_part(hasher, b"none"),
    }
}

fn update_optional_bool(hasher: &mut Sha256, value: Option<bool>) {
    update_digest_part(
        hasher,
        match value {
            None => b"none",
            Some(false) => b"false",
            Some(true) => b"true",
        },
    );
}

fn update_digest_part(hasher: &mut Sha256, field: &[u8]) {
    hasher.update((field.len() as u64).to_be_bytes());
    hasher.update(field);
}

fn digest_fields(domain: &str, fields: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    update_digest_part(&mut hasher, domain.as_bytes());
    for field in fields {
        update_digest_part(&mut hasher, field);
    }
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BlobRef, ContextConfig, ContextEntryInput, ContextEntryKind, ContextMessageRole,
        CoreAgentAction, CoreAgentCodec, CoreAgentCommand, CoreAgentDrive, CoreAgentEntry,
        CoreAgentEvent, CoreAgentLifecycleEvent, CoreAgentState, EventSeq, FunctionToolSpec,
        LlmFinish, LlmGenerationFacts, LlmGenerationResult, LlmGenerationStatus, ModelSelection,
        ObservedToolCall, ProviderApiKind, RunConfig, RunRequestCommand, RunRequestSource,
        SessionConfig, SessionPosition, ToolCallStatus, ToolInvocationBatchRequest,
        ToolInvocationBatchResult, ToolInvocationResult, ToolName, ToolParallelism,
        storage::StoredSessionEntry,
    };

    fn endpoint(workflow_id: &str) -> WorkflowEndpointRef {
        WorkflowEndpointRef {
            workflow_id: workflow_id.to_owned(),
            workflow_kind: "agent_work".to_owned(),
        }
    }

    fn definition(tool_id: &str, tool_name: &str) -> WorkflowToolDefinition {
        WorkflowToolDefinition {
            tool_id: WorkflowToolId::new(tool_id),
            revision: 1,
            semantic_type: "lightspeed.work.report.v1".to_owned(),
            tool: ToolSpec {
                name: ToolName::new(tool_name),
                execution: Default::default(),
                kind: ToolKind::Function(FunctionToolSpec {
                    description_ref: None,
                    input_schema_ref: BlobRef::from_bytes(b"input schema"),
                    output_schema_ref: None,
                    strict: Some(true),
                    provider_options_ref: None,
                }),
                parallelism: ToolParallelism::ParallelSafe,
            },
        }
    }

    fn tool_declaration(
        definition: WorkflowToolDefinition,
        receiver: WorkflowEndpointRef,
    ) -> WorkflowToolDeclaration {
        WorkflowToolDeclaration::bound_notify(definition, receiver)
    }

    fn reply_completion() -> WorkflowToolCompletion {
        reply_completion_with_deadline(None)
    }

    fn reply_completion_with_deadline(deadline_after_ms: Option<u64>) -> WorkflowToolCompletion {
        WorkflowToolCompletion::Promises {
            reply_schema_ref: None,
            deadline_after_ms,
            max_promises: 1,
            key_source: WorkflowToolCompletionKeySource::Reply,
        }
    }

    fn start_ref() -> WorkflowStartRef {
        WorkflowStartRef {
            recipe_format: 1,
            revision: 1,
            recipe_ref: BlobRef::from_bytes(b"recipe"),
            recipe_fingerprint: "recipe-fingerprint".to_owned(),
        }
    }

    #[test]
    fn promise_bearing_binding_to_lifecycle_controller_requires_a_hard_deadline() {
        let universe_id = Uuid::from_u128(1);
        let controller = endpoint("controller::work-1");
        let service = endpoint("service::approvals-1");

        for deadline_after_ms in [None, Some(0)] {
            let to_controller = ManagedSessionWorkflowTools::v1(
                Some(controller.clone()),
                vec![WorkflowToolDeclaration::new(
                    definition("approve", "request_approval"),
                    WorkflowToolTarget::Bound {
                        receiver: controller.clone(),
                        dispatch: BoundWorkflowToolDispatch::Push,
                    },
                    reply_completion_with_deadline(deadline_after_ms),
                )],
            );
            let error = to_controller.admit(universe_id).expect_err(
                "promise-bearing completion bound to the controller needs a hard deadline",
            );
            assert!(
                error
                    .to_string()
                    .contains("non-zero promise deadline_after_ms")
            );
        }

        ManagedSessionWorkflowTools::v1(
            Some(controller.clone()),
            vec![WorkflowToolDeclaration::new(
                definition("approve", "request_approval"),
                WorkflowToolTarget::Bound {
                    receiver: controller.clone(),
                    dispatch: BoundWorkflowToolDispatch::Push,
                },
                reply_completion_with_deadline(Some(30_000)),
            )],
        )
        .admit(universe_id)
        .expect("deadline-backed controller self-receiver admits");

        let to_service = ManagedSessionWorkflowTools::v1(
            Some(controller),
            vec![WorkflowToolDeclaration::new(
                definition("approve", "request_approval"),
                WorkflowToolTarget::Bound {
                    receiver: service,
                    dispatch: BoundWorkflowToolDispatch::Push,
                },
                reply_completion(),
            )],
        );
        to_service
            .admit(universe_id)
            .expect("promise-bearing completion to an independent receiver admits");
    }

    #[test]
    fn durable_self_receiver_binding_rechecks_the_hard_deadline() {
        let universe_id = Uuid::from_u128(1);
        let controller = endpoint("controller::work-1");
        let binding = WorkflowToolBinding::admit(
            universe_id,
            definition("approve", "request_approval"),
            WorkflowToolTarget::Bound {
                receiver: controller.clone(),
                dispatch: BoundWorkflowToolDispatch::Push,
            },
            reply_completion(),
        )
        .expect("binding is valid without controller context");
        let bindings = vec![binding];
        let event = WorkflowToolConfigEvent::ManagedBindingsAdmitted {
            session_universe_id: universe_id,
            declaration_version: MANAGED_TOOL_DECLARATION_VERSION,
            lifecycle_controller: Some(controller.clone()),
            creation_fingerprint: creation_fingerprint(
                universe_id,
                MANAGED_TOOL_DECLARATION_VERSION,
                Some(&controller),
                &bindings,
            )
            .expect("creation fingerprint"),
            bindings,
        };
        let mut state = CoreAgentState::new();
        state.lifecycle.status = crate::CoreAgentStatus::Open;

        let error = apply_config_event(&mut state, &event)
            .expect_err("durable replay must reject a deadline-free controller self-receiver");
        assert!(
            error
                .to_string()
                .contains("non-zero promise deadline_after_ms")
        );
    }

    #[test]
    fn start_target_requires_promise_bearing_completion() {
        let universe_id = Uuid::from_u128(1);
        let fire_and_forget = WorkflowToolBinding::admit(
            universe_id,
            definition("launch", "launch_job"),
            WorkflowToolTarget::Start { start: start_ref() },
            WorkflowToolCompletion::Accepted,
        );
        assert!(
            fire_and_forget.is_err(),
            "fire-and-forget start is deferred"
        );

        WorkflowToolBinding::admit(
            universe_id,
            definition("launch", "launch_job"),
            WorkflowToolTarget::Start { start: start_ref() },
            reply_completion(),
        )
        .expect("start target with promise-bearing completion admits");
    }

    #[test]
    fn completion_and_target_participate_in_binding_identity() {
        let universe_id = Uuid::from_u128(1);
        let receiver = endpoint("service::approvals-1");
        let notify = WorkflowToolBinding::admit_bound_notify(
            universe_id,
            definition("approve", "request_approval"),
            receiver.clone(),
        )
        .expect("notify binding");
        let pushed_notify = WorkflowToolBinding::admit(
            universe_id,
            definition("approve", "request_approval"),
            WorkflowToolTarget::Bound {
                receiver: receiver.clone(),
                dispatch: BoundWorkflowToolDispatch::Push,
            },
            WorkflowToolCompletion::Accepted,
        )
        .expect("pushed notify binding");
        assert_ne!(
            notify.binding_fingerprint,
            pushed_notify.binding_fingerprint
        );
        let request_reply = WorkflowToolBinding::admit(
            universe_id,
            definition("approve", "request_approval"),
            WorkflowToolTarget::Bound {
                receiver,
                dispatch: BoundWorkflowToolDispatch::Push,
            },
            reply_completion(),
        )
        .expect("request/reply binding");
        assert_ne!(
            notify.binding_fingerprint,
            request_reply.binding_fingerprint
        );

        let indexed = WorkflowToolBinding::admit(
            universe_id,
            definition("approve", "request_approval"),
            WorkflowToolTarget::Bound {
                receiver: endpoint("service::approvals-1"),
                dispatch: BoundWorkflowToolDispatch::Push,
            },
            WorkflowToolCompletion::Promises {
                reply_schema_ref: None,
                deadline_after_ms: None,
                max_promises: 4,
                key_source: WorkflowToolCompletionKeySource::ArrayIndices {
                    pointer: "/jobs".to_owned(),
                    prefix: "job-".to_owned(),
                },
            },
        )
        .expect("array-index binding");
        assert_ne!(
            request_reply.binding_fingerprint,
            indexed.binding_fingerprint
        );

        let invalid_reply = WorkflowToolCompletion::Promises {
            reply_schema_ref: None,
            deadline_after_ms: None,
            max_promises: 2,
            key_source: WorkflowToolCompletionKeySource::Reply,
        };
        assert!(invalid_reply.validate().is_err());
    }

    #[test]
    fn pull_dispatch_rejects_promise_completion() {
        let error = WorkflowToolBinding::admit(
            Uuid::from_u128(1),
            definition("approve", "request_approval"),
            WorkflowToolTarget::Bound {
                receiver: endpoint("service::approvals-1"),
                dispatch: BoundWorkflowToolDispatch::Pull,
            },
            reply_completion(),
        )
        .expect_err("pull dispatch cannot strand promise completion");
        assert!(
            error
                .to_string()
                .contains("pull dispatch supports Accepted")
        );
    }

    #[test]
    fn joined_completion_requires_push_or_start_and_a_hard_deadline() {
        let universe_id = Uuid::from_u128(1);
        let receiver = endpoint("service::approvals-1");
        let zero_deadline = WorkflowToolBinding::admit(
            universe_id,
            definition("approve", "request_approval"),
            WorkflowToolTarget::Bound {
                receiver: receiver.clone(),
                dispatch: BoundWorkflowToolDispatch::Push,
            },
            WorkflowToolCompletion::Joined {
                reply_schema_ref: None,
                deadline_after_ms: 0,
            },
        )
        .expect_err("Joined requires a non-zero hard deadline");
        assert!(zero_deadline.to_string().contains("non-zero deadline"));

        let pull = WorkflowToolBinding::admit(
            universe_id,
            definition("approve", "request_approval"),
            WorkflowToolTarget::Bound {
                receiver: receiver.clone(),
                dispatch: BoundWorkflowToolDispatch::Pull,
            },
            WorkflowToolCompletion::Joined {
                reply_schema_ref: None,
                deadline_after_ms: 30_000,
            },
        )
        .expect_err("pull cannot carry Joined completion");
        assert!(pull.to_string().contains("pull dispatch supports Accepted"));

        let pushed = WorkflowToolBinding::admit(
            universe_id,
            definition("approve", "request_approval"),
            WorkflowToolTarget::Bound {
                receiver,
                dispatch: BoundWorkflowToolDispatch::Push,
            },
            WorkflowToolCompletion::Joined {
                reply_schema_ref: None,
                deadline_after_ms: 30_000,
            },
        )
        .expect("pushed Joined binding");
        let started = WorkflowToolBinding::admit(
            universe_id,
            definition("launch", "launch_job"),
            WorkflowToolTarget::Start { start: start_ref() },
            WorkflowToolCompletion::Joined {
                reply_schema_ref: None,
                deadline_after_ms: 30_000,
            },
        )
        .expect("start Joined binding");
        assert_eq!(
            pushed.binding_fingerprint.as_str(),
            "wtb:sha256:4667d89301ad5afc1070b2cf7e830529466e99c42caff74706f8e5ffa995e312"
        );
        assert_eq!(
            started.binding_fingerprint.as_str(),
            "wtb:sha256:231342b0e047d5fec28a87c154fc8750fdcf8b641c5912b5bb19e97ad4814055"
        );
    }

    #[test]
    fn completion_promise_map_matches_the_binding_and_is_distinct() {
        let universe_id = Uuid::from_u128(1);
        let receiver = endpoint("service::approvals-1");
        let binding = WorkflowToolBinding::admit(
            universe_id,
            definition("approve", "request_approval"),
            WorkflowToolTarget::Bound {
                receiver,
                dispatch: BoundWorkflowToolDispatch::Push,
            },
            reply_completion(),
        )
        .expect("binding");
        let session_id = SessionId::new("session_1");
        let invocation_id = WorkflowToolInvocationId::for_call(
            universe_id,
            &session_id,
            RunId::new(1),
            TurnId::new(1),
            ToolBatchId::new(1),
            &ToolCallId::new("call-1"),
            &binding.binding_fingerprint,
        );
        let mut invocation = WorkflowToolInvocation {
            invocation_id: invocation_id.clone(),
            tool_id: binding.definition.tool_id.clone(),
            semantic_type: binding.definition.semantic_type.clone(),
            schema_revision: binding.definition.revision,
            binding_fingerprint: binding.binding_fingerprint.clone(),
            session_universe_id: universe_id,
            session_id,
            run_id: RunId::new(1),
            turn_id: TurnId::new(1),
            tool_batch_id: ToolBatchId::new(1),
            tool_call_id: ToolCallId::new("call-1"),
            arguments_ref: BlobRef::from_bytes(b"{}"),
            execution_context_ref: None,
            completion_promises: None,
        };

        invocation.execution_context_ref = Some(BlobRef::from_bytes(b"runtime context"));
        let effect = workflow_tool_emit_effect(&invocation);
        assert_eq!(
            invocation_from_emit_effect(&effect).expect("decode emit effect"),
            Some(invocation.clone())
        );

        // Missing map on a promise-bearing binding.
        assert!(validate_completion_promises(&binding, &invocation).is_err());

        // A single-reply map with a counter id passes.
        invocation.completion_promises = Some(BTreeMap::from([(
            REPLY_COMPLETION_KEY.to_owned(),
            PromiseId::from_number(1),
        )]));
        validate_completion_promises(&binding, &invocation).expect("reply map");

        // Wrong key for the singleton reply source.
        invocation.completion_promises = Some(BTreeMap::from([(
            "job-1".to_owned(),
            PromiseId::from_number(1),
        )]));
        assert!(validate_completion_promises(&binding, &invocation).is_err());

        // A keyed set must not reuse one promise id for two keys.
        let keyed = WorkflowToolBinding::admit(
            universe_id,
            definition("submit", "submit_orders"),
            WorkflowToolTarget::Bound {
                receiver: endpoint("service::orders-1"),
                dispatch: BoundWorkflowToolDispatch::Push,
            },
            WorkflowToolCompletion::Promises {
                reply_schema_ref: None,
                deadline_after_ms: None,
                max_promises: 4,
                key_source: WorkflowToolCompletionKeySource::StringArray {
                    pointer: "/orders".to_owned(),
                },
            },
        )
        .expect("keyed binding");
        invocation.completion_promises = Some(BTreeMap::from([
            ("a".to_owned(), PromiseId::from_number(2)),
            ("b".to_owned(), PromiseId::from_number(2)),
        ]));
        assert!(validate_completion_promises(&keyed, &invocation).is_err());
        invocation.completion_promises = Some(BTreeMap::from([
            ("a".to_owned(), PromiseId::from_number(2)),
            ("b".to_owned(), PromiseId::from_number(3)),
        ]));
        validate_completion_promises(&keyed, &invocation).expect("distinct keyed map");

        // Notify bindings must not carry a map.
        let notify = WorkflowToolBinding::admit_bound_notify(
            universe_id,
            definition("approve", "request_approval"),
            endpoint("service::approvals-1"),
        )
        .expect("notify binding");
        invocation.completion_promises = Some(BTreeMap::from([(
            REPLY_COMPLETION_KEY.to_owned(),
            PromiseId::from_number(1),
        )]));
        assert!(validate_completion_promises(&notify, &invocation).is_err());
    }

    fn session_config() -> SessionConfig {
        SessionConfig {
            model: ModelSelection {
                api_kind: ProviderApiKind::OpenAiResponses,
                provider_id: "openai".to_owned(),
                model: "gpt-test".to_owned(),
            },
            generation: Default::default(),
            limits: Default::default(),
            context: ContextConfig { compaction: None },
            features: Default::default(),
        }
    }

    fn commit_action(
        drive: &mut CoreAgentDrive,
        log: &mut Vec<StoredSessionEntry>,
        action: CoreAgentAction,
    ) {
        let CoreAgentAction::AppendEvents {
            expected_head,
            events,
        } = action
        else {
            panic!("expected append action");
        };
        assert_eq!(expected_head, drive.head().cloned());
        let mut head = expected_head;
        let entries = events
            .into_iter()
            .map(|event| {
                let seq = head
                    .as_ref()
                    .map_or(1, |position| position.seq.as_u64() + 1);
                let position = SessionPosition {
                    seq: EventSeq::new(seq),
                };
                head = Some(position.clone());
                StoredSessionEntry {
                    position,
                    observed_at_ms: event.observed_at_ms,
                    joins: event.joins,
                    event: event.event,
                }
            })
            .collect::<Vec<_>>();
        drive
            .resume_appended(entries.clone())
            .expect("resume valid workflow-tool log");
        log.extend(entries);
    }

    fn next_generation(
        drive: &mut CoreAgentDrive,
        log: &mut Vec<StoredSessionEntry>,
    ) -> crate::LlmGenerationRequest {
        for observed_at_ms in 21..80 {
            match drive.next_action(observed_at_ms, 64).expect("next action") {
                CoreAgentAction::GenerateLlm { request } => return request,
                action @ CoreAgentAction::AppendEvents { .. } => {
                    commit_action(drive, log, action);
                }
                other => panic!("unexpected action before generation: {other:?}"),
            }
        }
        panic!("drive did not request generation");
    }

    fn next_tool_batch(
        drive: &mut CoreAgentDrive,
        log: &mut Vec<StoredSessionEntry>,
    ) -> ToolInvocationBatchRequest {
        for observed_at_ms in 81..120 {
            match drive.next_action(observed_at_ms, 64).expect("next action") {
                CoreAgentAction::InvokeTools { request } => return request,
                action @ CoreAgentAction::AppendEvents { .. } => {
                    commit_action(drive, log, action);
                }
                other => panic!("unexpected action before tool batch: {other:?}"),
            }
        }
        panic!("drive did not request tool invocation");
    }

    fn valid_port_log(
        call_ids: &[&str],
    ) -> (
        Vec<StoredSessionEntry>,
        WorkflowEndpointRef,
        SessionId,
        RunId,
        Vec<WorkflowToolInvocation>,
    ) {
        let universe_id = Uuid::from_u128(1);
        let controller = endpoint("controller::work-1");
        let session_id = SessionId::new("managed-session");
        let mut definition = definition("report", "internal.report");
        definition.tool.kind = ToolKind::Builtin(crate::BuiltinToolSpec::default());
        let declaration = ManagedSessionWorkflowTools::v1(
            Some(controller.clone()),
            vec![tool_declaration(definition.clone(), controller.clone())],
        );
        let mut drive =
            CoreAgentDrive::from_replayed(session_id.clone(), CoreAgentState::new(), None);
        let mut log = Vec::new();

        let open = drive
            .admit_command(
                CoreAgentCommand::OpenManagedSession {
                    config: session_config(),
                    session_universe_id: universe_id,
                    workflow_tools: declaration,
                },
                10,
            )
            .expect("open managed session");
        commit_action(&mut drive, &mut log, open);

        let replace_tools = drive
            .admit_command(
                CoreAgentCommand::ReplaceTools {
                    expected_revision: Some(0),
                    tools: BTreeMap::from([(
                        definition.tool.name.clone(),
                        definition.tool.clone(),
                    )]),
                },
                15,
            )
            .expect("install workflow-tool tool");
        commit_action(&mut drive, &mut log, replace_tools);

        let request_run = drive
            .admit_command(
                CoreAgentCommand::RequestRun(RunRequestCommand {
                    requested_by: None,
                    notify_on_terminal: Vec::new(),
                    submission_id: None,
                    source: RunRequestSource::Input {
                        input: vec![ContextEntryInput {
                            kind: ContextEntryKind::Message {
                                role: ContextMessageRole::User,
                            },
                            content: crate::ContentRef {
                                content_ref: BlobRef::from_bytes(b"input"),
                                media_type: None,
                                provider_kind: None,
                            },
                            preview: None,
                            origin: None,
                            provenance_ref: None,
                            token_estimate: None,
                        }],
                    },
                    run_config: RunConfig::default(),
                }),
                20,
            )
            .expect("request run");
        commit_action(&mut drive, &mut log, request_run);

        let generation = next_generation(&mut drive, &mut log);
        let calls = call_ids
            .iter()
            .map(|call_id| ObservedToolCall {
                call_id: ToolCallId::new(*call_id),
                tool_id: Some(definition.tool.name.clone()),
                tool_name: ToolName::new("work_report"),
                provider_kind: None,
                arguments_ref: BlobRef::from_bytes(call_id.as_bytes()),
                native_call_ref: None,
            })
            .collect::<Vec<_>>();
        let generation_completed = drive
            .resume_generation(
                LlmGenerationResult {
                    run_id: generation.run_id,
                    turn_id: generation.turn_id,
                    status: LlmGenerationStatus::Succeeded,
                    failure_ref: None,
                    context_entries: Vec::new(),
                    facts: LlmGenerationFacts {
                        duration_ms: None,
                        provider_response_id: Some("response-tool".to_owned()),
                        finish: LlmFinish::ToolCalls,
                        usage: None,
                        tool_calls: calls,
                        approval_requests: Vec::new(),
                        context_token_estimate: None,
                    },
                },
                80,
            )
            .expect("complete generation");
        commit_action(&mut drive, &mut log, generation_completed);

        let request = next_tool_batch(&mut drive, &mut log);
        let binding = drive
            .state()
            .workflow_tools
            .bindings
            .get(&definition.tool_id)
            .cloned()
            .expect("durable workflow-tool binding");
        assert!(request.calls.iter().all(|call| {
            call.workflow_tool.as_ref().is_some_and(|runtime| {
                runtime.version == crate::WorkflowToolCallRuntime::VERSION
                    && runtime.binding == binding
                    && runtime.prior_emission_count == 0
            })
        }));
        let invocations = request
            .calls
            .iter()
            .map(|call| WorkflowToolInvocation {
                invocation_id: WorkflowToolInvocationId::for_call(
                    universe_id,
                    &session_id,
                    request.run_id,
                    request.turn_id,
                    request.batch_id,
                    &call.call_id,
                    &binding.binding_fingerprint,
                ),
                tool_id: definition.tool_id.clone(),
                semantic_type: definition.semantic_type.clone(),
                schema_revision: definition.revision,
                binding_fingerprint: binding.binding_fingerprint.clone(),
                session_universe_id: universe_id,
                session_id: session_id.clone(),
                run_id: request.run_id,
                turn_id: request.turn_id,
                tool_batch_id: request.batch_id,
                tool_call_id: call.call_id.clone(),
                arguments_ref: call.arguments_ref.clone(),
                execution_context_ref: None,
                completion_promises: None,
            })
            .collect::<Vec<_>>();
        let results = invocations
            .iter()
            .map(|invocation| ToolInvocationResult {
                duration_ms: None,
                output_bytes: None,
                truncated: false,
                call_id: invocation.tool_call_id.clone(),
                status: ToolCallStatus::Succeeded,
                output_ref: Some(BlobRef::from_bytes(b"accepted")),
                model_visible_context_entries: vec![
                    ToolInvocationResult::tool_result_context_entry(
                        &invocation.tool_call_id,
                        ToolCallStatus::Succeeded,
                        BlobRef::from_bytes(b"accepted"),
                    ),
                ],
                error_ref: None,
                effects: vec![workflow_tool_emit_effect(invocation)],
            })
            .collect();
        let tool_completed = drive
            .resume_tool_batch(
                ToolInvocationBatchResult {
                    run_id: request.run_id,
                    turn_id: request.turn_id,
                    batch_id: request.batch_id,
                    results,
                },
                90,
            )
            .expect("complete workflow-tool tool batch");
        commit_action(&mut drive, &mut log, tool_completed);

        (log, controller, session_id, request.run_id, invocations)
    }

    fn rewrite_emission(
        entries: &mut [StoredSessionEntry],
        rewrite: impl FnOnce(&mut WorkflowToolInvocation),
    ) {
        let codec = CoreAgentCodec;
        let entry = entries
            .iter_mut()
            .find(|entry| entry.event.kind == "lightspeed.core.workflow_tool.emitted")
            .expect("workflow-tool emission entry");
        let mut decoded = codec.decode_entry(entry).expect("decode emission");
        let CoreAgentEvent::WorkflowTool(WorkflowToolEvent::Emitted { invocation }) =
            &mut decoded.event
        else {
            panic!("expected emitted workflow-tool event");
        };
        rewrite(invocation);
        *entry = codec.encode_entry(&decoded).expect("re-encode emission");
    }

    #[test]
    fn endpoint_treats_workflow_id_as_an_opaque_bounded_string() {
        endpoint("deployment global / arbitrary 🔧 workflow id")
            .validate()
            .expect("opaque workflow id");
        assert!(endpoint("").validate().is_err());
        assert!(
            endpoint(&"x".repeat(WORKFLOW_ID_MAX_LEN + 1))
                .validate()
                .is_err()
        );
    }

    #[test]
    fn managed_admission_is_order_independent_and_binds_each_receiver() {
        let universe_id = Uuid::from_u128(1);
        let controller = endpoint("controller::work-1");
        let service = endpoint("plugin::approval-1");
        let left = ManagedSessionWorkflowTools::v1(
            Some(controller.clone()),
            vec![
                tool_declaration(definition("status", "work_status"), service.clone()),
                tool_declaration(definition("report", "work_report"), controller.clone()),
            ],
        )
        .admit(universe_id)
        .expect("admit managed-session tools");
        let right = ManagedSessionWorkflowTools::v1(
            Some(controller.clone()),
            vec![
                tool_declaration(definition("report", "work_report"), controller.clone()),
                tool_declaration(definition("status", "work_status"), service.clone()),
            ],
        )
        .admit(universe_id)
        .expect("admit managed-session tools");
        let other_universe = ManagedSessionWorkflowTools::v1(
            Some(controller.clone()),
            vec![
                tool_declaration(definition("report", "work_report"), controller.clone()),
                tool_declaration(definition("status", "work_status"), service.clone()),
            ],
        )
        .admit(Uuid::from_u128(2))
        .expect("admit managed-session tools for another source universe");
        let plugin_only = ManagedSessionWorkflowTools::v1(
            None,
            vec![tool_declaration(
                definition("status", "work_status"),
                service.clone(),
            )],
        )
        .admit(universe_id)
        .expect("admit plugin tool without lifecycle controller");
        let retargeted = ManagedSessionWorkflowTools::v1(
            Some(controller.clone()),
            vec![
                tool_declaration(definition("report", "work_report"), controller.clone()),
                tool_declaration(
                    definition("status", "work_status"),
                    endpoint("plugin::approval-2"),
                ),
            ],
        )
        .admit(universe_id)
        .expect("admit retargeted plugin tool");

        assert_eq!(left.creation_fingerprint, right.creation_fingerprint);
        assert_eq!(left.bindings, right.bindings);
        assert_ne!(
            left.creation_fingerprint,
            other_universe.creation_fingerprint
        );
        assert_ne!(
            left.bindings[0].binding_fingerprint,
            other_universe.bindings[0].binding_fingerprint
        );
        assert_ne!(left.creation_fingerprint, retargeted.creation_fingerprint);
        assert_eq!(
            left.bindings[0].bound_receiver().cloned().unwrap(),
            controller
        );
        assert_eq!(left.bindings[1].bound_receiver().cloned().unwrap(), service);
        assert_eq!(plugin_only.lifecycle_controller, None);
        assert_eq!(
            plugin_only.bindings[0].bound_receiver().cloned().unwrap(),
            service
        );
        assert!(
            left.bindings
                .iter()
                .all(|binding| binding.session_universe_id == universe_id)
        );
        // Golden values pin the explicit v3 field encoding. Serde field order
        // and JSON formatting must never participate in these identities.
        assert_eq!(
            left.bindings[0].binding_fingerprint,
            "wtb:sha256:de1b1562ce30f9016f2a8aed258acb0fbb22a80bcd55124809ad7f8e9c0171db"
        );
        assert_eq!(
            left.creation_fingerprint,
            "msc:sha256:f925b85a1c60556a15dd8fdb513673bc1aaf4efc001ad9572c00b577175e9950"
        );
    }

    #[test]
    fn declaration_rejects_duplicate_tool_names_and_reserved_semantic_type() {
        let universe_id = Uuid::from_u128(1);
        let controller = endpoint("controller::work-1");
        let duplicate = ManagedSessionWorkflowTools::v1(
            Some(controller.clone()),
            vec![
                tool_declaration(definition("report", "work_report"), controller.clone()),
                tool_declaration(definition("status", "work_report"), controller.clone()),
            ],
        );
        assert!(duplicate.admit(universe_id).is_err());

        let mut reserved = definition("report", "work_report");
        reserved.semantic_type = RESERVED_RUN_TERMINAL_SEMANTIC_TYPE.to_owned();
        assert!(
            ManagedSessionWorkflowTools::v1(
                Some(controller.clone()),
                vec![tool_declaration(reserved, controller)],
            )
            .admit(universe_id)
            .is_err()
        );
    }

    #[test]
    fn invocation_id_is_stable_and_universe_scoped() {
        let universe_id = Uuid::from_u128(1);
        let args = (
            SessionId::new("session-1"),
            RunId::new(2),
            TurnId::new(3),
            ToolBatchId::new(4),
            ToolCallId::new("call-5"),
        );
        let id = WorkflowToolInvocationId::for_call(
            universe_id,
            &args.0,
            args.1,
            args.2,
            args.3,
            &args.4,
            "wtb:sha256:test",
        );
        let retry = WorkflowToolInvocationId::for_call(
            universe_id,
            &args.0,
            args.1,
            args.2,
            args.3,
            &args.4,
            "wtb:sha256:test",
        );
        let other_universe = WorkflowToolInvocationId::for_call(
            Uuid::from_u128(2),
            &args.0,
            args.1,
            args.2,
            args.3,
            &args.4,
            "wtb:sha256:test",
        );
        assert_eq!(id, retry);
        assert_ne!(id, other_universe);
    }

    #[test]
    fn pull_read_is_receiver_authorized_run_scoped_and_log_ordered() {
        let (entries, controller, session_id, requested_run, invocations) =
            valid_port_log(&["z-first", "a-second"]);

        let emissions = read_tool_emissions(&entries, &controller, &session_id, requested_run)
            .expect("authorized pull read");

        assert_eq!(emissions, invocations);
        assert!(
            read_tool_emissions(
                &entries,
                &controller,
                &session_id,
                RunId::new(requested_run.as_u64() + 1),
            )
            .expect("other run read")
            .is_empty()
        );

        let error = read_tool_emissions(
            &entries,
            &endpoint("controller::other-work"),
            &session_id,
            requested_run,
        )
        .expect_err("unbound receiver must be rejected");
        assert!(matches!(
            error,
            ReadToolEmissionsError::ReceiverNotBound { .. }
        ));
    }

    #[test]
    fn pull_read_rejects_invocation_whose_durable_binding_metadata_was_changed() {
        let (mut entries, controller, session_id, run_id, _) = valid_port_log(&["forged"]);
        rewrite_emission(&mut entries, |invocation| {
            invocation.semantic_type = "lightspeed.work.other.v1".to_owned();
        });

        let error = read_tool_emissions(&entries, &controller, &session_id, run_id)
            .expect_err("changed binding metadata must fail");
        assert!(matches!(
            error,
            ReadToolEmissionsError::InvalidSessionLog { .. }
        ));
    }

    #[test]
    fn pull_read_replays_tool_success_and_arguments_invariants() {
        let (mut entries, controller, session_id, run_id, _) = valid_port_log(&["forged"]);
        rewrite_emission(&mut entries, |invocation| {
            invocation.arguments_ref = BlobRef::from_bytes(b"different arguments");
        });

        let error = read_tool_emissions(&entries, &controller, &session_id, run_id)
            .expect_err("arguments mismatch must fail full replay");
        assert!(matches!(
            error,
            ReadToolEmissionsError::InvalidSessionLog { .. }
        ));
    }

    #[test]
    fn managed_open_admits_lifecycle_and_bindings_in_one_batch() {
        let universe_id = Uuid::from_u128(1);
        let controller = endpoint("controller::work-1");
        let receiver = endpoint("plugin::reporter-1");
        let declaration = ManagedSessionWorkflowTools::v1(
            Some(controller),
            vec![tool_declaration(
                definition("report", "work_report"),
                receiver.clone(),
            )],
        );
        let expected_fingerprint = declaration
            .creation_fingerprint(universe_id)
            .expect("creation fingerprint");
        let proposals = crate::admit_command(
            &CoreAgentState::new(),
            CoreAgentCommand::OpenManagedSession {
                config: session_config(),
                session_universe_id: universe_id,
                workflow_tools: declaration,
            },
            10,
        )
        .expect("admit managed open");
        assert_eq!(proposals.len(), 2);
        assert!(matches!(
            proposals[0].event,
            CoreAgentEvent::Lifecycle(CoreAgentLifecycleEvent::Opened { .. })
        ));
        assert!(matches!(
            proposals[1].event,
            CoreAgentEvent::WorkflowToolConfig(
                WorkflowToolConfigEvent::ManagedBindingsAdmitted { .. }
            )
        ));
        let codec = CoreAgentCodec;
        let encoded = codec
            .encode_event(&proposals[1].event)
            .expect("encode managed binding event");
        assert_eq!(
            encoded.kind,
            "lightspeed.core.workflow_tool_config.managed_bindings_admitted"
        );
        assert_eq!(
            codec
                .decode_event(&encoded)
                .expect("decode managed binding event"),
            proposals[1].event
        );

        let mut state = CoreAgentState::new();
        for (index, proposal) in proposals.into_iter().enumerate() {
            crate::apply_event(
                &mut state,
                &CoreAgentEntry {
                    position: SessionPosition {
                        seq: EventSeq::new(index as u64 + 1),
                    },
                    observed_at_ms: 10,
                    joins: proposal.joins,
                    event: proposal.event,
                },
            )
            .expect("apply managed opening event");
        }
        assert_eq!(state.workflow_tools.session_universe_id, Some(universe_id));
        assert_eq!(
            state.workflow_tools.managed_creation_fingerprint.as_deref(),
            Some(expected_fingerprint.as_str())
        );
        assert_eq!(state.workflow_tools.bindings.len(), 1);
        assert_eq!(
            state
                .workflow_tools
                .bindings
                .values()
                .next()
                .expect("durable binding")
                .bound_receiver()
                .cloned()
                .expect("bound receiver"),
            receiver
        );
    }

    #[test]
    fn system_workflow_tool_admission_is_add_only_and_does_not_manage_session() {
        let universe_id = Uuid::from_u128(9);
        let declaration = WorkflowToolDeclaration::new(
            definition("core-job-submit", "job_submit"),
            WorkflowToolTarget::Start { start: start_ref() },
            reply_completion(),
        );
        let tool_id = declaration.definition.tool_id.clone();
        let mut drive = CoreAgentDrive::from_replayed(
            SessionId::new("ordinary-session"),
            CoreAgentState::new(),
            None,
        );
        let mut log = Vec::new();
        let open = drive
            .admit_command(
                CoreAgentCommand::OpenSession {
                    config: session_config(),
                },
                1,
            )
            .expect("open ordinary session");
        commit_action(&mut drive, &mut log, open);

        let admit = drive
            .admit_command(
                CoreAgentCommand::AdmitSystemWorkflowTool {
                    session_universe_id: universe_id,
                    declaration: declaration.clone(),
                },
                2,
            )
            .expect("admit system workflow tool");
        commit_action(&mut drive, &mut log, admit);

        assert!(drive.state().workflow_tools.session_universe_id.is_none());
        assert!(
            drive
                .state()
                .workflow_tools
                .managed_declaration_version
                .is_none()
        );
        assert!(drive.state().workflow_tools.lifecycle_controller.is_none());
        assert!(drive.state().workflow_tools.bindings.contains_key(&tool_id));
        assert!(
            drive
                .state()
                .workflow_tools
                .system_binding_ids
                .contains(&tool_id)
        );
        assert!(matches!(
            drive
                .admit_command(
                    CoreAgentCommand::AdmitSystemWorkflowTool {
                        session_universe_id: universe_id,
                        declaration,
                    },
                    3,
                )
                .expect("identical admission is idempotent"),
            CoreAgentAction::Idle
        ));
    }
}
