import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createDemoStore } from "./fixtures";
import { SOFTWARE_FACTORY_UNIVERSE_ID } from "./fixtures/software-factory";
import { decideApprovals, startRun } from "./engine";
import { debugTurn } from "./debug-commands";
import type { DemoStore, SessionRecord, UniverseState } from "./store";

/// Each command through a real demo store: a message is sent, time passes,
/// and the state the UI reads has changed.

let store: DemoStore;
let universe: UniverseState;
let session: SessionRecord;

beforeEach(() => {
  vi.useFakeTimers();
  store = createDemoStore();
  // As the demo entry does in debug mode.
  store.debug = debugTurn;
  universe = store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)!;
  session = universe.sessions.get("session-flaky-scheduler")!;
});
afterEach(() => {
  vi.clearAllTimers();
  vi.useRealTimers();
});

const send = (text: string, target = session) => startRun(store, universe, target, { text });
const lastReply = (target = session) =>
  [...target.activeContext.entries].reverse().find((entry) => entry.kind.type === "message" && entry.kind.role === "assistant")?.text ?? "";

describe("demo debug commands", () => {
  it("answers debug messages like any other without debug mode", () => {
    store.debug = undefined;
    send("debug: long think");
    vi.advanceTimersByTime(5_000);
    expect(lastReply()).not.toContain("Done thinking");
  });

  it("leaves ordinary messages to the scripted reply", () => {
    send("Fix the flaky test");
    vi.advanceTimersByTime(20_000);
    expect(lastReply()).not.toContain("Debug commands");
  });

  it("reads the command after the prefix in any case and spacing, and lists the commands on help or an unknown one", () => {
    send("  DEBUG:   Help ");
    vi.advanceTimersByTime(5_000);
    expect(lastReply()).toContain("`debug: long think`");
    send("debug: fly");
    vi.advanceTimersByTime(5_000);
    expect(lastReply()).toContain("Unknown debug command `fly`");
  });

  it("keeps a long-think run thinking for 15 seconds before it answers", () => {
    const run = send("debug: long think");
    const before = session.activeContext.entries.length;
    vi.advanceTimersByTime(14_000);
    expect(run.status).toBe("running");
    expect(session.activeContext.entries.length).toBe(before);
    vi.advanceTimersByTime(2_000);
    expect(run.status).toBe("completed");
    expect(session.activeContext.entries.slice(before).map((entry) => entry.kind.type)).toEqual(["reasoningState", "message"]);
  });

  it.each([["approve", "Approved"], ["reject", "Rejected"]] as const)("parks on an approval and goes on once it is decided (%s)", (decision, answer) => {
    const run = send("debug: approval");
    vi.advanceTimersByTime(5_000);
    expect(run.status).toBe("parked");
    expect(session.view.activity).toBe("waiting");
    const [pending] = run.pendingApprovals!;
    expect(pending!.subject.toolName).toBe("create_issue");
    // A message sent meanwhile waits behind the parked run.
    const queued = send("Anything else?");
    expect(queued.status).toBe("queued");

    decideApprovals(session, run, [{ approvalId: pending!.approvalId, decision }]);
    expect(run.status).toBe("running");
    vi.advanceTimersByTime(5_000);
    expect(run.status).toBe("completed");
    expect(session.activeContext.entries.some((entry) => entry.text?.startsWith(answer))).toBe(true);
  });

  it.each([["failed", "failed"], ["timed out", "timedOut"], ["terminated", "terminated"], ["missing", "missing"]] as const)(
    "stops the workflow (%s)",
    (words, failure) => {
      send(`debug: stop ${words}`);
      vi.advanceTimersByTime(5_000);
      expect(session.view.executionFailure).toBe(failure);
    },
  );

  it("closes the session after its reply", () => {
    send("debug: close");
    vi.advanceTimersByTime(5_000);
    expect(session.view.lifecycleStatus).toBe("closed");
  });

  it("fails the run with a provider error and no reply", () => {
    const run = send("debug: error");
    vi.advanceTimersByTime(5_000);
    expect(run.status).toBe("failed");
    expect(session.events.at(-1)?.kind).toMatchObject({ type: "runFailed", kind: "modelFailure" });
  });

  it("queues follow-ups that run one after another", () => {
    const run = send("debug: queue 2");
    vi.advanceTimersByTime(4_500);
    const runs = [...session.runs.values()];
    // The debug run ends as its reply lands; the first follow-up starts, the second waits.
    expect(run.status).toBe("completed");
    expect(runs.slice(-2).map((queued) => queued.status)).toEqual(["running", "queued"]);
    vi.advanceTimersByTime(30_000);
    expect(run.status).toBe("completed");
    expect(lastReply()).toBe("Follow-up 2 done.");
  });

  it("delegates to sub-agents that work and finish on their own", () => {
    send("debug: subagents 2");
    vi.advanceTimersByTime(4_000);
    const children = [...universe.sessions.values()].filter((child) => child.view.origin?.parentSessionId === session.view.id && child.view.displayName?.startsWith("Debug"));
    expect(children).toHaveLength(2);
    expect(children.every((child) => child.view.activity === "working")).toBe(true);
    vi.advanceTimersByTime(20_000);
    expect(children.every((child) => child.view.activity === "idle")).toBe(true);
  });

  it("sets the status of the session's environment", () => {
    const ci = universe.sessions.get("session-ci-slow") ?? [...universe.sessions.values()].find((candidate) => candidate.view.activeEnvironmentId);
    const environment = universe.environments.get(ci!.view.activeEnvironmentId!)!;
    send("debug: env failed", ci);
    vi.advanceTimersByTime(5_000);
    expect(environment.status).toBe("failed");
    send("debug: env ready", ci);
    vi.advanceTimersByTime(5_000);
    expect(environment.status).toBe("ready");
  });

  it("puts this session's bot in a state, adds waiting events and a failed one", () => {
    const record = universe.bots.get("ci-watch")!;
    const main = universe.sessions.get(record.state.mainSessionId)!;
    send("debug: bot paused", main);
    vi.advanceTimersByTime(5_000);
    expect(record.bot.enabled).toBe(false);

    send("debug: bot attention", main);
    vi.advanceTimersByTime(5_000);
    expect(record.bot.enabled).toBe(true);
    expect(record.state.controllerStatus).toBe("degraded");

    send("debug: bot events 4", main);
    vi.advanceTimersByTime(5_000);
    expect(record.events.filter((event) => event.outcome == null)).toHaveLength(4);

    send("debug: bot failed", main);
    vi.advanceTimersByTime(5_000);
    expect(record.events.at(-1)?.outcome).toBe("runFailed");

    send("debug: bot closed", main);
    vi.advanceTimersByTime(5_000);
    expect(record.bot.closedAtMs).not.toBeNull();
  });

  it("says so when a bot command is sent outside a bot's sessions", () => {
    send("debug: bot paused");
    vi.advanceTimersByTime(5_000);
    expect(lastReply()).toContain("not one of a bot's sessions");
  });
});
