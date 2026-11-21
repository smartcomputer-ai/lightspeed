import { isFailedToolCall, type TranscriptToolCall } from "./transcript";

// Older transcript entries may have only the model-facing name. A recorded
// tool ID takes precedence so similarly named external tools stay prominent.
const ENVIRONMENT_TOOLS = {
  "env.read_file": ["read_file", "Read"],
  "env.write_file": ["write_file", "Write"],
  "env.edit_file": ["edit_file", "Edit"],
  "env.apply_patch": ["apply_patch"],
  "env.grep": ["grep", "Grep"],
  "env.glob": ["glob", "Glob"],
  "env.list_dir": ["list_dir", "ListDir"],
  "env.reference": ["env_reference"],
  "env.run_process": ["run_process", "exec_command", "Bash"],
  "env.continue_process": ["continue_process", "write_stdin", "BashOutput", "KillShell"],
  "env.job_submit": ["job_submit"],
  "env.job_run": ["job_run"],
  "env.job_read": ["job_read"],
};
const ENVIRONMENT_TOOL_IDS = new Set(Object.keys(ENVIRONMENT_TOOLS));
const ENVIRONMENT_TOOL_NAMES = new Set(Object.values(ENVIRONMENT_TOOLS).flat());

/// Environment failures are normal working steps. Preserve their failure
/// status and details, but reserve summary badges and red styling for other
/// tool failures. The run's own failure outcome is independent of this rule.
export function isEmphasizedToolFailure(call: TranscriptToolCall): boolean {
  if (!isFailedToolCall(call)) return false;
  return call.toolId
    ? !ENVIRONMENT_TOOL_IDS.has(call.toolId)
    : !ENVIRONMENT_TOOL_NAMES.has(call.toolName);
}
