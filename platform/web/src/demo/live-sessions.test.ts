import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { createDemoStore } from "./fixtures";
import { createDemoRouter } from "./router";
import { activeRun } from "./engine";
import type { SessionRecord } from "./store";

/// The Software Factory sessions that are live when the demo opens: one
/// waiting on an approval, one that starts thinking at the first look at
/// the Sessions page and answers sooner once it is opened.

function boot() {
  const store = createDemoStore();
  const app = createDemoRouter(store);
  const universe = store.universeBySlug("software-factory")!;
  const base = `/api/v1/universes/${universe.universe.id}/sessions`;
  const call = (method: string, path = "", body?: unknown) => app.request(`${base}${path}`, {
    method, headers: { "content-type": "application/json" }, ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  return { universe, call };
}

beforeEach(() => {
  vi.useFakeTimers();
});
afterEach(() => vi.useRealTimers());

const reasoning = (session: SessionRecord) =>
  session.activeContext.entries.filter((entry) => entry.kind.type === "reasoningState").length;

it("opens with the release waiting on its Production deploy, which an approval lets through", async () => {
  const { universe, call } = boot();
  const release = universe.sessions.get("session-release-v2-14-1")!;
  const run = activeRun(release)!;
  expect(release.view.activity).toBe("waiting");
  expect(run.status).toBe("parked");
  expect(run.pendingApprovals).toHaveLength(1);
  const subject = run.pendingApprovals![0]!.subject;
  expect(subject).toMatchObject({ kind: "mcpToolCall", serverId: "octopus", toolName: "deploy_release" });
  // The server it waits on is one that asks before every call.
  expect(universe.mcpServers.get("octopus")?.approval).toBe("always");

  const approvalId = run.pendingApprovals![0]!.approvalId;
  expect((await call("POST", `/${release.view.id}/runs/${run.id}/approvals`, { decisions: [{ approvalId, decision: "approve" }] })).status).toBe(200);
  await vi.advanceTimersByTimeAsync(10_000);
  expect(run.status).toBe("completed");
  expect(release.view.activity).toBe("idle");
});

it("holds the release when the deploy is rejected", async () => {
  const { universe, call } = boot();
  const release = universe.sessions.get("session-release-v2-14-1")!;
  const run = activeRun(release)!;
  const approvalId = run.pendingApprovals![0]!.approvalId;
  await call("POST", `/${release.view.id}/runs/${run.id}/approvals`, { decisions: [{ approvalId, decision: "reject" }] });
  await vi.advanceTimersByTimeAsync(10_000);
  expect(run.status).toBe("completed");
  expect(release.activeContext.entries.at(-1)?.text).toContain("Held.");
});

it("waits only on servers that ask for approval", () => {
  const { universe } = boot();
  for (const session of universe.sessions.values()) {
    for (const approval of activeRun(session)?.pendingApprovals ?? []) {
      if (approval.subject.kind !== "mcpToolCall") continue;
      expect(universe.mcpServers.get(approval.subject.serverId)?.approval, session.view.id).toBe("always");
    }
  }
});

it("starts the auth session thinking at the first list and answers after about 25 s", async () => {
  const { universe, call } = boot();
  const auth = universe.sessions.get("session-auth-middleware")!;
  const before = reasoning(auth);
  expect(auth.view.activity).toBe("idle");

  await call("GET", "?limit=50");
  expect(auth.view.activity).toBe("working");
  const run = activeRun(auth)!;
  // A second look does not start another run.
  await call("GET", "?limit=50");
  expect(auth.view.runs?.filter((summary) => summary.status === "running")).toHaveLength(1);

  await vi.advanceTimersByTimeAsync(20_000);
  expect(run.status).toBe("running");
  await vi.advanceTimersByTimeAsync(6_000);
  expect(run.status).toBe("completed");
  expect(reasoning(auth)).toBe(before + 1);
});

it("answers within 5 s once the thinking session is opened", async () => {
  const { universe, call } = boot();
  const auth = universe.sessions.get("session-auth-middleware")!;
  await call("GET", "?limit=50");
  const run = activeRun(auth)!;
  await vi.advanceTimersByTimeAsync(2_000);
  await call("GET", `/${auth.view.id}`);
  await vi.advanceTimersByTimeAsync(4_000);
  expect(run.status).toBe("running");
  await vi.advanceTimersByTimeAsync(1_500);
  expect(run.status).toBe("completed");
});

it("starts and hurries the thinking session when it is opened first", async () => {
  const { universe, call } = boot();
  const auth = universe.sessions.get("session-auth-middleware")!;
  await call("GET", `/${auth.view.id}`);
  const run = activeRun(auth)!;
  expect(run.status).toBe("running");
  await vi.advanceTimersByTimeAsync(5_500);
  expect(run.status).toBe("completed");
  // Lists after that start nothing new.
  await call("GET", "?limit=50");
  expect(activeRun(auth)).toBeNull();
});

it("leaves the lineage lookup alone", async () => {
  const { universe, call } = boot();
  const auth = universe.sessions.get("session-auth-middleware")!;
  await call("GET", "?parent=session-flaky-scheduler");
  expect(auth.view.activity).toBe("idle");
});
