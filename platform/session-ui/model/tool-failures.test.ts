import { describe, expect, it } from "vitest";
import { isEmphasizedToolFailure } from "./tool-failures";
import type { TranscriptToolCall } from "./transcript";

const failed = (over: Partial<TranscriptToolCall> = {}): TranscriptToolCall => ({
  callId: "call", toolName: "custom_tool", status: "failed", isError: true, ...over,
});

describe("tool failure emphasis", () => {
  it.each([
    "env.read_file", "env.write_file", "env.edit_file", "env.apply_patch", "env.grep", "env.glob", "env.list_dir",
    "env.reference", "env.run_process", "env.continue_process", "env.job_submit", "env.job_run", "env.job_read",
  ])("mutes %s independently of its model-facing name or display group", (toolId) => {
    expect(isEmphasizedToolFailure(failed({ toolId }))).toBe(false);
  });

  it.each(["read_file", "Read", "exec_command", "Bash", "write_stdin", "BashOutput", "job_run", "job_read"])(
    "recognizes historical %s calls without a recorded tool ID", (toolName) => {
      expect(isEmphasizedToolFailure(failed({ toolName }))).toBe(false);
    },
  );

  it.each(["web.fetch", "mcp.read_file", "vfs.read_file", "env.vfs_capture", "unknown"])(
    "keeps %s failures prominent even with an environment-like name and display", (toolId) => {
      expect(isEmphasizedToolFailure(failed({
        toolId, toolName: "read_file", display: { group: "explore", verb: "Read" },
      }))).toBe(true);
    },
  );

  it.each<Partial<TranscriptToolCall>>([
    { status: "failed", isError: false },
    { status: "unavailable", isError: false },
    { status: "succeeded", isError: true },
  ])("applies the same emphasis policy to every failure representation: %j", (over) => {
    expect(isEmphasizedToolFailure(failed(over))).toBe(true);
    expect(isEmphasizedToolFailure(failed({ ...over, toolId: "env.run_process" }))).toBe(false);
  });

  it("does not emphasize successful or cancelled calls", () => {
    expect(isEmphasizedToolFailure(failed({ status: "succeeded", isError: false }))).toBe(false);
    expect(isEmphasizedToolFailure(failed({ status: "cancelled" }))).toBe(false);
  });
});
