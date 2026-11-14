import { expect, it } from "vitest";
import type { SessionEvent } from "@/api";
import { applyEvents, emptyTranscript, reconcileRuns } from "./transcript";
import { projectSubmissions, submissionReducer, type LocalSubmission } from "./submissions";
import { TranscriptWindow } from "./transcript-window";

const local = (kind: "run" | "steering" = "run"): LocalSubmission => ({
  id: "local", kind, text: "same text", queued: false, runId: kind === "run" ? null : "1",
});
const event = (seq: number, kind: SessionEvent["kind"], submissionId?: string): SessionEvent => ({
  sessionId: "session", cursor: { seq }, observedAtMs: seq, joins: { submissionId }, kind,
});
const accepted = event(1, { type: "runAccepted", runId: "1", submissionId: "local", source: { type: "input", entries: [] } });
const terminal = event(3, { type: "runCancelled", runId: "1" });

it("retires cancelled queued input only when the terminal event is consumed", () => {
  let state = applyEvents(emptyTranscript(), [accepted]);
  const submissions = [{ ...local(), queued: true }];
  expect(projectSubmissions(state, submissions).queued.has("local")).toBe(true);
  state = reconcileRuns(state, [{ id: "1", status: "cancelled", acceptedAtMs: 1, source: { type: "input" } }]);
  expect(projectSubmissions(state, submissions).pendingRuns).toHaveLength(1);
  state = applyEvents(state, [terminal]);
  const view = projectSubmissions(state, submissions);
  expect(view.pendingRuns).toHaveLength(0);
  expect(view.queuedRuns).toHaveLength(0);
  expect(view.sections).toHaveLength(0);
});

it("keeps a genuinely queued send out of the transcript until it starts, despite stale receipts", () => {
  const submissions = [{ ...local(), queued: true, status: "queued" as const, runId: "1" }];
  let state = applyEvents(emptyTranscript(), [accepted]);
  expect(projectSubmissions(state, submissions).sections).toHaveLength(0);
  state = applyEvents(state, [event(2, { type: "runStarted", runId: "1" })]);
  const view = projectSubmissions(state, submissions);
  expect(view.queuedRuns).toHaveLength(0);
  expect(view.queued.size).toBe(0);
  expect(view.sections[0]).toMatchObject({ key: "local", input: { key: "local" } });
});

it.each([false, true])("moves an idle guess into the queue when another run is ahead (started: %s)", started => {
  let state = applyEvents(emptyTranscript(), [
    event(1, { type: "runAccepted", runId: "other", submissionId: "remote", source: { type: "input", entries: [] } }),
    ...(started ? [event(2, { type: "runStarted", runId: "other" })] : []),
    { ...accepted, cursor: { seq: 3 } },
  ]);
  const submissions = [local()];
  const queued = projectSubmissions(state, submissions);
  expect(queued.queued.has("local")).toBe(true);
  expect(queued.queuedRuns.map(run => run.runId)).toContain("1");
  expect(queued.inlinePending).toHaveLength(0);
  expect(queued.sections.some(section => section.kind === "run" && section.input?.key === "local")).toBe(false);

  state = applyEvents(state, [event(4, { type: "runStarted", runId: "1" })]);
  const running = projectSubmissions(state, submissions);
  expect(running.queued.size).toBe(0);
  expect(running.inlinePending).toHaveLength(1);
  expect(running.queuedRuns.map(run => run.runId)).not.toContain("1");
});

it.each(["completed", "failed"] as const)("takes a %s receipt out of the queue but retains its echo until the tail catches up", status => {
  const state = applyEvents(emptyTranscript(), [accepted]);
  const submissions = submissionReducer([{ ...local(), queued: true }], {
    type: "accepted", id: "local", runId: "1", status,
  });
  const view = projectSubmissions(state, submissions);
  expect(view.queuedRuns).toHaveLength(0);
  expect(view.queued.size).toBe(0);
  expect(view.sections[0]).toMatchObject({ key: "local", input: { key: "local" } });
  const outcome = event(3, status === "completed" ? { type: "runCompleted", runId: "1" }
    : { type: "runFailed", runId: "1", kind: "requestRejected", message: "Rejected" });
  expect(projectSubmissions(applyEvents(state, [outcome]), submissions).pendingRuns).toHaveLength(0);
});

it("matches steering by identity, preserves attachment previews, and retains the join across older pages", () => {
  const window = new TranscriptWindow();
  window.append([
    event(2, { type: "runStarted", runId: "1" }),
    event(3, { type: "runSteeringAccepted", runId: "1", steeringId: "steer", input: [] }, "local"),
    event(4, { type: "contextEntriesApplied", baseRevision: 0, revision: 1, entries: [{
      id: "durable", kind: { type: "message", role: "user" },
      content: { blobRef: "image", mime: "image/png", mediaHandle: "media:image" },
      source: { type: "steering", runId: "1", steeringId: "steer", inputIndex: 0 },
    }] }),
  ]);
  window.prepend([accepted]);
  const media = [{ handle: "media:image", blobRef: "image", mime: "image/png", kind: "image" as const, localUrl: "blob:local" }];
  const submissions = [{ ...local("steering"), text: "", media }];
  const original = structuredClone(window.state);
  const view = projectSubmissions(window.state, submissions);
  expect(view.sections[0]).toMatchObject({ work: [{ key: "local", local: true, steeringId: "steer", media }] });
  expect(view.notices).toHaveLength(0);
  expect(window.state).toEqual(original);
});

it("reports undelivered steering after terminal events rather than an early session snapshot", () => {
  let state = applyEvents(emptyTranscript(), [accepted, event(2, { type: "runStarted", runId: "1" })]);
  const submissions = [local("steering")];
  state = reconcileRuns(state, [{ id: "1", status: "completed", acceptedAtMs: 1, source: { type: "input" } }]);
  expect(projectSubmissions(state, submissions).notices).toHaveLength(0);
  state = applyEvents(state, [terminal]);
  const view = projectSubmissions(state, submissions);
  expect(view.notices).toEqual([{ id: "local", text: expect.stringContaining("steering not delivered") }]);
});

it("keeps interim notes before pending steering when a terminal snapshot precedes the tail", () => {
  let state = applyEvents(emptyTranscript(), [accepted, event(2, { type: "runStarted", runId: "1" })]);
  state.entries = [{ kind: "message", key: "note", role: "assistant", text: "Checking", runId: "1" }];
  const submissions = [local("steering")];
  const before = projectSubmissions(state, submissions).sections;
  state = reconcileRuns(state, [{ id: "1", status: "completed", acceptedAtMs: 1, source: { type: "input" } }]);
  expect(state.activeRun).toBeNull();
  const view = projectSubmissions(state, submissions);
  expect(view.sections).toEqual(before);
  expect(view.sections[0]).toMatchObject({ live: true, work: [{ key: "note" }, { key: "local" }] });
});

it("a lost POST response does not remove already confirmed input or change its identity", () => {
  const state = applyEvents(emptyTranscript(), [accepted, event(2, {
    type: "contextEntriesApplied", baseRevision: 0, revision: 1, entries: [{
      id: "server", kind: { type: "message", role: "user" }, text: "same text", content: { blobRef: "text" },
      source: { type: "runInput", runId: "1", inputIndex: 0 },
    }],
  })]);
  const withdrawn = submissionReducer([local()], { type: "withdrawn", id: "local" });
  expect(projectSubmissions(state, withdrawn).sections[0]).toMatchObject({ key: "local", input: { key: "local", text: "same text" } });
});
