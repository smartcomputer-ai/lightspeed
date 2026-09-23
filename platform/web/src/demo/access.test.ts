import { accessState } from "./access-state";
import { expect, it } from "vitest";
import { createDemoStore } from "./fixtures";
import { createDemoRouter } from "./router";
import { SOFTWARE_FACTORY_UNIVERSE_ID } from "./fixtures/software-factory";

it("creates a collection and session, shares the root, and prevents stale saves and nonempty deletion", async () => {
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
  const made = await call("POST", "/collections", {
    displayName: "Review",
    access: { visibility: "restricted" },
    execution: { kind: "personal" },
  });
  expect(made.status).toBe(201);
  const id = made.data.collection.collectionId;
  expect(made.data.collection.access.execution.kind).toBe("personal");
  const created = await call("POST", "/sessions", {
    displayName: "Review notes",
    profile: { kind: "inline", profile: {} },
    access: { root: { kind: "collection", id } },
  });
  expect(created.status).toBe(200);
  expect(created.data.access).toEqual(made.data.collection.access);
  const sessionId = created.data.id;
  const detail = await call("GET", `/collections/${id}`);
  expect(detail.data.members).toContainEqual({
    kind: "session",
    id: sessionId,
  });
  expect((await call("DELETE", `/collections/${id}`)).status).toBe(409);
  const read = await call("POST", "/access/policy/read", {
    resource: { kind: "session", id: sessionId },
  });
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
  expect(
    (
      await call("POST", "/sessions", {
        profile: { kind: "inline", profile: {} },
        access: { root: { kind: "collection", id } },
        execution: { kind: "service" },
      })
    ).status,
  ).toBe(400);
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

it("seeds standalone work without collections", () => {
  const store = createDemoStore();
  const universe = store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)!;
  expect(accessState(universe).collections.size).toBe(0);
  expect([...accessState(universe).roots.values()].some((root) => root.kind === "collection")).toBe(false);
  const personal = accessState(universe).policies.get("session:session-flaky-scheduler")!;
  expect(personal.root).toEqual({ kind: "session", id: "session-flaky-scheduler" });
  expect(personal.visibility).toBe("restricted");
  expect(personal.execution).toEqual({ kind: "personal", runAs: store.currentUser.id });
});
