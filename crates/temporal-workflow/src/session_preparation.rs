//! Durable session preparation requests. These carry intent and references,
//! never credentials or provider transport payloads.
use engine::{
    CoreAgentState, SessionConfig, SessionId, ToolName, ToolSpec, WorkflowToolBinding,
    WorkflowToolDeclaration, WorkflowToolId,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionToolsetSource {
    pub config: SessionConfig,
    pub config_revision: u64,
    pub bindings: BTreeMap<WorkflowToolId, WorkflowToolBinding>,
    pub system_binding_ids: BTreeSet<WorkflowToolId>,
}

impl SessionToolsetSource {
    pub fn from_state(state: &CoreAgentState) -> Option<Self> {
        Some(Self {
            config: state.lifecycle.config.clone()?,
            config_revision: state.lifecycle.config_revision,
            bindings: state.workflow_tools.bindings.clone(),
            system_binding_ids: state.workflow_tools.system_binding_ids.clone(),
        })
    }
    pub fn matches(&self, state: &CoreAgentState) -> bool {
        Self::from_state(state).as_ref() == Some(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionToolsetPreparation {
    pub source: SessionToolsetSource,
    pub declarations: Vec<WorkflowToolDeclaration>,
    pub tools: BTreeMap<ToolName, ToolSpec>,
}

/// The resolved profile is captured at submission so workflow retries do not
/// reread a mutable named profile. Configuration has already been translated
/// into the engine's provider-neutral document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionProfileIntent {
    pub config: Option<SessionConfig>,
    pub instructions: Option<api::ProfileInstructions>,
    pub environment: Option<api::ProfileEnvironment>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionOperationRequest {
    pub operation_id: String,
    pub submitted_at_ms: u64,
    pub operation: SessionOperation,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionOperation {
    Configure {
        config: SessionConfig,
        expected_revision: Option<u64>,
    },
    ApplyProfile {
        profile: SessionProfileIntent,
        expected_config_revision: Option<u64>,
        expected_tools_revision: Option<u64>,
    },
    RefreshContext,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionOperationOutcome {
    pub receipt: SessionOperationReceipt,
    pub result: Result<api::ProfileApplySummary, api::AgentApiError>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionOperationStatus {
    pub outcome: Result<Option<SessionOperationOutcome>, api::AgentApiError>,
}

/// Compact identity retained after an operation completes. The timestamp is
/// part of the identity: callers must preserve it when retrying the same intent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionOperationReceipt {
    pub operation_id: String,
    pub submitted_at_ms: u64,
    pub fingerprint: String,
}

impl SessionOperationRequest {
    pub fn receipt(&self) -> Result<SessionOperationReceipt, serde_json::Error> {
        use sha2::{Digest, Sha256};
        Ok(SessionOperationReceipt {
            operation_id: self.operation_id.clone(),
            submitted_at_ms: self.submitted_at_ms,
            fingerprint: hex::encode(Sha256::digest(serde_json::to_vec(self)?)),
        })
    }
}

/// Retain recent results across workflow rollover without retaining full inputs.
/// Eviction retires every request at or before that submission timestamp, so an
/// evicted retry cannot execute again. Callers must reload state before making
/// a new operation after an expired receipt.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionOperationReceipts {
    retired_through_ms: Option<u64>,
    outcomes: BTreeMap<String, SessionOperationOutcome>,
}

impl SessionOperationReceipts {
    const CAPACITY: usize = 256;

    pub fn lookup(
        &self,
        receipt: &SessionOperationReceipt,
    ) -> Result<Option<SessionOperationOutcome>, api::AgentApiError> {
        if self
            .retired_through_ms
            .is_some_and(|time| receipt.submitted_at_ms <= time)
        {
            return Err(api::AgentApiError::conflict(
                "session preparation receipt expired; reload session state before submitting a new operation",
            ));
        }
        match self.outcomes.get(&receipt.operation_id) {
            Some(outcome) if outcome.receipt != *receipt => Err(api::AgentApiError::conflict(
                "operation ID was reused with different intent",
            )),
            outcome => Ok(outcome.cloned()),
        }
    }

    pub(crate) fn insert(&mut self, outcome: SessionOperationOutcome) {
        self.outcomes
            .insert(outcome.receipt.operation_id.clone(), outcome);
        if self.outcomes.len() > Self::CAPACITY {
            let oldest = self
                .outcomes
                .values()
                .map(|outcome| outcome.receipt.submitted_at_ms)
                .min()
                .unwrap();
            self.retired_through_ms = Some(oldest);
            self.outcomes
                .retain(|_, outcome| outcome.receipt.submitted_at_ms > oldest);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionProfilePreparationRequest {
    pub session_id: SessionId,
    pub instructions: Option<api::ProfileInstructions>,
    pub environment: Option<api::ProfileEnvironment>,
    pub source: SessionToolsetSource,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionProfilePreparation {
    pub toolset: SessionToolsetPreparation,
    pub instructions: BTreeMap<engine::ContextEntryKey, engine::ContextEntryInput>,
    pub environment_id: Option<engine::EnvironmentId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionToolsetRequest {
    pub source: SessionToolsetSource,
    pub validate_configuration: bool,
}

/// Compute a minimal patch from the currently published tools to an observed desired set.
pub fn session_toolset_patch(
    active: &BTreeMap<engine::ToolName, engine::ToolSpec>,
    desired: &BTreeMap<engine::ToolName, engine::ToolSpec>,
) -> engine::ToolPatch {
    engine::ToolPatch {
        remove: active
            .keys()
            .filter(|name| !desired.contains_key(*name))
            .cloned()
            .collect(),
        upsert: desired
            .iter()
            .filter(|(name, tool)| active.get(*name) != Some(*tool))
            .map(|(_, tool)| tool.clone())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::BlobRef;

    fn operation(id: usize, submitted_at_ms: u64) -> SessionOperationRequest {
        SessionOperationRequest {
            operation_id: format!("operation_{id}"),
            submitted_at_ms,
            operation: SessionOperation::RefreshContext,
        }
    }

    fn completed(request: &SessionOperationRequest) -> SessionOperationOutcome {
        SessionOperationOutcome {
            receipt: request.receipt().unwrap(),
            result: Ok(api::ProfileApplySummary::default()),
        }
    }

    #[test]
    fn receipts_preserve_success_and_failure_and_reject_changed_intent() {
        let mut receipts = SessionOperationReceipts::default();
        let request = operation(1, 1);
        let outcome = completed(&request);
        assert_eq!(receipts.lookup(&outcome.receipt), Ok(None));
        receipts.insert(outcome.clone());
        assert_eq!(receipts.lookup(&outcome.receipt), Ok(Some(outcome.clone())));

        let mut changed = request.clone();
        changed.operation = SessionOperation::ApplyProfile {
            profile: SessionProfileIntent {
                config: None,
                instructions: None,
                environment: None,
            },
            expected_config_revision: None,
            expected_tools_revision: None,
        };
        assert_eq!(
            receipts
                .lookup(&changed.receipt().unwrap())
                .unwrap_err()
                .kind,
            api::AgentApiErrorKind::Conflict
        );
        assert_eq!(receipts.lookup(&outcome.receipt), Ok(Some(outcome)));

        let mut failed = completed(&operation(2, 2));
        failed.result = Err(api::AgentApiError::rejected("configuration unavailable"));
        receipts.insert(failed.clone());
        assert_eq!(receipts.lookup(&failed.receipt), Ok(Some(failed)));
    }

    #[test]
    fn receipt_eviction_remains_bounded_and_rejects_old_retries_after_rollover() {
        let mut receipts = SessionOperationReceipts::default();
        for id in 0..1000 {
            let outcome = completed(&operation(id, id as u64));
            assert_eq!(receipts.lookup(&outcome.receipt), Ok(None));
            receipts.insert(outcome);
            assert!(receipts.outcomes.len() <= SessionOperationReceipts::CAPACITY);
        }
        let mut continuation = crate::AgentSessionContinuationState::v1(Vec::new());
        continuation.operation_outcomes = receipts;
        let restored: crate::AgentSessionContinuationState =
            serde_json::from_slice(&serde_json::to_vec(&continuation).unwrap()).unwrap();
        let receipts = restored.operation_outcomes;
        assert_eq!(receipts.outcomes.len(), SessionOperationReceipts::CAPACITY);
        let old = operation(0, 0).receipt().unwrap();
        assert_eq!(
            receipts.lookup(&old).unwrap_err().kind,
            api::AgentApiErrorKind::Conflict
        );
        let recent = completed(&operation(999, 999));
        assert_eq!(receipts.lookup(&recent.receipt), Ok(Some(recent)));
        // A delayed request inside the retired time range must also not execute.
        assert_eq!(
            receipts
                .lookup(&operation(2000, 1).receipt().unwrap())
                .unwrap_err()
                .kind,
            api::AgentApiErrorKind::Conflict
        );
    }

    #[test]
    fn eviction_retires_equal_timestamps_together() {
        let mut receipts = SessionOperationReceipts::default();
        for id in 0..=SessionOperationReceipts::CAPACITY {
            receipts.insert(completed(&operation(id, 10)));
        }
        assert!(receipts.outcomes.is_empty());
        assert_eq!(
            receipts
                .lookup(&operation(0, 10).receipt().unwrap())
                .unwrap_err()
                .kind,
            api::AgentApiErrorKind::Conflict
        );
        assert_eq!(
            receipts.lookup(&operation(1000, 11).receipt().unwrap()),
            Ok(None)
        );
    }

    #[test]
    fn receipts_do_not_retain_profile_payloads() {
        let mut request = operation(1, 1);
        request.operation = SessionOperation::ApplyProfile {
            profile: SessionProfileIntent {
                config: None,
                instructions: Some(api::ProfileInstructions::Text {
                    text: "large profile content".repeat(10_000),
                }),
                environment: None,
            },
            expected_config_revision: None,
            expected_tools_revision: None,
        };
        let mut receipts = SessionOperationReceipts::default();
        receipts.insert(completed(&request));
        let encoded = serde_json::to_string(&receipts).unwrap();
        assert!(encoded.len() < 1024);
        assert!(!encoded.contains("large profile content"));
        assert_eq!(
            request.receipt().unwrap(),
            request.clone().receipt().unwrap()
        );
    }

    #[test]
    fn toolset_reconcile_patch_preserves_declared_remote_mcp_tools() {
        let remote_tool_name = ToolName::new("mcp_crm");
        let old_tool_name = ToolName::new("old_tool");
        let new_tool_name = ToolName::new("new_tool");
        let active = BTreeMap::from([
            (
                remote_tool_name.clone(),
                test_remote_mcp_tool(remote_tool_name.clone()),
            ),
            (
                old_tool_name.clone(),
                test_function_tool(old_tool_name.clone()),
            ),
        ]);
        let mut desired = BTreeMap::from([(
            new_tool_name.clone(),
            test_function_tool(new_tool_name.clone()),
        )]);
        let desired_mcp = BTreeMap::from([(
            remote_tool_name.clone(),
            test_remote_mcp_tool(remote_tool_name.clone()),
        )]);

        desired.extend(desired_mcp);
        let patch = session_toolset_patch(&active, &desired);
        let tools = patch.apply_to(&active).expect("apply reconcile patch");

        assert!(tools.contains_key(&remote_tool_name));
        assert!(!tools.contains_key(&old_tool_name));
        assert!(tools.contains_key(&new_tool_name));
        assert_eq!(patch.upsert.len(), 1);
        assert_eq!(patch.remove, vec![old_tool_name]);
        assert!(session_toolset_patch(&tools, &desired).is_empty());
    }

    #[test]
    fn toolset_reconcile_patch_removes_undeclared_remote_mcp_tools() {
        let remote_tool_name = ToolName::new("mcp_crm");
        let active = BTreeMap::from([(
            remote_tool_name.clone(),
            test_remote_mcp_tool(remote_tool_name.clone()),
        )]);

        let patch = session_toolset_patch(&active, &BTreeMap::new());
        let tools = patch.apply_to(&active).expect("apply reconcile patch");

        assert!(!tools.contains_key(&remote_tool_name));
    }

    #[test]
    fn toolset_reconcile_patch_tracks_every_mcp_policy_transition() {
        let remote_tool_name = ToolName::new("mcp_crm");
        let find_tool_name = ToolName::new("mcp_find_tools");
        let call_tool_name = ToolName::new("mcp_call");

        let mut inject_all = test_remote_mcp_tool(remote_tool_name.clone());
        let engine::ToolKind::RemoteMcp(spec) = &mut inject_all.kind else {
            unreachable!("test helper must produce a remote MCP tool");
        };
        spec.execution = engine::RemoteMcpExecution::Native;
        let mut active = BTreeMap::from([(remote_tool_name.clone(), inject_all)]);

        let mut search_selected = test_remote_mcp_tool(remote_tool_name.clone());
        let engine::ToolKind::RemoteMcp(spec) = &mut search_selected.kind else {
            unreachable!("test helper must produce a remote MCP tool");
        };
        spec.record_revision = 2;
        spec.execution = engine::RemoteMcpExecution::Native;
        spec.exposure = engine::RemoteMcpExposure::Search;
        spec.allowed_tools = Some(vec!["lookup_customer".to_owned()]);
        let desired = BTreeMap::from([
            (remote_tool_name.clone(), search_selected),
            (
                find_tool_name.clone(),
                test_function_tool(find_tool_name.clone()),
            ),
            (
                call_tool_name.clone(),
                test_function_tool(call_tool_name.clone()),
            ),
        ]);
        active = session_toolset_patch(&active, &desired)
            .apply_to(&active)
            .expect("switch inject-all to search-selected");
        assert!(active.contains_key(&find_tool_name));
        assert!(active.contains_key(&call_tool_name));
        let engine::ToolKind::RemoteMcp(spec) = &active[&remote_tool_name].kind else {
            panic!("expected remote MCP tool");
        };
        assert_eq!(spec.exposure, engine::RemoteMcpExposure::Search);
        assert_eq!(spec.allowed_tools, Some(vec!["lookup_customer".to_owned()]));

        let mut search_other_selection = active[&remote_tool_name].clone();
        let engine::ToolKind::RemoteMcp(spec) = &mut search_other_selection.kind else {
            unreachable!("expected remote MCP tool");
        };
        spec.record_revision = 3;
        spec.allowed_tools = Some(vec!["create_customer".to_owned()]);
        let desired = BTreeMap::from([
            (remote_tool_name.clone(), search_other_selection),
            (find_tool_name.clone(), active[&find_tool_name].clone()),
            (call_tool_name.clone(), active[&call_tool_name].clone()),
        ]);
        active = session_toolset_patch(&active, &desired)
            .apply_to(&active)
            .expect("change selected search tools");
        let engine::ToolKind::RemoteMcp(spec) = &active[&remote_tool_name].kind else {
            panic!("expected remote MCP tool");
        };
        assert_eq!(spec.allowed_tools, Some(vec!["create_customer".to_owned()]));

        let mut inject_selected = active[&remote_tool_name].clone();
        let engine::ToolKind::RemoteMcp(spec) = &mut inject_selected.kind else {
            unreachable!("expected remote MCP tool");
        };
        spec.record_revision = 4;
        spec.exposure = engine::RemoteMcpExposure::Inject;
        let desired = BTreeMap::from([(remote_tool_name.clone(), inject_selected)]);
        active = session_toolset_patch(&active, &desired)
            .apply_to(&active)
            .expect("switch search to inject-selected");
        assert!(!active.contains_key(&find_tool_name));
        assert!(!active.contains_key(&call_tool_name));

        let mut inject_all = active[&remote_tool_name].clone();
        let engine::ToolKind::RemoteMcp(spec) = &mut inject_all.kind else {
            unreachable!("expected remote MCP tool");
        };
        spec.record_revision = 5;
        spec.allowed_tools = None;
        let desired = BTreeMap::from([(remote_tool_name.clone(), inject_all)]);
        active = session_toolset_patch(&active, &desired)
            .apply_to(&active)
            .expect("switch inject-selected to inject-all");
        let engine::ToolKind::RemoteMcp(spec) = &active[&remote_tool_name].kind else {
            panic!("expected remote MCP tool");
        };
        assert_eq!(spec.exposure, engine::RemoteMcpExposure::Inject);
        assert_eq!(spec.allowed_tools, None);
    }

    fn test_remote_mcp_tool(tool_name: ToolName) -> engine::ToolSpec {
        engine::ToolSpec {
            name: tool_name,
            execution: Default::default(),
            kind: engine::ToolKind::RemoteMcp(engine::RemoteMcpToolSpec {
                server_id: "crm".to_owned(),
                record_revision: 1,
                server_label: "crm".to_owned(),
                server_url: "https://crm.example.com/mcp".to_owned(),
                description_ref: None,
                allowed_tools: None,
                execution: engine::RemoteMcpExecution::Provider,
                exposure: engine::RemoteMcpExposure::Inject,
                approval: engine::RemoteMcpApprovalPolicy::Never,
                defer_loading: None,
                auth_ref: None,
                auth_required: false,
                allow_private_network: false,
            }),
            parallelism: engine::ToolParallelism::ParallelSafe,
        }
    }

    fn test_function_tool(tool_name: ToolName) -> engine::ToolSpec {
        engine::ToolSpec {
            name: tool_name,
            execution: Default::default(),
            kind: engine::ToolKind::Function(engine::FunctionToolSpec {
                description_ref: None,
                input_schema_ref: BlobRef::from_bytes(b"schema"),
                output_schema_ref: None,
                strict: Some(true),
                provider_options_ref: None,
            }),
            parallelism: engine::ToolParallelism::Exclusive,
        }
    }
}
