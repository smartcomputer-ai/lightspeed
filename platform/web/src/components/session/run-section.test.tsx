// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { sectionsByRun, type RunSection } from "@/lib/sessions/run-sections";
import type { ActiveRun, TranscriptEntry, TranscriptToolCall } from "@/lib/sessions/transcript";
import { RunSectionView } from "./run-section";

let root: Root;
let container: HTMLDivElement;

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

const read = (callId: string, over: Partial<TranscriptToolCall> = {}): TranscriptToolCall => ({
  callId, toolName: "read_file", status: "succeeded", isError: false,
  argumentsJson: '{"path":"/w/a.ts"}', output: "contents",
  display: { group: "explore", verb: "Read", target: "/w/a.ts" },
  startedAtMs: 1_700_000_000_000, completedAtMs: 1_700_000_000_340, durationMs: 340, outputBytes: 412,
  ...over,
});

function finished(status: "completed" | "failed" = "failed"): RunSection {
  const entries: TranscriptEntry[] = [
    { kind: "message", key: "ask", role: "user", text: "Fix the test", runId: "r1" },
    { kind: "reasoning", key: "think", text: "**Look at the reaper**", runId: "r1" },
    { kind: "tool-group", key: "batch", runId: "r1", status: "completedWithErrors", calls: [
      read("c1"),
      read("c2", { toolName: "exec_command", status: "failed", isError: true, error: "exit 101",
        display: { group: "execute", verb: "Run", target: "cargo clippy" } }),
    ] },
    { kind: "message", key: "note", role: "assistant", text: "Interim progress note.", runId: "r1" },
    { kind: "message", key: "reply", role: "assistant", text: "All fixed.", runId: "r1" },
    { kind: "run-summary", key: "done", runId: "r1", status, usageComplete: true, durationMs: 38_000,
      contextTokens: 44_000, usage: { inputTokens: 10_000, outputTokens: 2_000, modelCalls: 3 },
      ...(status === "failed" ? { error: "retries exhausted" } : {}) },
  ];
  return sectionsByRun(entries, null)[0] as RunSection;
}

async function render(section: RunSection, options: {
  activeRun?: ActiveRun | null; collapse?: boolean; stats?: boolean;
} = {}) {
  await act(async () => root.render(<RunSectionView
    section={section}
    activeRun={options.activeRun ?? null}
    showRunStatistics={options.stats ?? true}
    collapseCompletedRuns={options.collapse ?? true}
  />));
}
const STRIP = 'button[aria-label$="this run\'s activity"]';
const strip = () => container.querySelector<HTMLButtonElement>(STRIP)!;

it("folds a finished run behind one strip that names the outcome, keeps the reply visible, and mounts nothing inside", async () => {
  await render(finished());
  expect(container.textContent).toContain("Fix the test");
  expect(container.textContent).toContain("All fixed.");
  expect(strip().textContent).toContain("Failed after 38s");
  expect(strip().textContent).toContain("2 tool calls");
  expect(strip().textContent).toContain("1 failed");
  expect(container.textContent).toContain("Run failed: retries exhausted");
  expect(container.textContent).toContain("Context 44k");
  expect(container.textContent).not.toContain("Interim progress note.");
  expect(container.textContent).not.toContain("cargo clippy");
  expect(container.querySelector('[aria-label="Run statistics"]')).not.toBeNull();
});

it("opens on click, then reveals a row's details with the meta line, and follows the preference when it changes", async () => {
  await render(finished("completed"));
  expect(strip().textContent).toContain("Worked for 38s");
  await act(async () => strip().click());
  expect(container.textContent).toContain("Interim progress note.");
  expect(container.textContent).toContain("Thinking");
  expect(container.textContent).toContain("cargo clippy");
  expect(container.textContent).toContain("exit 101");
  expect(container.textContent).not.toContain("contents");

  const row = container.querySelector<HTMLButtonElement>('button[aria-label="Read /w/a.ts"]')!;
  await act(async () => row.click());
  expect(container.textContent).toContain("contents");
  const meta = container.querySelector('[aria-label="Call details"]')!.textContent;
  expect(meta).toContain("read_file");
  expect(meta).toContain("c1");
  expect(meta).toContain("340ms");
  expect(meta).toContain("412 B");
  expect(meta).toMatch(/started \d\d:\d\d:\d\d\.\d\d\d/);

  await act(async () => strip().click());
  expect(container.textContent).not.toContain("cargo clippy");
  await render(finished("completed"), { collapse: false });
  expect(container.textContent).toContain("cargo clippy");
  await render(finished("completed"), { collapse: true });
  expect(container.textContent).not.toContain("cargo clippy");
});

it("hides statistics on the strip when the preference is off", async () => {
  await render(finished("completed"), { stats: false });
  expect(container.querySelector('[aria-label="Run statistics"]')).toBeNull();
  expect(strip().textContent).toContain("Worked for 38s");
});

it("streams a live run open with a status row instead of a strip", async () => {
  const entries: TranscriptEntry[] = [
    { kind: "message", key: "ask", role: "user", text: "Retry", runId: "r2" },
    { kind: "tool-group", key: "batch", runId: "r2", status: "running", calls: [read("c1", { status: "running", completedAtMs: undefined, durationMs: undefined })] },
  ];
  const activeRun: ActiveRun = { runId: "r2", label: "running tools", cancelling: false, startedAtMs: Date.now() - 41_000 };
  const section = sectionsByRun(entries, activeRun)[0] as RunSection;
  await render(section, { activeRun });
  expect(container.querySelector(STRIP)).toBeNull();
  expect(container.querySelector('button[aria-label="Read /w/a.ts"]')).not.toBeNull();
  expect(container.textContent).toContain("Read");
  const status = container.querySelector('[role="status"]')!;
  expect(status.textContent).toContain("running tools…");
  expect(status.textContent).toMatch(/4\ds$/);
});

it("shows a run that did no tool work as one quiet line, only when statistics are wanted", async () => {
  const entries: TranscriptEntry[] = [
    { kind: "message", key: "ask", role: "user", text: "Hi", runId: "r3" },
    { kind: "message", key: "reply", role: "assistant", text: "Hello!", runId: "r3" },
    { kind: "run-summary", key: "done", runId: "r3", status: "completed", usageComplete: true, durationMs: 2_000, contextTokens: 900 },
  ];
  const section = sectionsByRun(entries, null)[0] as RunSection;
  await render(section, { stats: false });
  expect(container.textContent).toBe("HiHello!");
  expect(container.querySelector(STRIP)).toBeNull();
  await render(section, { stats: true });
  expect(container.textContent).toContain("Context 900");
  expect(container.querySelector(STRIP)).toBeNull();
});

it("coalesces consecutive context updates into one row of chips inside the work", async () => {
  const entries: TranscriptEntry[] = [
    { kind: "message", key: "ask", role: "user", text: "Go", runId: "r4" },
    { kind: "system", key: "cat-1", text: "Sub-agent catalog", superseded: true },
    { kind: "system", key: "cat-2", text: "Sub-agent catalog (updated)" },
    { kind: "tool-group", key: "batch", runId: "r4", status: "succeeded", calls: [read("c1")] },
    { kind: "run-summary", key: "done", runId: "r4", status: "completed", usageComplete: true },
  ];
  await render(sectionsByRun(entries, null)[0] as RunSection, { collapse: false });
  const chips = container.querySelectorAll('[aria-label="Context updates"]');
  expect(chips).toHaveLength(1);
  expect(chips[0]!.children).toHaveLength(2);
  expect(chips[0]!.children[0]!.className).toContain("opacity-50");
});
