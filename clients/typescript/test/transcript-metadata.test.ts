import { describe, expect, it } from "vitest";
import {
  transcriptAuthor, transcriptMetadata,
  type Attribution, type ContextEntryView, type SessionEventView,
} from "../src/index.js";

const event = (seq: number, observedAtMs: number, kind: SessionEventView["kind"], sessionId = "s"): SessionEventView => ({
  sessionId, cursor: { seq }, observedAtMs, joins: {}, kind,
});
const input = (id: string, source: ContextEntryView["source"] = { type: "runInput", runId: "1", inputIndex: 0 }): ContextEntryView => ({
  id, kind: { type: "message", role: "user" }, origin: "user:claimed",
  content: { blobRef: `blob-${id}` }, ...(source ? { source } : {}),
});
const applied = (seq: number, at: number, entries: ContextEntryView[], sessionId = "s") => event(seq, at, {
  type: "contextEntriesApplied", baseRevision: 0, revision: 1, entries,
}, sessionId);
const accepted = (seq: number, at: number, runId = "1", requestedBy: Attribution | null = { kind: "actor", id: "alice" }, sessionId = "s") => event(seq, at, {
  type: "runAccepted", runId, requestedBy, source: { type: "input", entries: [] },
}, sessionId);
const steered = (seq: number, at: number, runId: string, steeringId: string, id: string) => event(seq, at, {
  type: "runSteeringAccepted", runId, steeringId, requestedBy: { kind: "actor", id }, input: [],
});

describe("transcriptAuthor", () => {
  it("prefers authenticated actors and preserves opaque IDs", () => {
    expect(transcriptAuthor("user:claimed", { kind: "actor", id: "tenant:alice" })).toEqual({
      kind: "actor", id: "tenant:alice", source: "requestedBy",
    });
    expect(transcriptAuthor(undefined, { kind: "actor", id: "alice" })).toEqual({
      kind: "actor", id: "alice", source: "requestedBy",
    });
    expect(transcriptAuthor("user:tenant:alice")).toEqual({ kind: "actor", id: "tenant:alice", source: "origin" });
  });

  it("keeps event delivery distinct and leaves other origins unknown", () => {
    expect(transcriptAuthor("event", { kind: "actor", id: "alice" })).toEqual({ kind: "event" });
    for (const origin of [null, undefined, "", "user:", "user", "integration:worker", "User:alice"]) {
      expect(transcriptAuthor(origin)).toBeNull();
    }
  });

  it.each<Attribution>([
    { kind: "key", keyId: "service", scopeKind: "universe" },
    { kind: "internal", component: "bot", cause: "delivery" },
    { kind: "local" },
  ])("does not turn $kind attribution into a person", requestedBy => {
    expect(transcriptAuthor(null, requestedBy)).toBeNull();
    expect(transcriptAuthor("user:claimed", requestedBy)).toEqual({ kind: "actor", id: "claimed", source: "origin" });
  });
});

describe("transcriptMetadata", () => {
  it("uses acceptance time for queued input and introduction time for assistant output", () => {
    const output: ContextEntryView = { ...input("reply", { type: "assistantOutput", runId: "1", turnId: "1" }), kind: { type: "message", role: "assistant" } };
    const events = [accepted(1, 0), event(2, 100, { type: "runStarted", runId: "1" }), applied(3, 110, [input("message")]), applied(4, 150, [output])];
    const result = transcriptMetadata("s", events);
    expect(result.get("message")).toEqual({
      sentAtMs: 0, timeSource: "runAccepted", author: { kind: "actor", id: "alice", source: "requestedBy" },
      requestedBy: { kind: "actor", id: "alice" },
    });
    expect(result.get("reply")).toEqual({ sentAtMs: 150, timeSource: "contextEvent", author: null, requestedBy: null });
  });

  it("corrects time and attribution when an older acceptance page arrives", () => {
    const recent = [applied(10, 200, [input("message")])];
    const first = transcriptMetadata("s", recent);
    expect(first.get("message")).toEqual({
      sentAtMs: 200, timeSource: "contextEvent", author: { kind: "actor", id: "claimed", source: "origin" }, requestedBy: null,
    });
    const older = [accepted(1, 100)];
    const expected = transcriptMetadata("s", [...older, ...recent]);
    expect(transcriptMetadata("s", [...recent, ...older, ...recent, ...older])).toEqual(expected);
    expect(expected.get("message")).toMatchObject({ sentAtMs: 100, timeSource: "runAccepted", author: { id: "alice", source: "requestedBy" } });
    expect(first.get("message")!.sentAtMs).toBe(200);
  });

  it("joins steering by both run and steering ID, without borrowing run acceptance", () => {
    const a = input("a", { type: "steering", runId: "run:a", steeringId: "b", inputIndex: 0 });
    const b = input("b", { type: "steering", runId: "run", steeringId: "a:b", inputIndex: 0 });
    const c = input("c", { type: "steering", runId: "run:a", steeringId: "unloaded", inputIndex: 0 });
    const events = [accepted(1, 5, "run:a"), steered(2, 20, "run:a", "b", "alice"), steered(3, 30, "run", "a:b", "bob"), applied(4, 100, [a, b, c])];
    const result = transcriptMetadata("s", [...events].reverse());
    expect(result.get("a")).toMatchObject({ sentAtMs: 20, timeSource: "runSteeringAccepted", author: { id: "alice" } });
    expect(result.get("b")).toMatchObject({ sentAtMs: 30, timeSource: "runSteeringAccepted", author: { id: "bob" } });
    expect(result.get("c")).toMatchObject({ sentAtMs: 100, timeSource: "contextEvent", author: { id: "claimed", source: "origin" } });
  });

  it.each(["contextEntriesReplaced", "contextKeyPrefixReplaced"] as const)("uses the first loaded sequence when %s repeats a message", type => {
    const message = input("message", { type: "contextEdit" });
    const original = applied(2, 500, [message]);
    const repeated = type === "contextKeyPrefixReplaced"
      ? event(20, 300, { type, entries: [message], baseRevision: 1, revision: 2, keyPrefix: "notes/" })
      : event(20, 300, { type, entries: [message], baseRevision: 1, revision: 2 });
    expect(transcriptMetadata("s", [repeated]).get(message.id)!.sentAtMs).toBe(300);
    expect(transcriptMetadata("s", [repeated, original, repeated]).get(message.id)!.sentAtMs).toBe(500);
  });

  it("does not invent acceptance for context edits or source-less messages from envelope joins", () => {
    const addition = applied(2, 40, [input("edit", { type: "contextEdit" }), input("old", null)]);
    addition.joins.runId = "1";
    const result = transcriptMetadata("s", [accepted(1, 10), addition]);
    for (const id of ["edit", "old"]) expect(result.get(id)).toMatchObject({ sentAtMs: 40, timeSource: "contextEvent", requestedBy: null, author: { source: "origin" } });
  });

  it("retains non-actor attribution and the bot delivery classification", () => {
    const requestedBy: Attribution = { kind: "internal", component: "bot", cause: "delivery" };
    const message = { ...input("message"), origin: "event" };
    const result = transcriptMetadata("s", [accepted(1, 10, "1", requestedBy), applied(2, 20, [message])]);
    expect(result.get(message.id)).toEqual({ sentAtMs: 10, timeSource: "runAccepted", author: { kind: "event" }, requestedBy });
    const projected = result.get(message.id)!.requestedBy;
    if (projected?.kind !== "internal") throw new Error("Expected internal attribution");
    projected.cause = "changed copy";
    expect(requestedBy.cause).toBe("delivery");
  });

  it("uses acceptance time even when no authenticated requester was recorded", () => {
    const result = transcriptMetadata("s", [accepted(1, 10, "1", null), applied(2, 20, [input("message")])]);
    expect(result.get("message")).toMatchObject({ sentAtMs: 10, timeSource: "runAccepted", requestedBy: null, author: { source: "origin" } });
  });

  it("isolates sessions, ignores nonmessages, and does not fabricate queued message IDs", () => {
    const message = input("same-id");
    const tool: ContextEntryView = { id: "tool", kind: { type: "toolCall", callId: "c", name: "tool" }, content: { blobRef: "blob-tool" } };
    const events = [accepted(1, 10), applied(2, 20, [message, tool]), accepted(1, 999, "1", { kind: "actor", id: "other" }, "other"), applied(2, 1000, [message], "other")];
    expect(transcriptMetadata("s", events).size).toBe(1);
    expect(transcriptMetadata("s", events).get(message.id)).toMatchObject({ sentAtMs: 10, author: { id: "alice" } });
    expect(transcriptMetadata("other", events).get(message.id)).toMatchObject({ sentAtMs: 999, author: { id: "other" } });
    expect(transcriptMetadata("missing", events).size).toBe(0);
    expect(transcriptMetadata("s", [accepted(1, 10)]).size).toBe(0);
    expect(transcriptMetadata("s", []).size).toBe(0);
  });

  it("does not mutate the supplied events", () => {
    const events = [applied(2, 20, [input("message")]), accepted(1, 10)];
    const original = structuredClone(events);
    transcriptMetadata("s", Object.freeze(events));
    expect(events).toEqual(original);
  });
});
