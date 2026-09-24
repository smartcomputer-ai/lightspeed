import { accessState } from "./access-state";
import { expect, it } from "vitest";
import { createDemoStore } from "./fixtures";
import { createDemoRouter } from "./router";
import { SOFTWARE_FACTORY_UNIVERSE_ID } from "./fixtures/software-factory";

it("creates a personal session, shares its root, and prevents stale saves", async () => {
  const app = createDemoRouter(createDemoStore());
  const base = `/api/v1/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}`;
  const call = async (method: string, path: string, body?: unknown) => {
    const response = await app.request(base + path, {
      method,
      ...(body
        ? {
            headers: { "content-type": "application/json" },
            body: JSON.stringify(body),
          }
        : {}),
    });
    return { status: response.status, data: await response.json() };
  };
  const created = await call("POST", "/sessions", {
    displayName: "Review notes",
    profile: { kind: "inline", profile: {} },
    access: { visibility: "restricted" },
    execution: { kind: "personal" },
  });
  expect(created.status).toBe(200);
  const sessionId = created.data.id;
  expect(created.data.access.root).toEqual({ kind: "session", id: sessionId });
  expect(created.data.access.visibility).toBe("restricted");
  expect(created.data.access.execution.kind).toBe("personal");
  const read = await call("POST", "/access/policy/read", {
    resource: { kind: "session", id: sessionId },
  });
  expect(read.data.policy.root).toEqual({ kind: "session", id: sessionId });
  const update = {
    resource: read.data.policy.root,
    visibility: "universe",
    grants: [],
    expectedRevision: read.data.policy.revision,
  };
  expect((await call("PUT", "/access/policy", update)).status).toBe(200);
  expect((await call("PUT", "/access/policy", update)).status).toBe(409);
  expect(
    (await call("GET", `/sessions/${sessionId}`)).data.access.visibility,
  ).toBe("universe");
  // Personal work is bound to its owner and cannot be handed off.
  expect(
    (
      await call("PUT", "/access/policy", {
        ...update,
        expectedRevision: read.data.policy.revision + 1,
        owner: "00000000-0000-4000-8000-000000000002",
      })
    ).status,
  ).toBe(403);
});

it("exposes the bot's own policy and inherits it in its conversations", async () => {
  const app = createDemoRouter(createDemoStore());
  const base = `/api/v1/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}`;
  const bot = await (await app.request(`${base}/bots/implementer`)).json();
  expect(bot.access.root).toEqual({ kind: "bot", id: "implementer" });
  const session = await (
    await app.request(`${base}/sessions/bot:v1:implementer`)
  ).json();
  expect(session.access).toEqual(bot.access);
});

it("models privileged reads separately from ordinary reads and sharing", async () => {
  const store = createDemoStore();
  const universe = store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)!;
  const app = createDemoRouter(store);
  const base = `/api/v1/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}`;
  const policy = accessState(universe).policies.get(
    "session:session-flaky-scheduler",
  )!;
  policy.owner = "another-person";
  const member = universe.members.find(
    (m) => m.userId === store.currentUser.id,
  )!;
  const grant = (enabled: boolean) =>
    app.request(`${base}/members/${member.id}/private-content-access`, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ enabled }),
    });
  expect((await app.request(`${base}/sessions/session-flaky-scheduler`)).status).toBe(
    404,
  );
  expect((await grant(true)).status).toBe(200);
  const restricted = await app.request(`${base}/sessions/session-flaky-scheduler`);
  expect(restricted.status).toBe(200);
  expect(restricted.headers.get("x-lightspeed-privileged-read")).toBe("true");
  expect(
    (await app.request(`${base}/sessions`)).headers.get(
      "x-lightspeed-privileged-read",
    ),
  ).toBe("true");
  expect(
    (await app.request(`${base}/bots/implementer`)).headers.has(
      "x-lightspeed-privileged-read",
    ),
  ).toBe(false);
  const change = await app.request(`${base}/access/policy`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      resource: policy.resource,
      visibility: "universe",
      grants: [],
      expectedRevision: policy.revision,
    }),
  });
  expect(change.status).toBe(403);
  expect((await grant(false)).status).toBe(200);
  expect((await app.request(`${base}/sessions/session-flaky-scheduler`)).status).toBe(
    404,
  );
});

it("seeds every root as its own audience", () => {
  const store = createDemoStore();
  const universe = store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)!;
  for (const policy of accessState(universe).policies.values())
    expect(policy.root).toEqual(policy.resource);
  const personal = accessState(universe).policies.get("session:session-flaky-scheduler")!;
  expect(personal.root).toEqual({ kind: "session", id: "session-flaky-scheduler" });
  expect(personal.visibility).toBe("restricted");
  expect(personal.execution).toEqual({ kind: "personal", runAs: store.currentUser.id });
});

function demoApp() {
  const store = createDemoStore();
  const app = createDemoRouter(store);
  const base = `/api/v1/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}`;
  const call = async (method: string, path: string, body?: unknown) => {
    const response = await app.request(base + path, {
      method,
      ...(body
        ? {
            headers: { "content-type": "application/json" },
            body: JSON.stringify(body),
          }
        : {}),
    });
    return { status: response.status, data: await response.json() };
  };
  return { store, call, universe: store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)! };
}

it("summarizes access on every workspace, environment and MCP server in each universe", async () => {
  const store = createDemoStore();
  const app = createDemoRouter(store);
  for (const universe of store.universes.values()) {
    for (const [path, kind, field] of [
      ["workspaces", "workspace", "workspaceId"],
      ["environments", "environment", "environmentId"],
      ["mcp-servers", "mcp_server", "serverId"],
    ] as const) {
      const rows = (await (
        await app.request(`/api/v1/universes/${universe.universe.id}/${path}`)
      ).json()) as Record<string, unknown>[];
      for (const row of rows)
        expect(row.access, `${universe.universe.slug}: ${path}`).toMatchObject({
          root: { kind, id: row[field] },
          visibility: expect.any(String),
        });
      for (const row of rows)
        expect(row.access).not.toHaveProperty("execution");
    }
  }
});

it("seeds a restricted machine the default agent identity cannot use", async () => {
  const { call, store } = demoApp();
  const laptop = { kind: "environment", id: "env-priya-laptop" };
  const ci = { kind: "environment", id: "env-ci-runner" };
  const environments = (await call("GET", "/environments")).data as {
    environmentId: string;
    access: { visibility: string; owner: string };
  }[];
  expect(
    environments.find((e) => e.environmentId === laptop.id)?.access,
  ).toMatchObject({ visibility: "restricted", owner: "user-priya" });
  const policy = (await call("POST", "/access/policy/read", { resource: laptop }))
    .data.policy;
  expect(policy.grants).toMatchObject([
    { subject: { id: store.currentUser.id }, permission: "use" },
  ]);
  const mine = await call("POST", "/access", { resources: [laptop, ci] });
  expect(mine.data.resources[0].actions).toEqual(
    expect.arrayContaining(["read", "use_resource", "share_resource"]),
  );
  const service = await call("POST", "/access", {
    resources: [laptop, ci],
    as: "execution_service",
  });
  expect(service.data.actions).toEqual(mine.data.actions);
  expect(service.data.resources.map((r: { actions: string[] }) => r.actions)).toEqual([
    ["read"],
    ["read", "use_resource"],
  ]);
  const subjects = (await call("GET", "/access/subjects?q=default")).data.subjects;
  expect(subjects).toEqual([
    {
      subject: { kind: "principal", id: `execution-${SOFTWARE_FACTORY_UNIVERSE_ID}` },
      displayName: "Default agent identity",
    },
  ]);
});

it("creates restricted operational resources and shares them only with `use`", async () => {
  const { call } = demoApp();
  const workspace = await call("POST", "/workspaces", {
    workspaceId: "private-notes",
    access: { visibility: "restricted" },
  });
  expect(workspace.data.access).toMatchObject({
    root: { kind: "workspace", id: "private-notes" },
    visibility: "restricted",
  });
  const server = await call("POST", "/mcp-servers", {
    serverId: "internal",
    serverUrl: "https://mcp.internal.example/mcp",
    defaultServerLabel: "internal",
    access: { visibility: "restricted" },
  });
  expect(server.data.access.visibility).toBe("restricted");
  const resource = { kind: "workspace", id: "private-notes" };
  const { policy } = (await call("POST", "/access/policy/read", { resource })).data;
  const grant = (permission: string) => ({
    resource,
    visibility: "restricted",
    grants: [{ subject: { kind: "principal", id: "user-priya" }, permission }],
    expectedRevision: policy.revision,
  });
  expect((await call("PUT", "/access/policy", grant("read"))).status).toBe(400);
  expect((await call("PUT", "/access/policy", grant("use"))).status).toBe(200);
});

it("hides restricted operational resources from members without access", async () => {
  const { call, universe } = demoApp();
  await call("POST", "/workspaces", {
    workspaceId: "private-notes",
    access: { visibility: "restricted" },
  });
  accessState(universe).policies.get("workspace:private-notes")!.owner = "user-priya";
  universe.universe.role = "contributor";
  const rows = (await call("GET", "/workspaces")).data as { workspaceId: string }[];
  expect(rows.map((row) => row.workspaceId)).not.toContain("private-notes");
  expect((await call("GET", "/workspaces/private-notes/tree")).status).toBe(404);
  const environments = (await call("GET", "/environments")).data as { environmentId: string }[];
  // The contributor holds a `use` grant on Priya's laptop, so it stays listed.
  expect(environments.map((e) => e.environmentId)).toContain("env-priya-laptop");
});
