use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    BlobRef, CodecError, CoreAgentCodec, CoreAgentEntry, CoreAgentEvent, DomainError, PromiseId,
    RunId, SessionId, ToolBatchId, ToolCallId, ToolEffect, ToolKind, ToolName, ToolSpec, TurnId,
    WorkflowToolInvocationId, WorkflowToolPortId, storage::StoredSessionEntry,
};

const MANAGED_PORT_DECLARATION_VERSION: u32 = 1;
const MAX_MANAGED_PORTS: usize = 32;
pub const MAX_WORKFLOW_PORT_EMISSIONS_PER_RUN: u32 = 32;
pub const MAX_WORKFLOW_PORT_EMISSIONS_PER_READ: usize =
    MAX_MANAGED_PORTS * MAX_WORKFLOW_PORT_EMISSIONS_PER_RUN as usize;
const WORKFLOW_ID_MAX_LEN: usize = 512;
const WORKFLOW_KIND_MAX_LEN: usize = 128;
const SEMANTIC_TYPE_MAX_LEN: usize = 192;
const BINDING_FINGERPRINT_DOMAIN: &str = "lightspeed.workflow-port.binding.v1";
const CREATION_FINGERPRINT_DOMAIN: &str = "lightspeed.managed-session.creation.v1";
const FINGERPRINT_ENCODING_VERSION: u32 = 1;
const INVOCATION_ID_DOMAIN: &str = "lightspeed.workflow-port.invocation.v1";
const RESERVED_RUN_TERMINAL_SEMANTIC_TYPE: &str = "lightspeed.run.terminal.v1";
pub const WORKFLOW_PORT_EMIT_EFFECT_KIND: &str = "lightspeed.core.workflow_port.emit";

const EFFECT_INVOCATION_ID: &str = "invocation_id";
const EFFECT_PORT_ID: &str = "port_id";
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
const EFFECT_REPLY_PROMISE_ID: &str = "reply_promise_id";

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowToolPortDefinition {
    pub port_id: WorkflowToolPortId,
    pub revision: u32,
    pub semantic_type: String,
    /// Complete provider-facing function tool definition. Workflow-port
    /// routing remains separate and never appears in model arguments.
    pub tool: ToolSpec,
}

impl WorkflowToolPortDefinition {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.revision == 0 {
            return Err(DomainError::InvariantViolation(format!(
                "workflow port {} revision must be greater than zero",
                self.port_id
            )));
        }
        validate_semantic_type(&self.semantic_type)?;
        self.tool.validate()?;
        if !matches!(self.tool.kind, ToolKind::Function(_)) {
            return Err(DomainError::InvariantViolation(format!(
                "workflow port {} must use a function tool",
                self.port_id
            )));
        }
        if self.tool.target_requirement.namespace().is_some() {
            return Err(DomainError::InvariantViolation(format!(
                "workflow port {} must not declare an execution target",
                self.port_id
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowToolPortBinding {
    /// Universe of the managed session that owns this binding. This is the
    /// emission source scope, not a claim about the receiver's scope.
    pub session_universe_id: Uuid,
    pub definition: WorkflowToolPortDefinition,
    pub receiver: WorkflowEndpointRef,
    pub binding_fingerprint: String,
}

/// Bounded durable record of one successful workflow-port tool call.
///
/// The model arguments remain in CAS and are referenced by `arguments_ref`.
/// Receiver-specific interpretation belongs to the receiving workflow.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowToolInvocation {
    pub invocation_id: WorkflowToolInvocationId,
    pub port_id: WorkflowToolPortId,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_promise_id: Option<PromiseId>,
}

#[derive(Debug, Error)]
pub enum ReadPortEmissionsError {
    #[error("invalid workflow-port receiver endpoint: {message}")]
    InvalidReceiver { message: String },

    #[error("decode workflow-port session entry: {0}")]
    Decode(#[from] CodecError),

    #[error("reduce workflow-port session log: {message}")]
    InvalidSessionLog { message: String },

    #[error("invalid durable workflow-port binding {binding_fingerprint}: {message}")]
    InvalidBinding {
        binding_fingerprint: String,
        message: String,
    },

    #[error("workflow-port receiver is not bound to this session: {workflow_id}")]
    ReceiverNotBound { workflow_id: String },

    #[error(
        "workflow-port invocation {invocation_id} references unknown durable binding {binding_fingerprint}"
    )]
    UnknownBinding {
        invocation_id: WorkflowToolInvocationId,
        binding_fingerprint: String,
    },

    #[error(
        "workflow-port invocation {invocation_id} does not match its durable binding: {message}"
    )]
    InvocationBindingMismatch {
        invocation_id: WorkflowToolInvocationId,
        message: String,
    },

    #[error("workflow-port invocation {invocation_id} does not match its event joins")]
    InvocationJoinMismatch {
        invocation_id: WorkflowToolInvocationId,
    },

    #[error("duplicate workflow-port invocation in session log: {invocation_id}")]
    DuplicateInvocation {
        invocation_id: WorkflowToolInvocationId,
    },

    #[error("workflow-port emission read exceeds the bounded result limit of {limit} invocations")]
    ResultLimitExceeded { limit: usize },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowPortEvent {
    Emitted {
        invocation: WorkflowToolInvocation,
    },
    DeliveryFailed {
        invocation_id: WorkflowToolInvocationId,
        error_ref: BlobRef,
    },
}

impl WorkflowToolPortBinding {
    pub fn admit(
        session_universe_id: Uuid,
        definition: WorkflowToolPortDefinition,
        receiver: WorkflowEndpointRef,
    ) -> Result<Self, DomainError> {
        definition.validate()?;
        receiver.validate()?;
        let binding_fingerprint = binding_fingerprint(session_universe_id, &definition, &receiver)?;
        Ok(Self {
            session_universe_id,
            definition,
            receiver,
            binding_fingerprint,
        })
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.definition.validate()?;
        self.receiver.validate()?;
        let expected =
            binding_fingerprint(self.session_universe_id, &self.definition, &self.receiver)?;
        if self.binding_fingerprint != expected {
            return Err(DomainError::InvariantViolation(format!(
                "workflow port {} binding fingerprint does not match its durable definition and receiver",
                self.definition.port_id
            )));
        }
        Ok(())
    }
}

/// One trusted workflow-port declaration supplied when a managed session is
/// created. The receiver is opaque to the session core and need not be the
/// lifecycle controller.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowToolPortDeclaration {
    pub definition: WorkflowToolPortDefinition,
    pub receiver: WorkflowEndpointRef,
}

impl WorkflowToolPortDeclaration {
    pub fn new(definition: WorkflowToolPortDefinition, receiver: WorkflowEndpointRef) -> Self {
        Self {
            definition,
            receiver,
        }
    }
}

/// Trusted creation document supplied by a workflow plugin or other
/// authorized control-plane caller. The lifecycle controller owns the outer
/// session loop; each port independently names its receiver.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedSessionWorkflowPorts {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_controller: Option<WorkflowEndpointRef>,
    pub ports: Vec<WorkflowToolPortDeclaration>,
}

impl ManagedSessionWorkflowPorts {
    pub fn v1(
        lifecycle_controller: Option<WorkflowEndpointRef>,
        ports: Vec<WorkflowToolPortDeclaration>,
    ) -> Self {
        Self {
            version: MANAGED_PORT_DECLARATION_VERSION,
            lifecycle_controller,
            ports,
        }
    }

    pub fn admit(
        &self,
        session_universe_id: Uuid,
    ) -> Result<AdmittedManagedSessionWorkflowPorts, DomainError> {
        if self.version != MANAGED_PORT_DECLARATION_VERSION {
            return Err(DomainError::InvariantViolation(format!(
                "unsupported managed-session workflow port declaration version {}",
                self.version
            )));
        }
        if let Some(controller) = &self.lifecycle_controller {
            controller.validate()?;
        }
        if self.ports.len() > MAX_MANAGED_PORTS {
            return Err(DomainError::InvariantViolation(format!(
                "managed-session workflow port declaration contains {} ports, max {}",
                self.ports.len(),
                MAX_MANAGED_PORTS
            )));
        }

        let mut declarations = self.ports.clone();
        declarations.sort_by(|left, right| left.definition.port_id.cmp(&right.definition.port_id));
        let mut port_ids = BTreeSet::new();
        let mut tool_names = BTreeSet::new();
        let mut bindings = Vec::with_capacity(declarations.len());
        for declaration in declarations {
            let definition = declaration.definition;
            if !port_ids.insert(definition.port_id.clone()) {
                return Err(DomainError::InvariantViolation(format!(
                    "managed-session workflow port declaration contains duplicate port id {}",
                    definition.port_id
                )));
            }
            if !tool_names.insert(definition.tool.name.clone()) {
                return Err(DomainError::InvariantViolation(format!(
                    "managed-session workflow port declaration contains duplicate tool name {}",
                    definition.tool.name
                )));
            }
            bindings.push(WorkflowToolPortBinding::admit(
                session_universe_id,
                definition,
                declaration.receiver,
            )?);
        }
        let creation_fingerprint = creation_fingerprint(
            session_universe_id,
            self.version,
            self.lifecycle_controller.as_ref(),
            &bindings,
        )?;
        Ok(AdmittedManagedSessionWorkflowPorts {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmittedManagedSessionWorkflowPorts {
    pub session_universe_id: Uuid,
    pub version: u32,
    pub lifecycle_controller: Option<WorkflowEndpointRef>,
    pub creation_fingerprint: String,
    pub bindings: Vec<WorkflowToolPortBinding>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowPortConfigEvent {
    ManagedBindingsAdmitted {
        session_universe_id: Uuid,
        declaration_version: u32,
        lifecycle_controller: Option<WorkflowEndpointRef>,
        creation_fingerprint: String,
        bindings: Vec<WorkflowToolPortBinding>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowPortState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_universe_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_controller: Option<WorkflowEndpointRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_declaration_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_creation_fingerprint: Option<String>,
    #[serde(default)]
    pub bindings: BTreeMap<WorkflowToolPortId, WorkflowToolPortBinding>,
    #[serde(default)]
    pub emissions: BTreeMap<WorkflowToolInvocationId, WorkflowToolInvocation>,
    #[serde(default)]
    pub delivery_failures: BTreeMap<WorkflowToolInvocationId, BlobRef>,
}

impl WorkflowPortState {
    pub fn matches_managed_declaration(
        &self,
        session_universe_id: Uuid,
        declaration: &ManagedSessionWorkflowPorts,
    ) -> Result<bool, DomainError> {
        let expected = declaration.creation_fingerprint(session_universe_id)?;
        Ok(self.session_universe_id == Some(session_universe_id)
            && self.managed_creation_fingerprint.as_deref() == Some(expected.as_str()))
    }

    pub fn binding_for_tool_name(&self, tool_name: &ToolName) -> Option<&WorkflowToolPortBinding> {
        self.bindings
            .values()
            .find(|binding| &binding.definition.tool.name == tool_name)
    }

    pub fn emission_count(&self, run_id: RunId, port_id: &WorkflowToolPortId) -> u32 {
        self.emissions
            .values()
            .filter(|invocation| invocation.run_id == run_id && &invocation.port_id == port_id)
            .count()
            .try_into()
            .unwrap_or(u32::MAX)
    }
}

/// Project the workflow-port invocations for one receiver and run from the
/// durable session log.
///
/// Results retain session-log order. Bindings are learned only from durable
/// configuration facts encountered before an invocation, so registry changes
/// cannot retarget historical emissions. Invocations inherited by a session
/// fork are ignored because their embedded session id names the source
/// session.
pub fn read_port_emissions(
    entries: &[StoredSessionEntry],
    receiver_endpoint: &WorkflowEndpointRef,
    session_id: &SessionId,
    run_id: RunId,
) -> Result<Vec<WorkflowToolInvocation>, ReadPortEmissionsError> {
    receiver_endpoint
        .validate()
        .map_err(|error| ReadPortEmissionsError::InvalidReceiver {
            message: error.to_string(),
        })?;

    let mut projection = WorkflowPortEmissionReadProjection {
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
            ReadPortEmissionsError::InvalidSessionLog {
                message: error.to_string(),
            }
        })?;
        projection.observe(&decoded)?;
    }
    projection.finish()
}

struct WorkflowPortEmissionReadProjection<'a> {
    receiver_endpoint: &'a WorkflowEndpointRef,
    session_id: &'a SessionId,
    run_id: RunId,
    bindings: BTreeMap<String, WorkflowToolPortBinding>,
    receiver_bound: bool,
    seen_invocations: BTreeSet<WorkflowToolInvocationId>,
    emissions: Vec<WorkflowToolInvocation>,
}

impl WorkflowPortEmissionReadProjection<'_> {
    fn observe(&mut self, entry: &CoreAgentEntry) -> Result<(), ReadPortEmissionsError> {
        match &entry.event {
            CoreAgentEvent::WorkflowPortConfig(event) => self.observe_config(event)?,
            CoreAgentEvent::WorkflowPort(WorkflowPortEvent::Emitted { invocation })
                if invocation.session_id == *self.session_id =>
            {
                let binding = self
                    .bindings
                    .get(&invocation.binding_fingerprint)
                    .ok_or_else(|| ReadPortEmissionsError::UnknownBinding {
                        invocation_id: invocation.invocation_id.clone(),
                        binding_fingerprint: invocation.binding_fingerprint.clone(),
                    })?;
                validate_invocation_against_binding(binding, invocation).map_err(|error| {
                    ReadPortEmissionsError::InvocationBindingMismatch {
                        invocation_id: invocation.invocation_id.clone(),
                        message: error.to_string(),
                    }
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
                    return Err(ReadPortEmissionsError::InvocationBindingMismatch {
                        invocation_id: invocation.invocation_id.clone(),
                        message: "invocation id is not canonical".to_owned(),
                    });
                }
                if entry.joins.run_id != Some(invocation.run_id)
                    || entry.joins.turn_id != Some(invocation.turn_id)
                    || entry.joins.tool_batch_id != Some(invocation.tool_batch_id)
                    || entry.joins.tool_call_id.as_ref() != Some(&invocation.tool_call_id)
                {
                    return Err(ReadPortEmissionsError::InvocationJoinMismatch {
                        invocation_id: invocation.invocation_id.clone(),
                    });
                }
                if !self
                    .seen_invocations
                    .insert(invocation.invocation_id.clone())
                {
                    return Err(ReadPortEmissionsError::DuplicateInvocation {
                        invocation_id: invocation.invocation_id.clone(),
                    });
                }
                if invocation.run_id == self.run_id && binding.receiver == *self.receiver_endpoint {
                    if self.emissions.len() >= MAX_WORKFLOW_PORT_EMISSIONS_PER_READ {
                        return Err(ReadPortEmissionsError::ResultLimitExceeded {
                            limit: MAX_WORKFLOW_PORT_EMISSIONS_PER_READ,
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
        event: &WorkflowPortConfigEvent,
    ) -> Result<(), ReadPortEmissionsError> {
        match event {
            WorkflowPortConfigEvent::ManagedBindingsAdmitted {
                session_universe_id,
                declaration_version,
                lifecycle_controller,
                creation_fingerprint: observed_creation_fingerprint,
                bindings,
            } => {
                if *declaration_version != MANAGED_PORT_DECLARATION_VERSION {
                    return Err(ReadPortEmissionsError::InvalidBinding {
                        binding_fingerprint: observed_creation_fingerprint.clone(),
                        message: format!(
                            "unsupported managed-session declaration version {declaration_version}"
                        ),
                    });
                }
                if let Some(controller) = lifecycle_controller {
                    controller.validate().map_err(|error| {
                        ReadPortEmissionsError::InvalidBinding {
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
                .map_err(|error| ReadPortEmissionsError::InvalidBinding {
                    binding_fingerprint: observed_creation_fingerprint.clone(),
                    message: error.to_string(),
                })?;
                if observed_creation_fingerprint != &expected_creation_fingerprint {
                    return Err(ReadPortEmissionsError::InvalidBinding {
                        binding_fingerprint: observed_creation_fingerprint.clone(),
                        message: "managed-session creation fingerprint does not match".to_owned(),
                    });
                }

                for binding in bindings {
                    binding
                        .validate()
                        .map_err(|error| ReadPortEmissionsError::InvalidBinding {
                            binding_fingerprint: binding.binding_fingerprint.clone(),
                            message: error.to_string(),
                        })?;
                    if binding.session_universe_id != *session_universe_id {
                        return Err(ReadPortEmissionsError::InvalidBinding {
                            binding_fingerprint: binding.binding_fingerprint.clone(),
                            message: "binding source universe differs from its managed-session declaration"
                                .to_owned(),
                        });
                    }
                    if binding.receiver == *self.receiver_endpoint {
                        self.receiver_bound = true;
                    }
                    match self
                        .bindings
                        .insert(binding.binding_fingerprint.clone(), binding.clone())
                    {
                        Some(existing) if existing != *binding => {
                            return Err(ReadPortEmissionsError::InvalidBinding {
                                binding_fingerprint: binding.binding_fingerprint.clone(),
                                message: "fingerprint identifies more than one durable binding"
                                    .to_owned(),
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<Vec<WorkflowToolInvocation>, ReadPortEmissionsError> {
        if !self.receiver_bound {
            return Err(ReadPortEmissionsError::ReceiverNotBound {
                workflow_id: self.receiver_endpoint.workflow_id.clone(),
            });
        }
        Ok(self.emissions)
    }
}

pub(crate) fn apply_config_event(
    state: &mut crate::CoreAgentState,
    event: &WorkflowPortConfigEvent,
) -> Result<(), DomainError> {
    match event {
        WorkflowPortConfigEvent::ManagedBindingsAdmitted {
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
            if state.workflow_ports.session_universe_id.is_some()
                || state.workflow_ports.lifecycle_controller.is_some()
                || state.workflow_ports.managed_creation_fingerprint.is_some()
                || !state.workflow_ports.bindings.is_empty()
            {
                return Err(DomainError::InvariantViolation(
                    "managed-session workflow bindings are immutable after session creation"
                        .to_owned(),
                ));
            }
            if *declaration_version != MANAGED_PORT_DECLARATION_VERSION {
                return Err(DomainError::InvariantViolation(format!(
                    "unsupported managed-session workflow port declaration version {declaration_version}"
                )));
            }
            if let Some(controller) = lifecycle_controller {
                controller.validate()?;
            }

            let mut previous_port_id: Option<&WorkflowToolPortId> = None;
            let mut tool_names = BTreeSet::new();
            let mut binding_map = BTreeMap::new();
            for binding in bindings {
                binding.validate()?;
                if binding.session_universe_id != *session_universe_id {
                    return Err(DomainError::InvariantViolation(format!(
                        "workflow port {} source universe does not match the managed session",
                        binding.definition.port_id
                    )));
                }
                if previous_port_id.is_some_and(|previous| previous >= &binding.definition.port_id)
                {
                    return Err(DomainError::InvariantViolation(
                        "managed-session workflow port bindings must be unique and sorted by port id"
                            .to_owned(),
                    ));
                }
                previous_port_id = Some(&binding.definition.port_id);
                if !tool_names.insert(binding.definition.tool.name.clone()) {
                    return Err(DomainError::InvariantViolation(format!(
                        "managed-session workflow port bindings contain duplicate tool name {}",
                        binding.definition.tool.name
                    )));
                }
                binding_map.insert(binding.definition.port_id.clone(), binding.clone());
            }
            if bindings.len() > MAX_MANAGED_PORTS {
                return Err(DomainError::InvariantViolation(format!(
                    "managed-session workflow binding event contains {} ports, max {}",
                    bindings.len(),
                    MAX_MANAGED_PORTS
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

            state.workflow_ports.session_universe_id = Some(*session_universe_id);
            state.workflow_ports.lifecycle_controller = lifecycle_controller.clone();
            state.workflow_ports.managed_declaration_version = Some(*declaration_version);
            state.workflow_ports.managed_creation_fingerprint =
                Some(observed_creation_fingerprint.clone());
            state.workflow_ports.bindings = binding_map;
            Ok(())
        }
    }
}

pub(crate) fn apply_event(
    state: &mut crate::CoreAgentState,
    event: &WorkflowPortEvent,
) -> Result<(), DomainError> {
    match event {
        WorkflowPortEvent::Emitted { invocation } => {
            validate_invocation_against_state(state, invocation)?;
            if state
                .workflow_ports
                .emissions
                .contains_key(&invocation.invocation_id)
            {
                return Err(DomainError::InvariantViolation(format!(
                    "workflow port invocation {} was already emitted",
                    invocation.invocation_id
                )));
            }
            if state
                .workflow_ports
                .emission_count(invocation.run_id, &invocation.port_id)
                >= MAX_WORKFLOW_PORT_EMISSIONS_PER_RUN
            {
                return Err(DomainError::InvariantViolation(format!(
                    "workflow port {} exceeded its per-run emission cap",
                    invocation.port_id
                )));
            }
            state
                .workflow_ports
                .emissions
                .insert(invocation.invocation_id.clone(), invocation.clone());
            Ok(())
        }
        WorkflowPortEvent::DeliveryFailed {
            invocation_id,
            error_ref,
        } => {
            if !state.workflow_ports.emissions.contains_key(invocation_id) {
                return Err(DomainError::InvariantViolation(format!(
                    "workflow port delivery failure references unknown invocation {invocation_id}"
                )));
            }
            match state.workflow_ports.delivery_failures.get(invocation_id) {
                Some(existing) if existing == error_ref => Ok(()),
                Some(_) => Err(DomainError::InvariantViolation(format!(
                    "workflow port invocation {invocation_id} already has a different delivery failure"
                ))),
                None => {
                    state
                        .workflow_ports
                        .delivery_failures
                        .insert(invocation_id.clone(), error_ref.clone());
                    Ok(())
                }
            }
        }
    }
}

pub fn workflow_port_emit_effect(invocation: &WorkflowToolInvocation) -> ToolEffect {
    let mut data = BTreeMap::new();
    data.insert(
        EFFECT_INVOCATION_ID.to_owned(),
        invocation.invocation_id.as_str().to_owned(),
    );
    data.insert(
        EFFECT_PORT_ID.to_owned(),
        invocation.port_id.as_str().to_owned(),
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
    if let Some(reply_promise_id) = &invocation.reply_promise_id {
        data.insert(
            EFFECT_REPLY_PROMISE_ID.to_owned(),
            reply_promise_id.as_str().to_owned(),
        );
    }
    ToolEffect {
        kind: WORKFLOW_PORT_EMIT_EFFECT_KIND.to_owned(),
        data,
    }
}

pub(crate) fn invocation_from_emit_effect(
    effect: &ToolEffect,
) -> Result<Option<WorkflowToolInvocation>, DomainError> {
    if effect.kind != WORKFLOW_PORT_EMIT_EFFECT_KIND {
        return Ok(None);
    }
    let field = |key: &str| {
        effect.data.get(key).cloned().ok_or_else(|| {
            DomainError::InvariantViolation(format!("workflow port emit effect is missing `{key}`"))
        })
    };
    let parse_u64 = |key: &str, value: String| {
        value.parse::<u64>().map_err(|_| {
            DomainError::InvariantViolation(format!(
                "workflow port emit effect `{key}` is not a u64"
            ))
        })
    };
    let invocation_id =
        WorkflowToolInvocationId::try_new(field(EFFECT_INVOCATION_ID)?).map_err(|error| {
            DomainError::InvariantViolation(format!(
                "workflow port emit effect has invalid invocation id: {error}"
            ))
        })?;
    let port_id = WorkflowToolPortId::try_new(field(EFFECT_PORT_ID)?).map_err(|error| {
        DomainError::InvariantViolation(format!(
            "workflow port emit effect has invalid port id: {error}"
        ))
    })?;
    let session_universe_id =
        Uuid::parse_str(&field(EFFECT_SESSION_UNIVERSE_ID)?).map_err(|error| {
            DomainError::InvariantViolation(format!(
                "workflow port emit effect has invalid source universe: {error}"
            ))
        })?;
    let session_id = SessionId::try_new(field(EFFECT_SESSION_ID)?).map_err(|error| {
        DomainError::InvariantViolation(format!(
            "workflow port emit effect has invalid session id: {error}"
        ))
    })?;
    let tool_call_id = ToolCallId::try_new(field(EFFECT_TOOL_CALL_ID)?).map_err(|error| {
        DomainError::InvariantViolation(format!(
            "workflow port emit effect has invalid tool call id: {error}"
        ))
    })?;
    let arguments_ref = BlobRef::parse(field(EFFECT_ARGUMENTS_REF)?).map_err(|error| {
        DomainError::InvariantViolation(format!(
            "workflow port emit effect has invalid arguments ref: {error}"
        ))
    })?;
    let reply_promise_id = effect
        .data
        .get(EFFECT_REPLY_PROMISE_ID)
        .map(|value| PromiseId::new(value.clone()));

    Ok(Some(WorkflowToolInvocation {
        invocation_id,
        port_id,
        semantic_type: field(EFFECT_SEMANTIC_TYPE)?,
        schema_revision: parse_u64(EFFECT_SCHEMA_REVISION, field(EFFECT_SCHEMA_REVISION)?)?
            .try_into()
            .map_err(|_| {
                DomainError::InvariantViolation(
                    "workflow port emit effect schema revision exceeds u32".to_owned(),
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
        reply_promise_id,
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
            "workflow port emit effect does not match its session/run/turn/batch/call joins"
                .to_owned(),
        ));
    }
    validate_invocation_binding(state, invocation)?;
    let active_run = state.runs.active.as_ref().ok_or_else(|| {
        DomainError::InvariantViolation(
            "workflow port emit effect requires an active run".to_owned(),
        )
    })?;
    let batch = active_run
        .tool_batches
        .get(&expected_batch_id)
        .ok_or_else(|| {
            DomainError::InvariantViolation(format!(
                "workflow port emit effect references missing tool batch {expected_batch_id}"
            ))
        })?;
    let call = batch
        .calls
        .iter()
        .find(|call| &call.call.call_id == expected_call_id)
        .ok_or_else(|| {
            DomainError::InvariantViolation(format!(
                "workflow port emit effect references missing tool call {expected_call_id}"
            ))
        })?;
    let binding = state
        .workflow_ports
        .bindings
        .get(&invocation.port_id)
        .expect("binding was validated above");
    if call.call.tool_name != binding.definition.tool.name
        || call.call.arguments_ref != invocation.arguments_ref
    {
        return Err(DomainError::InvariantViolation(
            "workflow port emit effect does not match its admitted tool name and arguments"
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
            "workflow port invocation id does not match its durable call identity".to_owned(),
        ));
    }
    let existing = state
        .workflow_ports
        .emission_count(invocation.run_id, &invocation.port_id);
    if existing.saturating_add(pending_emissions_for_port) >= MAX_WORKFLOW_PORT_EMISSIONS_PER_RUN {
        return Err(DomainError::InvariantViolation(format!(
            "workflow port {} exceeded its per-run emission cap",
            invocation.port_id
        )));
    }
    Ok(())
}

fn validate_invocation_binding(
    state: &crate::CoreAgentState,
    invocation: &WorkflowToolInvocation,
) -> Result<(), DomainError> {
    let binding = state
        .workflow_ports
        .bindings
        .get(&invocation.port_id)
        .ok_or_else(|| {
            DomainError::InvariantViolation(format!(
                "workflow port invocation references unknown port {}",
                invocation.port_id
            ))
        })?;
    validate_invocation_against_binding(binding, invocation)
}

fn validate_invocation_against_binding(
    binding: &WorkflowToolPortBinding,
    invocation: &WorkflowToolInvocation,
) -> Result<(), DomainError> {
    if invocation.session_universe_id != binding.session_universe_id
        || invocation.semantic_type != binding.definition.semantic_type
        || invocation.schema_revision != binding.definition.revision
        || invocation.binding_fingerprint != binding.binding_fingerprint
        || invocation.port_id != binding.definition.port_id
    {
        return Err(DomainError::InvariantViolation(format!(
            "workflow port invocation {} does not match its durable binding",
            invocation.invocation_id
        )));
    }
    if invocation.reply_promise_id.is_some() {
        return Err(DomainError::InvariantViolation(
            "notify-only workflow port invocation must not include a reply promise".to_owned(),
        ));
    }
    Ok(())
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
            "workflow port emitted event has a non-canonical invocation id".to_owned(),
        ));
    }
    let active_run = state.runs.active.as_ref().ok_or_else(|| {
        DomainError::InvariantViolation(
            "workflow port invocation can only be emitted for an active run".to_owned(),
        )
    })?;
    if active_run.run_id != invocation.run_id {
        return Err(DomainError::InvariantViolation(
            "workflow port invocation does not match the active run".to_owned(),
        ));
    }
    let batch = active_run
        .tool_batches
        .get(&invocation.tool_batch_id)
        .ok_or_else(|| {
            DomainError::InvariantViolation(format!(
                "workflow port invocation references missing tool batch {}",
                invocation.tool_batch_id
            ))
        })?;
    if batch.turn_id != invocation.turn_id {
        return Err(DomainError::InvariantViolation(
            "workflow port invocation does not match its tool batch turn".to_owned(),
        ));
    }
    let call = batch
        .calls
        .iter()
        .find(|call| call.call.call_id == invocation.tool_call_id)
        .ok_or_else(|| {
            DomainError::InvariantViolation(format!(
                "workflow port invocation references missing tool call {}",
                invocation.tool_call_id
            ))
        })?;
    let binding = state
        .workflow_ports
        .bindings
        .get(&invocation.port_id)
        .expect("binding was validated above");
    if call.call.tool_name != binding.definition.tool.name
        || call.call.arguments_ref != invocation.arguments_ref
        || call.status != crate::ToolCallStatus::Succeeded
    {
        return Err(DomainError::InvariantViolation(
            "workflow port invocation does not match a successful durable tool call".to_owned(),
        ));
    }
    Ok(())
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
        Self::new(format!("wpi:sha256:{}", hex::encode(digest)))
    }
}

fn validate_semantic_type(value: &str) -> Result<(), DomainError> {
    validate_component(
        "workflow port semantic type",
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
            "workflow port semantic type must be a dotted identifier ending in '.v<digits>'"
                .to_owned(),
        ));
    }
    if value == RESERVED_RUN_TERMINAL_SEMANTIC_TYPE {
        return Err(DomainError::InvariantViolation(format!(
            "workflow port semantic type {value} is reserved by the emission substrate"
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
    definition: &WorkflowToolPortDefinition,
    receiver: &WorkflowEndpointRef,
) -> Result<String, DomainError> {
    let mut hasher = canonical_fingerprint_hasher(BINDING_FINGERPRINT_DOMAIN);
    update_digest_part(&mut hasher, session_universe_id.as_bytes());
    update_definition_fingerprint(&mut hasher, definition)?;
    update_endpoint_fingerprint(&mut hasher, receiver);
    Ok(format!("wpb:sha256:{}", hex::encode(hasher.finalize())))
}

fn creation_fingerprint(
    session_universe_id: Uuid,
    version: u32,
    controller: Option<&WorkflowEndpointRef>,
    bindings: &[WorkflowToolPortBinding],
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
    definition: &WorkflowToolPortDefinition,
) -> Result<(), DomainError> {
    update_digest_part(hasher, definition.port_id.as_str().as_bytes());
    update_digest_part(hasher, &definition.revision.to_be_bytes());
    update_digest_part(hasher, definition.semantic_type.as_bytes());
    update_digest_part(hasher, definition.tool.name.as_str().as_bytes());

    let ToolKind::Function(function) = &definition.tool.kind else {
        return Err(DomainError::InvariantViolation(format!(
            "workflow port {} fingerprint requires a function tool",
            definition.port_id
        )));
    };
    update_digest_part(hasher, b"function");
    update_optional_text(hasher, function.model_name.as_ref().map(ToolName::as_str));
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
    update_digest_part(
        hasher,
        match definition.tool.parallelism {
            crate::ToolParallelism::Exclusive => b"exclusive",
            crate::ToolParallelism::ParallelSafe => b"parallel_safe",
        },
    );
    if definition.tool.target_requirement != crate::ToolTargetRequirement::None {
        return Err(DomainError::InvariantViolation(format!(
            "workflow port {} fingerprint does not support an execution target requirement",
            definition.port_id
        )));
    }
    update_digest_part(hasher, b"target_none");
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
        ToolTargetRequirement, storage::StoredSessionEntry,
    };

    fn endpoint(workflow_id: &str) -> WorkflowEndpointRef {
        WorkflowEndpointRef {
            workflow_id: workflow_id.to_owned(),
            workflow_kind: "agent_work".to_owned(),
        }
    }

    fn definition(port_id: &str, tool_name: &str) -> WorkflowToolPortDefinition {
        WorkflowToolPortDefinition {
            port_id: WorkflowToolPortId::new(port_id),
            revision: 1,
            semantic_type: "lightspeed.work.report.v1".to_owned(),
            tool: ToolSpec {
                name: ToolName::new(tool_name),
                kind: ToolKind::Function(FunctionToolSpec {
                    model_name: None,
                    description_ref: None,
                    input_schema_ref: BlobRef::from_bytes(b"input schema"),
                    output_schema_ref: None,
                    strict: Some(true),
                    provider_options_ref: None,
                }),
                parallelism: ToolParallelism::ParallelSafe,
                target_requirement: ToolTargetRequirement::None,
            },
        }
    }

    fn port_declaration(
        definition: WorkflowToolPortDefinition,
        receiver: WorkflowEndpointRef,
    ) -> WorkflowToolPortDeclaration {
        WorkflowToolPortDeclaration::new(definition, receiver)
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
            .expect("resume valid workflow-port log");
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
        let definition = definition("report", "work_report");
        let declaration = ManagedSessionWorkflowPorts::v1(
            Some(controller.clone()),
            vec![port_declaration(definition.clone(), controller.clone())],
        );
        let mut drive =
            CoreAgentDrive::from_replayed(session_id.clone(), CoreAgentState::new(), None);
        let mut log = Vec::new();

        let open = drive
            .admit_command(
                CoreAgentCommand::OpenManagedSession {
                    config: session_config(),
                    session_universe_id: universe_id,
                    workflow_ports: declaration,
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
            .expect("install workflow-port tool");
        commit_action(&mut drive, &mut log, replace_tools);

        let request_run = drive
            .admit_command(
                CoreAgentCommand::RequestRun(RunRequestCommand {
                    notify_on_terminal: Vec::new(),
                    submission_id: None,
                    source: RunRequestSource::Input {
                        input: vec![ContextEntryInput {
                            kind: ContextEntryKind::Message {
                                role: ContextMessageRole::User,
                            },
                            content_ref: BlobRef::from_bytes(b"input"),
                            media_type: None,
                            preview: None,
                            provider_kind: None,
                            provider_item_id: None,
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
                tool_name: definition.tool.name.clone(),
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
                        provider_response_id: Some("response-port".to_owned()),
                        finish: LlmFinish::ToolCalls,
                        usage: None,
                        tool_calls: calls,
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
            .workflow_ports
            .bindings
            .get(&definition.port_id)
            .cloned()
            .expect("durable workflow-port binding");
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
                port_id: definition.port_id.clone(),
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
                reply_promise_id: None,
            })
            .collect::<Vec<_>>();
        let results = invocations
            .iter()
            .map(|invocation| ToolInvocationResult {
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
                effects: vec![workflow_port_emit_effect(invocation)],
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
            .expect("complete workflow-port tool batch");
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
            .find(|entry| entry.event.kind == "lightspeed.core.workflow_port.emitted")
            .expect("workflow-port emission entry");
        let mut decoded = codec.decode_entry(entry).expect("decode emission");
        let CoreAgentEvent::WorkflowPort(WorkflowPortEvent::Emitted { invocation }) =
            &mut decoded.event
        else {
            panic!("expected emitted workflow-port event");
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
        let left = ManagedSessionWorkflowPorts::v1(
            Some(controller.clone()),
            vec![
                port_declaration(definition("status", "work_status"), service.clone()),
                port_declaration(definition("report", "work_report"), controller.clone()),
            ],
        )
        .admit(universe_id)
        .expect("admit managed-session ports");
        let right = ManagedSessionWorkflowPorts::v1(
            Some(controller.clone()),
            vec![
                port_declaration(definition("report", "work_report"), controller.clone()),
                port_declaration(definition("status", "work_status"), service.clone()),
            ],
        )
        .admit(universe_id)
        .expect("admit managed-session ports");
        let other_universe = ManagedSessionWorkflowPorts::v1(
            Some(controller.clone()),
            vec![
                port_declaration(definition("report", "work_report"), controller.clone()),
                port_declaration(definition("status", "work_status"), service.clone()),
            ],
        )
        .admit(Uuid::from_u128(2))
        .expect("admit managed-session ports for another source universe");
        let plugin_only = ManagedSessionWorkflowPorts::v1(
            None,
            vec![port_declaration(
                definition("status", "work_status"),
                service.clone(),
            )],
        )
        .admit(universe_id)
        .expect("admit plugin port without lifecycle controller");
        let retargeted = ManagedSessionWorkflowPorts::v1(
            Some(controller.clone()),
            vec![
                port_declaration(definition("report", "work_report"), controller.clone()),
                port_declaration(
                    definition("status", "work_status"),
                    endpoint("plugin::approval-2"),
                ),
            ],
        )
        .admit(universe_id)
        .expect("admit retargeted plugin port");

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
        assert_eq!(left.bindings[0].receiver, controller);
        assert_eq!(left.bindings[1].receiver, service);
        assert_eq!(plugin_only.lifecycle_controller, None);
        assert_eq!(plugin_only.bindings[0].receiver, service);
        assert!(
            left.bindings
                .iter()
                .all(|binding| binding.session_universe_id == universe_id)
        );
        // Golden values pin the explicit v1 field encoding. Serde field order
        // and JSON formatting must never participate in these identities.
        assert_eq!(
            left.bindings[0].binding_fingerprint,
            "wpb:sha256:ee15bc93d0286ef9a79be389f9b80e73928447e47554388baf20da832c214ff4"
        );
        assert_eq!(
            left.creation_fingerprint,
            "msc:sha256:1ef80ce536738850b0a4968e0ad88bd1d5e643bc6268104cda04567428cd18a2"
        );
    }

    #[test]
    fn declaration_rejects_duplicate_tool_names_and_reserved_semantic_type() {
        let universe_id = Uuid::from_u128(1);
        let controller = endpoint("controller::work-1");
        let duplicate = ManagedSessionWorkflowPorts::v1(
            Some(controller.clone()),
            vec![
                port_declaration(definition("report", "work_report"), controller.clone()),
                port_declaration(definition("status", "work_report"), controller.clone()),
            ],
        );
        assert!(duplicate.admit(universe_id).is_err());

        let mut reserved = definition("report", "work_report");
        reserved.semantic_type = RESERVED_RUN_TERMINAL_SEMANTIC_TYPE.to_owned();
        assert!(
            ManagedSessionWorkflowPorts::v1(
                Some(controller.clone()),
                vec![port_declaration(reserved, controller)],
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
            "wpb:sha256:test",
        );
        let retry = WorkflowToolInvocationId::for_call(
            universe_id,
            &args.0,
            args.1,
            args.2,
            args.3,
            &args.4,
            "wpb:sha256:test",
        );
        let other_universe = WorkflowToolInvocationId::for_call(
            Uuid::from_u128(2),
            &args.0,
            args.1,
            args.2,
            args.3,
            &args.4,
            "wpb:sha256:test",
        );
        assert_eq!(id, retry);
        assert_ne!(id, other_universe);
    }

    #[test]
    fn pull_read_is_receiver_authorized_run_scoped_and_log_ordered() {
        let (entries, controller, session_id, requested_run, invocations) =
            valid_port_log(&["z-first", "a-second"]);

        let emissions = read_port_emissions(&entries, &controller, &session_id, requested_run)
            .expect("authorized pull read");

        assert_eq!(emissions, invocations);
        assert!(
            read_port_emissions(
                &entries,
                &controller,
                &session_id,
                RunId::new(requested_run.as_u64() + 1),
            )
            .expect("other run read")
            .is_empty()
        );

        let error = read_port_emissions(
            &entries,
            &endpoint("controller::other-work"),
            &session_id,
            requested_run,
        )
        .expect_err("unbound receiver must be rejected");
        assert!(matches!(
            error,
            ReadPortEmissionsError::ReceiverNotBound { .. }
        ));
    }

    #[test]
    fn pull_read_rejects_invocation_whose_durable_binding_metadata_was_changed() {
        let (mut entries, controller, session_id, run_id, _) = valid_port_log(&["forged"]);
        rewrite_emission(&mut entries, |invocation| {
            invocation.semantic_type = "lightspeed.work.other.v1".to_owned();
        });

        let error = read_port_emissions(&entries, &controller, &session_id, run_id)
            .expect_err("changed binding metadata must fail");
        assert!(matches!(
            error,
            ReadPortEmissionsError::InvalidSessionLog { .. }
        ));
    }

    #[test]
    fn pull_read_replays_tool_success_and_arguments_invariants() {
        let (mut entries, controller, session_id, run_id, _) = valid_port_log(&["forged"]);
        rewrite_emission(&mut entries, |invocation| {
            invocation.arguments_ref = BlobRef::from_bytes(b"different arguments");
        });

        let error = read_port_emissions(&entries, &controller, &session_id, run_id)
            .expect_err("arguments mismatch must fail full replay");
        assert!(matches!(
            error,
            ReadPortEmissionsError::InvalidSessionLog { .. }
        ));
    }

    #[test]
    fn managed_open_admits_lifecycle_and_bindings_in_one_batch() {
        let universe_id = Uuid::from_u128(1);
        let controller = endpoint("controller::work-1");
        let receiver = endpoint("plugin::reporter-1");
        let declaration = ManagedSessionWorkflowPorts::v1(
            Some(controller),
            vec![port_declaration(
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
                workflow_ports: declaration,
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
            CoreAgentEvent::WorkflowPortConfig(
                WorkflowPortConfigEvent::ManagedBindingsAdmitted { .. }
            )
        ));
        let codec = CoreAgentCodec;
        let encoded = codec
            .encode_event(&proposals[1].event)
            .expect("encode managed binding event");
        assert_eq!(
            encoded.kind,
            "lightspeed.core.workflow_port_config.managed_bindings_admitted"
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
        assert_eq!(state.workflow_ports.session_universe_id, Some(universe_id));
        assert_eq!(
            state.workflow_ports.managed_creation_fingerprint.as_deref(),
            Some(expected_fingerprint.as_str())
        );
        assert_eq!(state.workflow_ports.bindings.len(), 1);
        assert_eq!(
            state
                .workflow_ports
                .bindings
                .values()
                .next()
                .expect("durable binding")
                .receiver,
            receiver
        );
    }
}
