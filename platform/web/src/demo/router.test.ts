import { describe, expect, it } from "vitest";
import { createDemoStore } from "./fixtures";
import { createDemoRouter } from "./router";
import type {
  Universe,
  BotListItem,
  McpServer,
  McpToolDiscovery,
  Environment,
  SessionEventsPage,
  SessionListPage,
  SessionView,
} from "@/api";
import { SOFTWARE_FACTORY_UNIVERSE_ID } from "./fixtures/software-factory";
import { applyEvents, emptyTranscript } from "@/lib/sessions/transcript";
import { appendScriptedRun } from "./engine";

/// Walks the demo router the way the UI does: every read path each page
/// opens with must answer, and the live paths (a message, its tail, a bot
/// event) must move state. A stub the UI needs but the router lacks fails
/// here before it fails in a browser.
async function boot() {
  const store = createDemoStore();
  const app = createDemoRouter(store);
  const call = async (method: string, path: string, body?: unknown) => {
    const response = await app.fetch(
      new Request(`http://demo.local${path}`, {
        method,
        headers: body !== undefined ? { "content-type": "application/json" } : undefined,
        body: body !== undefined ? JSON.stringify(body) : undefined,
      }),
    );
    const text = await response.text();
    return { status: response.status, json: text ? (JSON.parse(text) as unknown) : null };
  };
  return { store, call };
}

const universeReads = [
  "sessions",
  "profiles",
  "workspaces",
  "environments",
  "environment-templates",
  "environment-provider-bindings",
  "environment-registration-keys",
  "mcp-servers",
  "secrets",
  "auth-grants",
  "models",
  "setups",
  "api-keys",
  "members",
  "bots",
  "channel-accounts",
  "channel-pairings",
  "integrations/github",
  "integrations/subscriptions",
];

describe("demo router", () => {
  it("keeps full run output after its entries leave active context", async () => {
    const { store } = await boot();
    const universe = store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)!;
    const session = universe.sessions.get("session-flaky-scheduler")!;
    const text = "A complete response. ".repeat(700);
    const run = appendScriptedRun(store, session, {
      at: Date.now(),
      user: "Explain the result",
      steps: [{ thinking: text, text }],
    });
    const assistant = run.entries?.slice().reverse().find(
      (entry) => entry.kind.type === "message" && entry.kind.role === "assistant",
    );
    session.activeContext.entries = [];
    expect(run.output).toEqual(assistant?.content);
    expect(run.outputText).toBe(text);
    expect(run.entries?.find((entry) => entry.kind.type === "reasoningState")?.text).toBe(text);
    expect(session.events.at(-1)?.kind).toEqual({ type: "runCompleted", runId: run.id, output: run.output });
  });

  it("reads the original bytes of a large tool result for expansion", async () => {
    const { store, call } = await boot();
    const text = "full tool result ".repeat(700);
    const blobRef = store.putText(text);
    const result = await call("GET", `/api/v1/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}/blobs/${encodeURIComponent(blobRef)}`);
    expect(result.status).toBe(200);
    expect(result.json).toEqual({ blobRef, bytesBase64: btoa(text), bytes: text.length });
  });

  it("signs the visitor in as a platform admin without a login", async () => {
    const { call } = await boot();
    const session = await call("GET", "/api/auth/get-session");
    expect(session.status).toBe(200);
    expect((session.json as { user: { role: string } }).user.role).toBe("admin");
  });

  it("answers every read the universe pages open with, for every universe", async () => {
    const { call } = await boot();
    const universes = await call("GET", "/api/v1/universes");
    expect(universes.status).toBe(200);
    const list = universes.json as Universe[];
    expect(list.length).toBeGreaterThanOrEqual(2);
    for (const universe of list) {
      for (const path of universeReads) {
        const result = await call("GET", `/api/v1/universes/${universe.id}/${path}`);
        expect(result.status, `${universe.slug}: ${path}`).toBe(200);
      }
      const bots = ((await call("GET", `/api/v1/universes/${universe.id}/bots`)).json as { bots: BotListItem[] }).bots;
      expect(bots.length, `${universe.slug}: bots`).toBeGreaterThan(0);
      for (const bot of bots) {
        for (const path of ["", "/triggers", "/events", "/state"]) {
          const result = await call("GET", `/api/v1/universes/${universe.id}/bots/${bot.botId}${path}`);
          expect(result.status, `${universe.slug}: bots/${bot.botId}${path}`).toBe(200);
        }
      }
      const sessions = (await call("GET", `/api/v1/universes/${universe.id}/sessions`)).json as SessionListPage;
      expect(sessions.sessions.length, `${universe.slug}: sessions`).toBeGreaterThan(0);
      for (const session of sessions.sessions) {
        for (const path of ["", "/events?limit=200", "/events?direction=backward&limit=200", "/instructions"]) {
          const result = await call("GET", `/api/v1/universes/${universe.id}/sessions/${session.id}${path}`);
          expect(result.status, `${universe.slug}: sessions/${session.id}${path}`).toBe(200);
        }
      }
    }
  });

  it("pages transcript history backward with stable bounds while newer runs arrive", async () => {
    const { store, call } = await boot();
    const universe = store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)!;
    const session = universe.sessions.get("session-flaky-scheduler")!;
    const path = `/api/v1/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}/sessions/${session.view.id}/events?direction=backward`;
    const expected = session.events.map((event) => event.cursor.seq);
    const first = (await call("GET", `${path}&limit=7`)).json as SessionEventsPage;
    expect(first.events?.map((event) => event.cursor.seq)).toEqual(expected.slice(-7));
    appendScriptedRun(store, session, { at: Date.now(), user: "New input", steps: [{ text: "New output" }] });
    const loaded = [...(first.events ?? [])];
    let page = first;
    while (!page.complete) {
      const before = page.nextCursor!.seq;
      page = (await call("GET", `${path}&limit=7&before=${before}`)).json as SessionEventsPage;
      expect(page.events?.every((event) => event.cursor.seq < before)).toBe(true);
      loaded.unshift(...(page.events ?? []));
    }
    expect(loaded.map((event) => event.cursor.seq)).toEqual(expected);
    for (const query of ["before=0", "before=-1", "before=1.5", "before=9007199254740992", "limit=0", "limit=bad"]) {
      expect((await call("GET", `${path}&${query}`)).status).toBe(400);
    }
  });

  it("serves builtin registry IDs alongside the recorded names in demo transcripts", async () => {
    const { call } = await boot();
    const response = await call(
      "GET",
      `/api/v1/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}/sessions/session-flaky-scheduler/events?limit=200`,
    );
    expect(response.status).toBe(200);
    const page = response.json as SessionEventsPage;
    const calls = (page.events ?? []).flatMap((event) =>
      event.kind.type === "toolBatchStarted" ? event.kind.calls : [],
    );
    expect(calls).toEqual(expect.arrayContaining([
      expect.objectContaining({ toolId: "env.run_process", toolName: "exec_command" }),
      expect.objectContaining({ toolId: "env.read_file", toolName: "read_file" }),
    ]));
    const transcript = applyEvents(emptyTranscript(), page.events ?? []);
    const renderedCalls = transcript.entries.flatMap((entry) => entry.kind === "tool-group" ? entry.calls : []);
    for (const call of calls) {
      expect(renderedCalls).toEqual(expect.arrayContaining([
        expect.objectContaining({ callId: call.callId, toolId: call.toolId, toolName: call.toolName }),
      ]));
    }
  });

  it("includes long, filterable evaluation metadata in the software factory", async () => {
    const { call } = await boot();
    const campaign = "terminal-bench-lightspeed-rerun-hosted-20260904-113000-software-factory";
    const result = await call(
      "GET",
      `/api/v1/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}/sessions?metadata=${encodeURIComponent(`campaign=${campaign}`)}`,
    );
    expect(result.status).toBe(200);
    const sessions = (result.json as SessionListPage).sessions;
    expect(sessions.length).toBeGreaterThanOrEqual(2);
    expect(sessions.every((session) => session.metadata?.campaign === campaign)).toBe(true);
    expect(sessions.some((session) => (session.metadata?.workflowRunId?.length ?? 0) > 60)).toBe(true);
  });

  it("answers the admin pages", async () => {
    const { call } = await boot();
    for (const path of [
      "/api/v1/users",
      "/api/v1/universes/reconcile",
      "/api/v1/status/channels",
      "/api/v1/channel-accounts",
      "/api/v1/admin/environment-providers",
      "/api/v1/admin/environment-provider-bindings",
      "/api/auth/admin/list-users",
    ]) {
      expect((await call("GET", path)).status, path).toBe(200);
    }
  });

  it("updates a user's admin-managed account fields and accepts a password reset", async () => {
    const { store, call } = await boot();
    const target = [...store.users.values()].find((user) => user.id !== store.currentUser.id)!;

    const updated = await call("POST", "/api/auth/admin/update-user", {
      userId: target.id,
      data: {
        name: "Updated User",
        email: "UPDATED@EXAMPLE.COM",
        emailVerified: true,
        role: "admin",
      },
    });
    expect(updated.status).toBe(200);
    expect(store.users.get(target.id)).toMatchObject({
      name: "Updated User",
      email: "updated@example.com",
      emailVerified: true,
      role: "admin",
    });

    expect(
      (await call("POST", "/api/auth/admin/set-user-password", {
        userId: target.id,
        newPassword: "replacement-password",
      })).status,
    ).toBe(200);
    expect(
      (await call("POST", "/api/auth/admin/revoke-user-sessions", {
        userId: target.id,
      })).status,
    ).toBe(200);
  });

  it("returns a request-local MCP tool inventory", async () => {
    const { call } = await boot();
    const [universe] = (await call("GET", "/api/v1/universes")).json as Universe[];
    const servers = (await call(
      "GET",
      `/api/v1/universes/${universe!.id}/mcp-servers`,
    )).json as McpServer[];
    const server = servers.find((candidate) => candidate.status === "active");
    expect(server).toBeDefined();
    const discovered = await call(
      "POST",
      `/api/v1/universes/${universe!.id}/mcp-servers/${server!.serverId}/tools/discover`,
    );
    expect(discovered.status).toBe(200);
    const inventory = discovered.json as McpToolDiscovery;
    expect(inventory.status).toBe("success");
    if (inventory.status === "success") {
      expect(inventory.tools[0]).toMatchObject({ name: "search" });
    }
  });

  it("runs a message through the tail and completes it", async () => {
    const { call } = await boot();
    const [universe] = (await call("GET", "/api/v1/universes")).json as Universe[];
    const created = await call("POST", `/api/v1/universes/${universe!.id}/sessions`, {
      displayName: "smoke",
      profile: { kind: "inline", profile: {} },
    });
    expect(created.status).toBe(200);
    const sessionId = (created.json as { id: string }).id;
    const accepted = await call("POST", `/api/v1/universes/${universe!.id}/sessions/${sessionId}/messages`, {
      text: "hello from the smoke test",
      submissionId: "sub-1",
    });
    expect(accepted.status).toBe(200);
    const runId = (accepted.json as { run: { id: string; status: string } }).run.id;
    let after = 0;
    let completed = false;
    let acceptedSubmission: string | null | undefined;
    let userEntrySource: unknown = null;
    let userEntryOrigin: string | null | undefined;
    for (let i = 0; i < 20 && !completed; i++) {
      const page = (
        await call("GET", `/api/v1/universes/${universe!.id}/sessions/${sessionId}/events?after=${after}&limit=100&waitMs=3000`)
      ).json as SessionEventsPage;
      for (const event of page.events ?? []) {
        after = event.cursor.seq;
        if (event.kind.type === "runAccepted" && event.kind.runId === runId) {
          acceptedSubmission = event.kind.submissionId;
        }
        if (event.kind.type === "contextEntriesApplied") {
          for (const entry of event.kind.entries) {
            if (entry.kind.type === "message" && entry.kind.role === "user") {
              userEntrySource = entry.source ?? null;
              userEntryOrigin = entry.origin;
            }
          }
        }
        if (event.kind.type === "runCompleted" && event.kind.runId === runId) completed = true;
      }
    }
    expect(completed).toBe(true);
    // The page reconciles its optimistic bubble through these two joins:
    // `runAccepted.submissionId` maps the send to its run, and the user
    // entry's `source.runId` confirms the echo. Losing either shows a
    // double bubble in the demo.
    expect(acceptedSubmission).toBe("sub-1");
    expect(userEntrySource).toMatchObject({ type: "runInput", runId });
    expect(userEntryOrigin).toMatch(/^user:.+/);
    const view = (await call("GET", `/api/v1/universes/${universe!.id}/sessions/${sessionId}`)).json as {
      status: string;
      runs: Array<{ id: string; status: string }>;
    };
    expect(view.status).toBe("idle");
    expect(view.runs.find((run) => run.id === runId)?.status).toBe("completed");
  }, 30_000);

  it("copies profile metadata and accepts lightweight environment overrides", async () => {
    const { call } = await boot();
    const base = `/api/v1/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}`;
    const environments = (await call("GET", `${base}/environments`)).json as Environment[];
    const existing = environments.find((environment) => environment.status !== "closed");
    expect(existing).toBeDefined();

    const withoutEnvironment = await call("POST", `${base}/sessions`, {
      profile: { kind: "named", profileId: "implementer" },
      metadata: { campaign: "explicit-campaign" },
      environment: { type: "none" },
    });
    expect(withoutEnvironment.status).toBe(200);
    expect(withoutEnvironment.json as SessionView).toMatchObject({
      activeEnvironmentId: null,
      metadata: {
        agent: "lightspeed-software-factory-agent-with-existing-incus-environment",
        campaign: "explicit-campaign",
        profileRole: "parallel-task-implementation-and-pull-request-authoring",
      },
    });

    const withExisting = await call("POST", `${base}/sessions`, {
      profile: { kind: "named", profileId: "implementer" },
      environment: { type: "existing", environmentId: existing!.environmentId },
    });
    expect(withExisting.status).toBe(200);
    expect(withExisting.json as SessionView).toMatchObject({
      activeEnvironmentId: existing!.environmentId,
      metadata: { campaign: expect.stringContaining("terminal-bench-lightspeed") },
    });
  });

  it("session start, close, and deletion leave environments independently managed", async () => {
    const { call } = await boot();
    const base = `/api/v1/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}`;
    const before = (await call("GET", `${base}/environments`)).json as Environment[];
    const existing = before.find((environment) => environment.status === "ready")!;
    expect(existing).toBeDefined();
    const created = await call("POST", `${base}/sessions`, {
      profile: { kind: "named", profileId: "implementer" },
      environment: { type: "existing", environmentId: existing.environmentId },
    });
    expect(created.status).toBe(200);
    const session = created.json as SessionView;
    expect(((await call("GET", `${base}/environments`)).json as Environment[]).map((env) => env.environmentId))
      .toEqual(before.map((env) => env.environmentId));
    expect((await call("POST", `${base}/sessions/${session.id}/close`, { force: true })).status).toBe(200);
    expect((await call("DELETE", `${base}/sessions/${session.id}`)).status).toBe(200);
    const after = (await call("GET", `${base}/environments`)).json as Environment[];
    expect(after.find((env) => env.environmentId === existing.environmentId)).toEqual(existing);
  });

  it("sets and clears session-tree retention through the web routes", async () => {
    const { call } = await boot();
    const [universe] = (await call("GET", "/api/v1/universes")).json as Universe[];
    const created = await call("POST", `/api/v1/universes/${universe!.id}/sessions`, {
      displayName: "retained",
      deleteAfterCloseMs: 86_400_000,
      profile: { kind: "inline", profile: {} },
    });
    expect(created.status).toBe(200);
    const session = created.json as { id: string; retention: { rootSessionId: string; deleteAfterCloseMs: number } };
    expect(session.retention).toMatchObject({
      rootSessionId: session.id,
      deleteAfterCloseMs: 86_400_000,
    });

    const cleared = await call(
      "PUT",
      `/api/v1/universes/${universe!.id}/sessions/${session.id}/retention`,
      { deleteAfterCloseMs: null },
    );
    expect(cleared.status).toBe(200);
    expect((cleared.json as { retention: { deleteAfterCloseMs: null } }).retention.deleteAfterCloseMs).toBeNull();

    const inherited = await call("POST", `/api/v1/universes/${universe!.id}/sessions`, {
      profile: {
        kind: "inline",
        profile: { retention: { deleteAfterCloseMs: 172_800_000 } },
      },
    });
    expect((inherited.json as SessionView).retention.deleteAfterCloseMs).toBe(172_800_000);

    const overriddenToKeep = await call("POST", `/api/v1/universes/${universe!.id}/sessions`, {
      deleteAfterCloseMs: null,
      profile: {
        kind: "inline",
        profile: { retention: { deleteAfterCloseMs: 172_800_000 } },
      },
    });
    expect((overriddenToKeep.json as SessionView).retention.deleteAfterCloseMs).toBeNull();
  });

  it("admits a manual bot event and resolves its outcome", async () => {
    const { call } = await boot();
    const [universe] = (await call("GET", "/api/v1/universes")).json as Universe[];
    const [bot] = ((await call("GET", `/api/v1/universes/${universe!.id}/bots`)).json as { bots: BotListItem[] }).bots;
    const before = (await call("GET", `/api/v1/universes/${universe!.id}/bots/${bot!.botId}/events`)).json as {
      events: Array<{ seq: number }>;
    };
    const admitted = await call("POST", `/api/v1/universes/${universe!.id}/bots/${bot!.botId}/events`, {
      event: { kind: "smoke.test", summary: "smoke test event", data: { hello: "world" } },
    });
    expect(admitted.status).toBe(202);
    expect((admitted.json as { duplicate: boolean }).duplicate).toBe(false);
    await new Promise((resolve) => setTimeout(resolve, 6_000));
    const after = (await call("GET", `/api/v1/universes/${universe!.id}/bots/${bot!.botId}/events`)).json as {
      events: Array<{ seq: number; outcome: string | null }>;
    };
    expect(after.events.length).toBe(before.events.length + 1);
    const newest = after.events.reduce((a, b) => (a.seq > b.seq ? a : b));
    expect(newest.outcome).not.toBeNull();
  }, 20_000);

  it("names the missing stub instead of hanging", async () => {
    const { call } = await boot();
    const result = await call("GET", "/api/v1/nope");
    expect(result.status).toBe(404);
    expect((result.json as { error: string }).error).toMatch(/demo: no stub/);
  });
});
