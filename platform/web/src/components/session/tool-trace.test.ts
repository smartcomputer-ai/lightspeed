import { createElement } from "react";
import { renderToStaticMarkup, renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { TranscriptToolCall, TranscriptToolGroup } from "@/lib/sessions/transcript";
import {
  ToolGroupTrace,
  TranscriptLinksContext,
  batchTitle,
  shortenTarget,
  subagentSessionId,
  type TranscriptLinks,
} from "./tool-trace";

const call = (over: Partial<TranscriptToolCall> = {}): TranscriptToolCall => ({
  callId: "call-1", toolName: "read_file", status: "succeeded", isError: false, ...over,
});
const group = (calls: TranscriptToolCall[], status: TranscriptToolGroup["status"] = "succeeded"): TranscriptToolGroup =>
  ({ kind: "tool-group", key: "batch", status, calls });
const render = (entry: TranscriptToolGroup, links?: TranscriptLinks) => {
  const trace = createElement(ToolGroupTrace, { group: entry });
  return renderToStaticMarkup(links ? createElement(TranscriptLinksContext.Provider, { value: links }, trace) : trace);
};
const text = (html: string) => html.replace(/<[^>]+>/g, " ").replace(/\s+/g, " ").trim();

describe("tool trace names", () => {
  it.each(["exec_command", "Bash", "run_process"])("renders the recorded name %s for the same builtin", (toolName) => {
    const html = renderToString(createElement(ToolGroupTrace, {
      group: group([call({ toolId: "env.run_process", toolName })]),
    }));
    expect(html).toContain(toolName);
    expect(html).not.toContain("env.run_process");
  });
});

describe("step rows", () => {
  it("renders a finished call as one quiet row: verb, target, duration, and no badge", () => {
    const html = render(group([call({
      display: { group: "explore", verb: "Read", target: "/workspace/notes.md" }, durationMs: 340,
    })]));
    expect(text(html)).toBe("Read /workspace/notes.md 340ms");
    expect(html).not.toMatch(/Done|Completed|Succeeded/);
  });

  it("marks a failed call with its first error line while the details stay closed", () => {
    const html = render(group([call({
      status: "failed", isError: true, error: "exit 101\nerror: unused variable",
      display: { group: "execute", verb: "Run", target: "cargo clippy" }, durationMs: 12_000,
    })], "completedWithErrors"));
    expect(text(html)).toBe("Run cargo clippy failed · 12s exit 101");
    expect(html).not.toContain("unused variable");
  });

  it("shows cancelled and waiting states without inventing a duration", () => {
    expect(text(render(group([call({ status: "cancelled", display: { group: "execute", verb: "Run", target: "sleep 9" } })], "cancelled"))))
      .toBe("Run sleep 9 cancelled");
    expect(text(render(group([call({ status: "running", display: { group: "agent", verb: "Await", target: "promise_3" } })], "waiting"))))
      .toBe("Await promise_3 waiting");
  });

  it("names a batch from its rows and counts its failures", () => {
    const reads = [1, 2, 3].map((index) => call({
      callId: `c${index}`, display: { group: "explore", verb: "Read", target: `/w/${index}.ts` }, startedAtMs: 100, completedAtMs: 100 + index * 100,
    }));
    const html = render(group(reads));
    expect(text(html)).toContain("Read 3 files 300ms");
    expect(html).not.toMatch(/Done/);
    expect(batchTitle([call({ display: { group: "explore", verb: "Read" } }), call({ display: { group: "execute", verb: "Run" } })])).toBe("2 tool calls");
    expect(batchTitle([call({ display: { group: "mcp", verb: "stripe" } }), call({ display: { group: "mcp", verb: "stripe" } })])).toBe("stripe × 2");

    const mixed = render(group([reads[0]!, call({ callId: "bad", status: "failed", isError: true, display: { group: "explore", verb: "Read" } })], "completedWithErrors"));
    expect(text(mixed)).toContain("Read 2 files 1 failed");
  });

  it("names peer bots on emit rows and shows self-emits as such", () => {
    const links: TranscriptLinks = { botName: (id) => ({ escalations: "Escalations" })[id] };
    const peer = render(group([call({
      toolName: "bot_emit", argumentsJson: '{"to":"escalations","kind":"bug.confirmed"}',
      display: { group: "bot", verb: "Emit", target: "to escalations", detail: "bug.confirmed · reply requested" },
    })]), links);
    expect(text(peer)).toBe("Emit to Escalations bug.confirmed · reply requested");
    const self = render(group([call({
      toolName: "bot_emit", display: { group: "bot", verb: "Emit", target: "to self", detail: "research.followup" },
    })]), links);
    expect(text(self)).toBe("Emit to self research.followup");
  });

  it("links a delegate row to the child session named in the result envelope", () => {
    const html = render(group([call({
      toolName: "agent_run", output: '{"agent":"reviewer","session_id":"child-7","status":"completed"}',
      display: { group: "agent", verb: "Delegate", target: "reviewer", detail: "Review the diff" },
    })]), { sessionHref: (id) => `/u/acme/sessions/${id}` });
    expect(html).toContain('href="/u/acme/sessions/child-7"');
    expect(text(html)).toContain("open session");
    expect(subagentSessionId('{"sessionId":"x"}')).toBe("x");
    expect(subagentSessionId("not json")).toBeNull();
    expect(subagentSessionId('{"promise":"promise_3"}')).toBeNull();
  });

  it("shortens long environment paths on the row and keeps the full path as the title", () => {
    const full = "/Users/lukas/dev/lightspeed/.lightspeed-dev/envd/workspace/agent_node_demo.js";
    expect(shortenTarget(full)).toBe("…/lightspeed/.lightspeed-dev/envd/workspace/agent_node_demo.js");
    expect(shortenTarget("/short/path.ts")).toBe("/short/path.ts");
    expect(shortenTarget("x".repeat(80))).toBe("x".repeat(80));
    const html = render(group([call({ display: { group: "explore", verb: "Read", target: full } })]));
    expect(html).toContain(`title="${full}"`);
    expect(text(html)).toBe("Read …/lightspeed/.lightspeed-dev/envd/workspace/agent_node_demo.js");
  });

  it("labels a call whose start is outside the loaded window", () => {
    const html = render(group([call({ toolName: "Tool activity (continued)", status: "running", continuation: true })], "running"));
    expect(text(html)).toBe("Tool activity started before the loaded history");
  });
});
