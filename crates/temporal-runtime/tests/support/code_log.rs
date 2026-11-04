//! Code-mode facts read from the session log. Reduced state prunes a finished
//! invocation and its code scope at batch completion, so a check made once the
//! call has finished reads what happened from the log instead.
#![allow(dead_code)]

use harness::{
    CodeToolCallSpec, CodeToolEvent, CodeToolScopeSpec, CoreAgentCodec, CoreAgentEvent, SessionId,
    WorkflowToolEvent, event_kind,
    storage::{ReadSessionEvents, SessionStore},
};

#[derive(Debug, Default)]
pub struct CodeLog {
    /// The workflow id of each `code_execute` start the session recorded.
    pub execution_ids: Vec<String>,
    pub scopes_opened: Vec<CodeToolScopeSpec>,
    pub calls_admitted: Vec<CodeToolCallSpec>,
}

impl CodeLog {
    pub fn calls_in(&self, execution_id: &str) -> Vec<&CodeToolCallSpec> {
        self.calls_admitted
            .iter()
            .filter(|call| call.origin.execution_id == execution_id)
            .collect()
    }
}

pub async fn read_code_log(
    store: &store_pg::PgStore,
    session_id: &SessionId,
) -> anyhow::Result<CodeLog> {
    let mut log = CodeLog::default();
    let mut after = None;
    loop {
        let page = store
            .read_after(ReadSessionEvents {
                session_id: session_id.clone(),
                after,
                limit: 1000,
            })
            .await?;
        for entry in &page.entries {
            if ![
                event_kind::WORKFLOW_TOOL_START_REQUESTED,
                event_kind::CODE_TOOL_SCOPE_OPENED,
                event_kind::CODE_TOOL_CALL_ADMITTED,
            ]
            .contains(&entry.event.kind.as_str())
            {
                continue;
            }
            match CoreAgentCodec.decode_event(&entry.event)? {
                CoreAgentEvent::WorkflowTool(WorkflowToolEvent::StartRequested {
                    invocation,
                    execution_id,
                }) if invocation.workflow_tool_id.as_str()
                    == tools::code::CODE_EXECUTE_WORKFLOW_TOOL_ID =>
                {
                    log.execution_ids.push(execution_id);
                }
                CoreAgentEvent::CodeTool(CodeToolEvent::ScopeOpened { scope }) => {
                    log.scopes_opened.push(scope);
                }
                CoreAgentEvent::CodeTool(CodeToolEvent::CallAdmitted { call, .. }) => {
                    log.calls_admitted.push(call);
                }
                _ => {}
            }
        }
        if page.complete {
            return Ok(log);
        }
        after = page.next_after;
    }
}
