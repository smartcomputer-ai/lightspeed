use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    DomainError, RunId, SessionId, ToolBatchId, ToolCallId, ToolKind, ToolSpec, TurnId,
    WorkflowToolInvocationId, WorkflowToolPortId,
};

const CONTROLLER_PORT_DECLARATION_VERSION: u32 = 1;
const MAX_CONTROLLER_PORTS: usize = 32;
const WORKFLOW_ID_MAX_LEN: usize = 512;
const WORKFLOW_KIND_MAX_LEN: usize = 128;
const SEMANTIC_TYPE_MAX_LEN: usize = 192;
const BINDING_FINGERPRINT_DOMAIN: &str = "lightspeed.workflow-port.binding.v1";
const CREATION_FINGERPRINT_DOMAIN: &str = "lightspeed.managed-session.creation.v1";
const INVOCATION_ID_DOMAIN: &str = "lightspeed.workflow-port.invocation.v1";
const RESERVED_RUN_TERMINAL_SEMANTIC_TYPE: &str = "lightspeed.run.terminal.v1";

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

/// Trusted declaration supplied only while a lifecycle controller creates
/// its managed session. Every declared port is bound to that controller.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControllerWorkflowPorts {
    pub version: u32,
    pub controller: WorkflowEndpointRef,
    pub ports: Vec<WorkflowToolPortDefinition>,
}

impl ControllerWorkflowPorts {
    pub fn v1(controller: WorkflowEndpointRef, ports: Vec<WorkflowToolPortDefinition>) -> Self {
        Self {
            version: CONTROLLER_PORT_DECLARATION_VERSION,
            controller,
            ports,
        }
    }

    pub fn admit(
        &self,
        session_universe_id: Uuid,
    ) -> Result<AdmittedControllerWorkflowPorts, DomainError> {
        if self.version != CONTROLLER_PORT_DECLARATION_VERSION {
            return Err(DomainError::InvariantViolation(format!(
                "unsupported controller workflow port declaration version {}",
                self.version
            )));
        }
        self.controller.validate()?;
        if self.ports.len() > MAX_CONTROLLER_PORTS {
            return Err(DomainError::InvariantViolation(format!(
                "controller workflow port declaration contains {} ports, max {}",
                self.ports.len(),
                MAX_CONTROLLER_PORTS
            )));
        }

        let mut definitions = self.ports.clone();
        definitions.sort_by(|left, right| left.port_id.cmp(&right.port_id));
        let mut port_ids = BTreeSet::new();
        let mut tool_names = BTreeSet::new();
        let mut bindings = Vec::with_capacity(definitions.len());
        for definition in definitions {
            if !port_ids.insert(definition.port_id.clone()) {
                return Err(DomainError::InvariantViolation(format!(
                    "controller workflow port declaration contains duplicate port id {}",
                    definition.port_id
                )));
            }
            if !tool_names.insert(definition.tool.name.clone()) {
                return Err(DomainError::InvariantViolation(format!(
                    "controller workflow port declaration contains duplicate tool name {}",
                    definition.tool.name
                )));
            }
            bindings.push(WorkflowToolPortBinding::admit(
                session_universe_id,
                definition,
                self.controller.clone(),
            )?);
        }
        let creation_fingerprint = creation_fingerprint(
            session_universe_id,
            self.version,
            &self.controller,
            &bindings,
        )?;
        Ok(AdmittedControllerWorkflowPorts {
            session_universe_id,
            version: self.version,
            controller: self.controller.clone(),
            creation_fingerprint,
            bindings,
        })
    }

    pub fn creation_fingerprint(&self, session_universe_id: Uuid) -> Result<String, DomainError> {
        Ok(self.admit(session_universe_id)?.creation_fingerprint)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmittedControllerWorkflowPorts {
    pub session_universe_id: Uuid,
    pub version: u32,
    pub controller: WorkflowEndpointRef,
    pub creation_fingerprint: String,
    pub bindings: Vec<WorkflowToolPortBinding>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowPortConfigEvent {
    ControllerBindingsAdmitted {
        session_universe_id: Uuid,
        declaration_version: u32,
        controller: WorkflowEndpointRef,
        creation_fingerprint: String,
        bindings: Vec<WorkflowToolPortBinding>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowPortState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_universe_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller: Option<WorkflowEndpointRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_declaration_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_creation_fingerprint: Option<String>,
    #[serde(default)]
    pub controller_bindings: BTreeMap<WorkflowToolPortId, WorkflowToolPortBinding>,
}

impl WorkflowPortState {
    pub fn matches_controller_declaration(
        &self,
        session_universe_id: Uuid,
        declaration: &ControllerWorkflowPorts,
    ) -> Result<bool, DomainError> {
        let expected = declaration.creation_fingerprint(session_universe_id)?;
        Ok(self.session_universe_id == Some(session_universe_id)
            && self.managed_creation_fingerprint.as_deref() == Some(expected.as_str()))
    }
}

pub(crate) fn apply_config_event(
    state: &mut crate::CoreAgentState,
    event: &WorkflowPortConfigEvent,
) -> Result<(), DomainError> {
    match event {
        WorkflowPortConfigEvent::ControllerBindingsAdmitted {
            session_universe_id,
            declaration_version,
            controller,
            creation_fingerprint: observed_creation_fingerprint,
            bindings,
        } => {
            if state.lifecycle.status != crate::CoreAgentStatus::Open {
                return Err(DomainError::InvariantViolation(
                    "controller workflow bindings can only be admitted to an open session"
                        .to_owned(),
                ));
            }
            if state.workflow_ports.session_universe_id.is_some()
                || state.workflow_ports.controller.is_some()
                || state.workflow_ports.managed_creation_fingerprint.is_some()
                || !state.workflow_ports.controller_bindings.is_empty()
            {
                return Err(DomainError::InvariantViolation(
                    "controller workflow bindings are immutable after session creation".to_owned(),
                ));
            }
            if *declaration_version != CONTROLLER_PORT_DECLARATION_VERSION {
                return Err(DomainError::InvariantViolation(format!(
                    "unsupported controller workflow port declaration version {declaration_version}"
                )));
            }
            controller.validate()?;

            let mut previous_port_id: Option<&WorkflowToolPortId> = None;
            let mut tool_names = BTreeSet::new();
            let mut binding_map = BTreeMap::new();
            for binding in bindings {
                binding.validate()?;
                if binding.session_universe_id != *session_universe_id {
                    return Err(DomainError::InvariantViolation(format!(
                        "controller workflow port {} source universe does not match the managed session",
                        binding.definition.port_id
                    )));
                }
                if &binding.receiver != controller {
                    return Err(DomainError::InvariantViolation(format!(
                        "controller workflow port {} receiver does not match the lifecycle controller",
                        binding.definition.port_id
                    )));
                }
                if previous_port_id.is_some_and(|previous| previous >= &binding.definition.port_id)
                {
                    return Err(DomainError::InvariantViolation(
                        "controller workflow port bindings must be unique and sorted by port id"
                            .to_owned(),
                    ));
                }
                previous_port_id = Some(&binding.definition.port_id);
                if !tool_names.insert(binding.definition.tool.name.clone()) {
                    return Err(DomainError::InvariantViolation(format!(
                        "controller workflow port bindings contain duplicate tool name {}",
                        binding.definition.tool.name
                    )));
                }
                binding_map.insert(binding.definition.port_id.clone(), binding.clone());
            }
            if bindings.len() > MAX_CONTROLLER_PORTS {
                return Err(DomainError::InvariantViolation(format!(
                    "controller workflow binding event contains {} ports, max {}",
                    bindings.len(),
                    MAX_CONTROLLER_PORTS
                )));
            }
            let expected_creation_fingerprint = creation_fingerprint(
                *session_universe_id,
                *declaration_version,
                controller,
                bindings,
            )?;
            if observed_creation_fingerprint != &expected_creation_fingerprint {
                return Err(DomainError::InvariantViolation(
                    "managed-session creation fingerprint does not match its durable controller bindings"
                        .to_owned(),
                ));
            }

            state.workflow_ports.session_universe_id = Some(*session_universe_id);
            state.workflow_ports.controller = Some(controller.clone());
            state.workflow_ports.controller_declaration_version = Some(*declaration_version);
            state.workflow_ports.managed_creation_fingerprint =
                Some(observed_creation_fingerprint.clone());
            state.workflow_ports.controller_bindings = binding_map;
            Ok(())
        }
    }
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
    let encoded =
        serde_json::to_vec(&(session_universe_id, definition, receiver)).map_err(|error| {
            DomainError::InvariantViolation(format!(
                "failed to encode workflow port binding fingerprint input: {error}"
            ))
        })?;
    Ok(format!(
        "wpb:sha256:{}",
        hex::encode(digest_fields(BINDING_FINGERPRINT_DOMAIN, &[&encoded]))
    ))
}

fn creation_fingerprint(
    session_universe_id: Uuid,
    version: u32,
    controller: &WorkflowEndpointRef,
    bindings: &[WorkflowToolPortBinding],
) -> Result<String, DomainError> {
    let encoded = serde_json::to_vec(&(session_universe_id, version, controller, bindings))
        .map_err(|error| {
            DomainError::InvariantViolation(format!(
                "failed to encode managed-session creation fingerprint input: {error}"
            ))
        })?;
    Ok(format!(
        "msc:sha256:{}",
        hex::encode(digest_fields(CREATION_FINGERPRINT_DOMAIN, &[&encoded]))
    ))
}

fn digest_fields(domain: &str, fields: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update((domain.len() as u64).to_be_bytes());
    hasher.update(domain.as_bytes());
    for field in fields {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BlobRef, ContextConfig, CoreAgentCodec, CoreAgentCommand, CoreAgentEntry, CoreAgentEvent,
        CoreAgentLifecycleEvent, CoreAgentState, EventSeq, FunctionToolSpec, ModelSelection,
        ProviderApiKind, SessionConfig, SessionPosition, ToolName, ToolParallelism,
        ToolTargetRequirement,
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
    fn controller_admission_is_order_independent_and_binds_every_port() {
        let universe_id = Uuid::from_u128(1);
        let controller = endpoint("controller::work-1");
        let left = ControllerWorkflowPorts::v1(
            controller.clone(),
            vec![
                definition("status", "work_status"),
                definition("report", "work_report"),
            ],
        )
        .admit(universe_id)
        .expect("admit controller ports");
        let right = ControllerWorkflowPorts::v1(
            controller.clone(),
            vec![
                definition("report", "work_report"),
                definition("status", "work_status"),
            ],
        )
        .admit(universe_id)
        .expect("admit controller ports");
        let other_universe = ControllerWorkflowPorts::v1(
            controller.clone(),
            vec![
                definition("report", "work_report"),
                definition("status", "work_status"),
            ],
        )
        .admit(Uuid::from_u128(2))
        .expect("admit controller ports for another source universe");

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
        assert!(
            left.bindings
                .iter()
                .all(|binding| binding.receiver == controller
                    && binding.session_universe_id == universe_id)
        );
    }

    #[test]
    fn declaration_rejects_duplicate_tool_names_and_reserved_semantic_type() {
        let universe_id = Uuid::from_u128(1);
        let controller = endpoint("controller::work-1");
        let duplicate = ControllerWorkflowPorts::v1(
            controller.clone(),
            vec![
                definition("report", "work_report"),
                definition("status", "work_report"),
            ],
        );
        assert!(duplicate.admit(universe_id).is_err());

        let mut reserved = definition("report", "work_report");
        reserved.semantic_type = RESERVED_RUN_TERMINAL_SEMANTIC_TYPE.to_owned();
        assert!(
            ControllerWorkflowPorts::v1(controller, vec![reserved])
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
    fn managed_open_admits_lifecycle_and_bindings_in_one_batch() {
        let universe_id = Uuid::from_u128(1);
        let declaration = ControllerWorkflowPorts::v1(
            endpoint("controller::work-1"),
            vec![definition("report", "work_report")],
        );
        let expected_fingerprint = declaration
            .creation_fingerprint(universe_id)
            .expect("creation fingerprint");
        let proposals = crate::admit_command(
            &CoreAgentState::new(),
            CoreAgentCommand::OpenManagedSession {
                config: session_config(),
                session_universe_id: universe_id,
                controller_ports: declaration,
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
                WorkflowPortConfigEvent::ControllerBindingsAdmitted { .. }
            )
        ));
        let codec = CoreAgentCodec;
        let encoded = codec
            .encode_event(&proposals[1].event)
            .expect("encode controller binding event");
        assert_eq!(
            encoded.kind,
            "lightspeed.core.workflow_port_config.controller_bindings_admitted"
        );
        assert_eq!(
            codec
                .decode_event(&encoded)
                .expect("decode controller binding event"),
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
        assert_eq!(state.workflow_ports.controller_bindings.len(), 1);
    }
}
