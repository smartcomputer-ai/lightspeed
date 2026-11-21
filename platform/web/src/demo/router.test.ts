import type { BotListItem } from "@lightspeed-ai/sdk";
import type { AuditPage } from "@lightspeed-ai/platform-shared";
import { defaultEnvironmentAttachment, environmentAttachments } from "@session-ui/model/resource-features";
import { describe, expect, it, vi } from "vitest";
import { createDemoStore } from "./fixtures";
import { createStandardDemoRouter } from "@distribution/demo";
import type {
  Universe,
  McpServer,
  McpToolDiscovery,
  Environment,
  EnvironmentRegistrationKey,
  SessionEventsPage,
  SessionListPage,
  SessionView,
  ModelDefaults,
} from "@/api";
import { SOFTWARE_FACTORY_UNIVERSE_ID } from "./fixtures/software-factory";
import { applyEvents, emptyTranscript } from "@session-ui/model/transcript";
import { sectionsByRun } from "@session-ui/model/run-sections";
import { appendScriptedRun, startRun } from "./engine";

/// Walks the demo router the way the UI does: every read path each page
/// opens with must answer, and the live paths (a message, its tail, a bot
/// event) must move state. A stub the UI needs but the router lacks fails
/// here before it fails in a browser.
async function boot() {
  const store = createDemoStore();
  const app = createStandardDemoRouter(store);
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
  "modules/sessions",
  "profiles",
  "workspaces",
  "environments",
  "environment-templates",
  "environment-providers",
  "environment-registration-keys",
  "mcp-servers",
  "secrets",
  "credential-grants",
  "models",
  "models/defaults",
  "modules/bots",
  "integrations/github",
  "integrations/subscriptions",
];

describe("demo router", () => {
  it("paginates audit events by time and id while new events arrive", async () => {
    const { store, call } = await boot();
    const id = (n: number) => `00000000-0000-4000-8000-${n.toString().padStart(12, "0")}`;
    const event = (n: number, createdAt = "2026-01-01T00:00:00.000Z") => ({
      id: id(n), createdAt, actorId: null, targetId: null, universeId: null,
      action: "test.event", outcome: "success", details: {},
    });
    store.auditEvents.push(event(1), event(3), event(2), event(4, "2025-12-31T00:00:00.000Z"));
    const first = (await call("GET", "/api/ui/admin/audit?limit=2")).json as AuditPage;
    expect(first.events.map(entry => entry.id)).toEqual([id(3), id(2)]);
    expect(first.nextCursor).toBe(id(2));
    store.auditEvents.push(event(5, "2026-01-02T00:00:00.000Z"));
    const second = (await call("GET", `/api/ui/admin/audit?limit=2&cursor=${first.nextCursor}`)).json as AuditPage;
    expect(second.events.map(entry => entry.id)).toEqual([id(1), id(4)]);
    expect(second.nextCursor).toBeNull();
    expect((await call("GET", `/api/ui/admin/audit?cursor=${id(4)}`)).json).toEqual({ events: [], nextCursor: null });
    for (const query of ["limit=0", "limit=101", "limit=-1", "limit=1.5", "limit=abc", "limit=", "cursor=bad", "cursor=", `cursor=${id(999)}`]) {
      expect((await call("GET", `/api/ui/admin/audit?${query}`)).status).toBe(400);
    }
    store.currentUser.role = "user";
    expect((await call("GET", "/api/ui/admin/audit?limit=2")).status).toBe(403);
  });

  it("lets the UI read, cancel, and steer a queued message with attachments", async () => {
    vi.useFakeTimers();
    try {
      const { store, call } = await boot();
      const universe = store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)!;
      const session = universe.sessions.get("session-flaky-scheduler")!;
      const active = startRun(store, universe, session, { text: "Work on this" });
      const base = `/api/ui/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}`;
      const upload = await call("POST", `${base}/attachments`, { bytesBase64: btoa("%PDF-1.7") });
      const blobRef = (upload.json as { blobs: { blobRef: string }[] }).blobs[0]!.blobRef;
      const attachment = { blobRef, mime: "application/pdf", kind: "document" as const, name: "notes.pdf" };
      const queued = startRun(store, universe, session, { text: "Use these notes", media: [attachment] });
      const path = `${base}/sessions/${session.view.id}`;
      const input = await call("GET", `${path}/runs/${queued.id}`);
      expect(input.status).toBe(200);
      expect(input.json).toMatchObject({ status: "queued", source: { items: [
        { type: "media", ...attachment }, { type: "text", text: "Use these notes" },
      ] } });
      expect((await call("POST", `${path}/runs/${queued.id}/cancel`, {})).json).toEqual({ run: { id: queued.id, status: "cancelled" } });
      expect((await call("POST", `${path}/runs/${active.id}/steer`, { text: "Use these notes", attachments: [attachment], submissionId: "local-steer" })).status).toBe(200);
      expect(session.queue.some((entry) => entry.runId === queued.id)).toBe(false);
      expect(session.events.find((event) => event.kind.type === "runSteeringAccepted" && event.kind.runId === active.id)?.joins.submissionId).toBe("local-steer");
      expect((await call("GET", `${path}/runs/missing`)).status).toBe(404);
      await vi.runAllTimersAsync();
      const rejected = await call("POST", `${path}/runs/${active.id}/steer`, { text: "Too late" });
      expect(rejected.status).toBe(409);
      expect(rejected.json).toMatchObject({ kind: "steering_not_submitted" });
    } finally {
      vi.clearAllTimers();
      vi.useRealTimers();
    }
  });

  it("starts demo universes with their own icons in the default color, while new universes use defaults", async () => {
    const { call } = await boot();
    const response = await call("GET", "/api/ui/universes");
    expect(response.status).toBe(200);
    const universes = response.json as Universe[];
    expect(universes).toHaveLength(3);
    expect(Object.fromEntries(universes.map(universe => [universe.name, [universe.icon, universe.iconColor]]))).toEqual({
      "Software Factory": ["mobius", "violet"],
      "Technical Support": ["support", "amber"],
      "Personal Assistant": ["personal-assistant", "blue"],
    });
    const created = await call("POST", "/api/ui/universes", { name: "New universe" });
    expect(created.status).toBe(201);
    expect(created.json).toMatchObject({ icon: "orbit", iconColor: "default" });
  });
  it("keeps bots on in the seeded universes while a new universe starts with bots off", async () => {
    const { store, call } = await boot();
    const seeded = (await call("GET", "/api/ui/universes")).json as Universe[];
    expect(seeded).toHaveLength(3);
    for (const universe of seeded) {
      expect(universe.features, universe.handle).toEqual({ bots: true });
    }
    const created = (await call("POST", "/api/ui/universes", { name: "New universe" })).json as Universe;
    expect(created.features).toEqual({ bots: false });
    // A switch is stored only while it differs from the default, as on the Platform.
    const path = `/api/ui/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}`;
    expect(store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)!.featureOverrides).toEqual({ bots: true });
    expect((await call("PATCH", path, { features: { bots: false } })).json).toMatchObject({ features: { bots: false } });
    expect(store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)!.featureOverrides).toEqual({});
    expect((await call("PATCH", path, { features: { bots: true } })).json).toMatchObject({ features: { bots: true } });
  });
  it("saves shared universe appearance and validates choices", async () => {
    const { call } = await boot();
    const path = `/api/ui/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}`;
    expect((await call("PATCH", path, { icon: "network", iconColor: "violet" })).status).toBe(200);
    expect((await call("GET", path)).json).toMatchObject({ icon: "network", iconColor: "violet" });
    expect((await call("GET", "/api/ui/universes")).json).toEqual(expect.arrayContaining([
      expect.objectContaining({ id: SOFTWARE_FACTORY_UNIVERSE_ID, icon: "network", iconColor: "violet" }),
    ]));
    expect((await call("PATCH", path, { icon: "security", iconColor: "invalid" })).status).toBe(400);
    expect((await call("PATCH", path, { icon: "unknown", iconColor: "blue" })).status).toBe(400);
    expect((await call("GET", path)).json).toMatchObject({ icon: "network", iconColor: "violet" });
  });
  it("keeps model defaults universe-scoped and revision-safe while preserving existing session models", async () => {
    const { store, call } = await boot();
    const base = `/api/ui/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}`;
    const other = [...store.universes.values()].find((state) => state.universe.id !== SOFTWARE_FACTORY_UNIVERSE_ID)!;
    const otherDefaults = structuredClone(other.modelDefaults);
    const original = (await call("GET", `${base}/models/defaults`)).json as ModelDefaults;
    const model = { providerId: "private", apiKind: "openai:responses", model: "manual-model" };
    const updated = await call("PUT", `${base}/models/defaults`, { slot: "agentRun", model, expectedRevision: original.revision });
    expect(updated.status).toBe(200);
    const revision = original.revision + 1;
    expect(updated.json).toEqual({ ...original, agentRun: model, revision });
    expect((await call("PUT", `${base}/models/defaults`, { slot: "agentRun", model: null, expectedRevision: original.revision })).status).toBe(409);
    expect((await call("PUT", `${base}/models/defaults`, { slot: "agentRun", expectedRevision: revision })).status).toBe(400);
    const created = await call("POST", `${base}/modules/sessions`, { profile: { kind: "inline", profile: {} } });
    expect(created.status).toBe(200);
    const session = created.json as SessionView;
    expect(session.config).toMatchObject({ model });
    const cleared = await call("PUT", `${base}/models/defaults`, { slot: "agentRun", model: null, expectedRevision: revision });
    expect(cleared.json).toMatchObject({ revision: revision + 1, agentRun: null });
    const missing = await call("POST", `${base}/modules/sessions`, { profile: { kind: "inline", profile: {} } });
    expect(missing.status).toBe(400);
    expect(missing.json).toMatchObject({ kind: "modelDefaultUnset", modelDefaultSlot: "agentRun" });
    const existing = await call("PUT", `${base}/sessions/${session.id}/config`, { config: {}, expectedConfigRevision: session.configRevision });
    expect(existing.status).toBe(200);
    expect(store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)!.sessions.get(session.id)!.view.config).toMatchObject({ model });
    const explicit = await call("POST", `${base}/modules/sessions`, { profile: { kind: "inline", profile: { config: { model } } } });
    expect(explicit.status).toBe(200);
    expect(other.modelDefaults).toEqual(otherDefaults);
  });

  it("uses universe defaults for demo bot sessions and refuses rotation when that choice is cleared", async () => {
    const { store, call } = await boot();
    const universe = store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)!;
    const base = `/api/ui/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}`;
    await call("PUT", `${base}/profiles/inherit`, { profileId: "inherit", config: {} });
    const created = await call("POST", `${base}/modules/bots`, { bot: { botId: "inherit-model", profileId: "inherit" } });
    expect(created.status).toBe(201);
    const record = universe.bots.get("inherit-model")!;
    const sessionId = record.state.mainSessionId!;
    const model = universe.modelDefaults.agentRun;
    expect(universe.sessions.get(sessionId)!.view.config).toMatchObject({ model });
    await call("PUT", `${base}/models/defaults`, { slot: "agentRun", model: null, expectedRevision: universe.modelDefaults.revision });
    const refused = await call("POST", `${base}/modules/bots/inherit-model/sessions/${sessionId}/rotate`);
    expect(refused.status).toBe(400);
    expect(refused.json).toMatchObject({ kind: "modelDefaultUnset" });
    expect(universe.sessions.get(sessionId)!.view.lifecycleStatus).not.toBe("closed");
    expect((await call("POST", `${base}/modules/bots`, { bot: { botId: "no-model", profileId: "inherit" } })).status).toBe(400);
    expect(universe.bots.has("no-model")).toBe(false);
  });

  it("carries runtime handles through creation and adoption without local suffixes", async () => {
    const { store, call } = await boot();
    const created = await call("POST", "/api/ui/universes", { name: "Display", handle: "chosen" });
    expect(created.status).toBe(201);
    expect(created.json).toMatchObject({ handle: "chosen" });
    expect((await call("POST", "/api/ui/universes", { name: "Another", handle: "chosen" })).status).toBe(409);
    const named = { universeId: "11111111-1111-4111-8111-111111111111", handle: "from-runtime", sessions: 0, workspaces: 0, profiles: 0, blobBytes: 0, createdAtMs: 0 };
    store.orphanEngineUniverses.push(named);
    const adoptionScope = { organizationId: "demo-organization", initialOwner: { kind: "user", id: store.currentUser.id } };
    const adopted = await call("POST", "/api/ui/universes/adopt", { ...adoptionScope, name: "Unrelated name", lightspeedUniverseId: named.universeId });
    expect(adopted.status).toBe(201);
    expect(adopted.json).toMatchObject({ handle: "from-runtime", lightspeedUniverseId: named.universeId });
    const other = { ...named, universeId: "22222222-2222-4222-8222-222222222222", handle: "kept" };
    store.orphanEngineUniverses.push(other);
    expect((await call("POST", "/api/ui/universes/adopt", { ...adoptionScope, name: "Other", lightspeedUniverseId: other.universeId, handle: "renamed-on-adopt" })).status).toBe(400);
    expect(store.orphanEngineUniverses.some(u => u.universeId === other.universeId)).toBe(true);
    const universe = created.json as Universe;
    expect((await call("PUT", `/api/ui/universes/${universe.id}/handle`, { handle: "from-runtime" })).status).toBe(409);
    const renamed = await call("PUT", `/api/ui/universes/${universe.id}/handle`, { handle: "renamed" });
    expect(renamed.status).toBe(200);
    expect(renamed.json).toMatchObject({ id: universe.id, lightspeedUniverseId: universe.lightspeedUniverseId, handle: "renamed" });
    expect((await call("GET", "/api/ui/universes")).json).toEqual(expect.arrayContaining([expect.objectContaining({ id: universe.id, handle: "renamed" })]));
  });

  it("adopts into the explicit organization with an Owner from that organization", async () => {
    const { store, call } = await boot();
    const created = await call("POST", "/api/v1/platform/organizations", { name: "Destination", handle: "destination" });
    const organizationId = (created.json as { organization: { id: string } }).organization.id;
    const owner = [...store.users.values()].find(user => user.id !== store.currentUser.id)!;
    const runtime = { universeId: crypto.randomUUID(), handle: "adopt-me", displayName: "Runtime name", protected: true, sessions: 0, workspaces: 0, profiles: 0, blobBytes: 0, createdAtMs: 0 };
    store.orphanEngineUniverses.push(runtime);
    const input = { organizationId, name: "Chosen name", lightspeedUniverseId: runtime.universeId, initialOwner: { kind: "user", id: owner.id } };
    expect((await call("POST", "/api/ui/universes/adopt", { ...input, organizationId: "missing" })).status).toBe(404);
    expect((await call("POST", "/api/ui/universes/adopt", input)).status).toBe(400);
    await call("PUT", `/api/v1/platform/organizations/${organizationId}/members/${owner.id}`, { role: "member" });
    const adopted = await call("POST", "/api/ui/universes/adopt", input);
    expect(adopted.status).toBe(201);
    expect(adopted.json).toMatchObject({ organizationId, name: "Chosen name", handle: "adopt-me", protected: true, lightspeedUniverseId: runtime.universeId });
    expect(store.orphanEngineUniverses.some(universe => universe.universeId === runtime.universeId)).toBe(false);
    const grants = await call("GET", `/api/v1/platform/universes/${runtime.universeId}/grants`);
    expect(grants.json).toEqual({ grants: [expect.objectContaining({ principal: { kind: "user", id: owner.id }, role: "owner" })] });
    expect((await call("GET", `/api/ui/universes/reconcile?organizationId=${organizationId}`)).json).toMatchObject({ platform: [expect.objectContaining({ lightspeedUniverseId: runtime.universeId })], orphans: [] });
  });

  it("keeps seeded final replies outside each run's collapsible activity", async () => {
    const { store } = await boot();
    let checked = 0;
    for (const universe of store.universes.values()) {
      for (const session of universe.sessions.values()) {
        const transcript = applyEvents(emptyTranscript(), session.events);
        const sections = sectionsByRun(transcript.entries, null).filter((section) => section.kind === "run");
        for (const run of session.runs.values()) {
          if (run.status !== "completed" || !run.outputText) continue;
          const section = sections.find((section) => section.runId === run.id);
          expect(section?.reply, `${universe.universe.handle}/${session.view.id}/${run.id}`).toMatchObject({
            role: "assistant",
            runId: run.id,
            contentRef: run.output?.blobRef,
            text: run.outputText,
          });
          expect(section?.work).not.toContain(section?.reply);
          checked += 1;
        }
      }
    }
    expect(checked).toBeGreaterThan(0);
  });

  it("shares an unshared root session once, with its sub-agents", async () => {
    const { store, call } = await boot();
    const universe = store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)!;
    const path = `/api/ui/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}/sessions/session-flaky-scheduler`;
    expect((await call("GET", path)).json).toMatchObject({ access: { visibility: "restricted" } });
    const listed = (await call("GET", `/api/ui/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}/modules/sessions?limit=200`)).json as { sessions: { id: string; access: { visibility: string } }[] };
    expect(listed.sessions.find((session) => session.id === "session-flaky-scheduler")?.access.visibility).toBe("restricted");
    const shared = await call("POST", `${path}/share`);
    expect(shared.status).toBe(200);
    expect(shared.json).toMatchObject({ access: { visibility: "universe" } });
    expect(universe.sessions.get("session-flaky-scheduler")!.view.access.visibility).toBe("universe");
    expect((await call("POST", `${path}/share`)).status).toBe(409);
  });

  it("lists each session with its latest message as one plain, bounded line", async () => {
    const { store, call } = await boot();
    const universe = store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)!;
    const session = universe.sessions.get("session-flaky-scheduler")!;
    const list = async () => {
      const listed = (await call("GET", `/api/ui/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}/modules/sessions?limit=200`)).json as {
        sessions: { id: string; lastMessagePreview?: string }[];
      };
      return new Map(listed.sessions.map((entry) => [entry.id, entry.lastMessagePreview]));
    };

    appendScriptedRun(store, session, {
      at: Date.now(),
      user: "Is the scheduler fixed?",
      steps: [{ text: "## Done\n\nThe **scheduler** suite passes:\n\n```ts\nawait run();\n```\n\nSee [PR #487](https://example.test/487) for `retry_policy`." }],
    });
    // The agent's reply is the latest message: Markdown reduced to its words, code dropped.
    expect((await list()).get(session.view.id)).toBe("Done The scheduler suite passes: See PR #487 for retry_policy.");

    appendScriptedRun(store, session, { at: Date.now(), user: "Thanks!", steps: [{ text: "x".repeat(500) }] });
    const long = (await list()).get(session.view.id)!;
    expect(long).toHaveLength(200);
    expect(long.endsWith("…")).toBe(true);

    // Every seeded session with messages has one; none spans lines.
    for (const preview of (await list()).values()) {
      if (preview !== undefined) expect(preview).not.toMatch(/\n|```/);
    }
  });

  it("uploads an attachment and runs it through the transcript as input media", async () => {
    const { store, call } = await boot();
    const base = `/api/ui/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}`;
    const created = (await call("POST", `${base}/modules/sessions`, { profile: { kind: "inline", profile: {} } })).json as SessionView;
    const uploaded = await call("POST", `${base}/attachments`, { bytesBase64: btoa("%PDF-1.7") });
    expect(uploaded.status).toBe(200);
    const blobRef = (uploaded.json as { blobs: { blobRef: string }[] }).blobs[0]!.blobRef;
    const attachment = { blobRef, mime: "application/pdf", kind: "document", name: "offer.pdf" };
    const path = `${base}/sessions/${created.id}`;
    expect((await call("POST", `${path}/messages`, { submissionId: "missing", attachments: [{ ...attachment, blobRef: `sha256:${"0".repeat(64)}` }] })).status).toBe(400);
    const pinned = created.config!.model as { providerId: string; apiKind: string };
    expect((await call("POST", `${path}/messages`, {
      text: "hi", submissionId: "wrong-route", options: { model: { providerId: "elsewhere", apiKind: pinned.apiKind, model: "x" } },
    })).status).toBe(400);
    const accepted = await call("POST", `${path}/messages`, { text: "Summarize this", submissionId: "with-file", attachments: [attachment] });
    expect(accepted.status).toBe(200);
    const session = store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)!.sessions.get(created.id)!;
    const transcript = applyEvents(emptyTranscript(), session.events);
    expect(transcript.entries[0]).toMatchObject({
      kind: "message",
      role: "user",
      text: "Summarize this",
      media: [{ blobRef, mime: "application/pdf", kind: "document", name: "offer.pdf" }],
    });
  });

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

  it("signs the visitor in as a platform admin without a login", async () => {
    const { call } = await boot();
    const session = await call("GET", "/api/auth/get-session");
    expect(session.status).toBe(200);
    expect((session.json as { user: { role: string } }).user.role).toBe("admin");
  });

  it("answers every read the universe pages open with, for every universe", async () => {
    const { call } = await boot();
    const universes = await call("GET", "/api/ui/universes");
    expect(universes.status).toBe(200);
    const list = universes.json as Universe[];
    expect(list.length).toBeGreaterThanOrEqual(2);
    for (const universe of list) {
      for (const path of universeReads) {
        const result = await call("GET", `/api/ui/universes/${universe.id}/${path}`);
        expect(result.status, `${universe.handle}: ${path}`).toBe(200);
      }
      const bots = ((await call("GET", `/api/ui/universes/${universe.id}/modules/bots`)).json as { bots: BotListItem[] }).bots;
      expect(bots.length, `${universe.handle}: bots`).toBeGreaterThan(0);
      for (const bot of bots) {
        for (const path of ["", "/triggers", "/events", "/state"]) {
          const result = await call("GET", `/api/ui/universes/${universe.id}/modules/bots/${bot.botId}${path}`);
          expect(result.status, `${universe.handle}: bots/${bot.botId}${path}`).toBe(200);
        }
      }
      const sessions = (await call("GET", `/api/ui/universes/${universe.id}/modules/sessions`)).json as SessionListPage;
      expect(sessions.sessions.length, `${universe.handle}: sessions`).toBeGreaterThan(0);
      for (const session of sessions.sessions) {
        for (const path of ["", "/events?limit=200", "/events?direction=backward&limit=200", "/instructions"]) {
          const result = await call("GET", `/api/ui/universes/${universe.id}/sessions/${session.id}${path}`);
          expect(result.status, `${universe.handle}: sessions/${session.id}${path}`).toBe(200);
        }
      }
    }
  });

  it("pages transcript history backward with stable bounds while newer runs arrive", async () => {
    const { store, call } = await boot();
    const universe = store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)!;
    const session = universe.sessions.get("session-flaky-scheduler")!;
    const path = `/api/ui/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}/sessions/${session.view.id}/events?direction=backward`;
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
      `/api/ui/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}/sessions/session-flaky-scheduler/events?limit=200`,
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
      `/api/ui/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}/modules/sessions?metadata=${encodeURIComponent(`campaign=${campaign}`)}`,
    );
    expect(result.status).toBe(200);
    const sessions = (result.json as SessionListPage).sessions;
    expect(sessions.length).toBeGreaterThanOrEqual(2);
    expect(sessions.every((session) => session.metadata?.campaign === campaign)).toBe(true);
    expect(sessions.some((session) => (session.metadata?.workflowRunId?.length ?? 0) > 60)).toBe(true);
  });

  it("deletes an active universe only with its exact handle and records the request", async () => {
    const { store, call } = await boot();
    const state = store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)!;
    const path = `/api/ui/universes/${state.universe.id}`;
    expect(state.universe.status).toBe("active");
    expect((await call("DELETE", path, { handle: "wrong" })).status).toBe(400);
    expect((await call("DELETE", path, { handle: state.universe.handle })).status).toBe(200);
    expect(store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)).toBeNull();
    expect(store.auditEvents.at(-1)).toMatchObject({ action: "universe.delete", targetId: state.universe.lightspeedUniverseId });
  });

  it("answers the admin pages", async () => {
    const { call } = await boot();
    for (const path of [
      "/api/ui/users",
      "/api/ui/universes/reconcile",
      "/api/ui/admin/environment-providers",
      "/api/ui/admin/environment-provider-bindings",
      "/api/auth/admin/list-users",
    ]) {
      expect((await call("GET", path)).status, path).toBe(200);
    }
  });

  it("scopes administrative reconciliation to one organization while keeping unclaimed names", async () => {
    const { store, call } = await boot();
    const selected = store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)!;
    selected.universe.organizationId = "selected";
    const unclaimed = { universeId: crypto.randomUUID(), handle: "unclaimed", displayName: "Runtime name", sessions: 0, workspaces: 0, profiles: 0, blobBytes: 0, createdAtMs: 0 };
    store.orphanEngineUniverses.push(unclaimed);
    const scoped = await call("GET", "/api/ui/universes/reconcile?organizationId=selected");
    expect(scoped.status).toBe(200);
    expect(scoped.json).toEqual({ platform: [{ id: selected.universe.id, organizationId: "selected", lightspeedUniverseId: selected.universe.lightspeedUniverseId, engine: "ok" }], orphans: [] });
    expect((await call("GET", "/api/ui/universes/reconcile")).json).toMatchObject({ orphans: expect.arrayContaining([unclaimed]) });
    store.currentUser.role = "user";
    expect((await call("GET", "/api/ui/universes/reconcile?organizationId=selected")).status).toBe(403);
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
    const [universe] = (await call("GET", "/api/ui/universes")).json as Universe[];
    const servers = (await call(
      "GET",
      `/api/ui/universes/${universe!.id}/mcp-servers`,
    )).json as McpServer[];
    const server = servers.find((candidate) => candidate.status === "active");
    expect(server).toBeDefined();
    const discovered = await call(
      "POST",
      `/api/ui/universes/${universe!.id}/mcp-servers/${server!.serverId}/tools/discover`,
    );
    expect(discovered.status).toBe(200);
    const inventory = discovered.json as McpToolDiscovery;
    expect(inventory.status).toBe("success");
    if (inventory.status === "success") {
      expect(inventory.tools[0]).toMatchObject({ name: "search" });
    }
  });

  it("runs a message through the tail and completes it", async ({ onTestFinished }) => {
    // The engine paces its steps with timers; run them out instead of
    // waiting on the wall clock.
    vi.useFakeTimers();
    onTestFinished(() => { vi.clearAllTimers(); vi.useRealTimers(); });
    const { call } = await boot();
    const [universe] = (await call("GET", "/api/ui/universes")).json as Universe[];
    const created = await call("POST", `/api/ui/universes/${universe!.id}/modules/sessions`, {
      displayName: "smoke",
      profile: { kind: "inline", profile: {} },
    });
    expect(created.status).toBe(200);
    const sessionId = (created.json as { id: string }).id;
    const accepted = await call("POST", `/api/ui/universes/${universe!.id}/sessions/${sessionId}/messages`, {
      text: "hello from the smoke test",
      submissionId: "sub-1",
    });
    expect(accepted.status).toBe(200);
    const runId = (accepted.json as { run: { id: string; status: string } }).run.id;
    await vi.runAllTimersAsync();
    let after = 0;
    let completed = false;
    let acceptedSubmission: string | null | undefined;
    let userEntrySource: unknown = null;
    let userEntryOrigin: string | null | undefined;
    let transcript = emptyTranscript();
    for (let i = 0; i < 20 && !completed; i++) {
      const page = (
        await call("GET", `/api/ui/universes/${universe!.id}/sessions/${sessionId}/events?after=${after}&limit=100`)
      ).json as SessionEventsPage;
      if (!page.events?.length) break;
      transcript = applyEvents(transcript, page.events ?? []);
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
    const view = (await call("GET", `/api/ui/universes/${universe!.id}/sessions/${sessionId}`)).json as {
      lifecycleStatus: string;
      activity: string;
      runs: Array<{ id: string; status: string }>;
    };
    expect(view.lifecycleStatus).toBe("open");
    expect(view.activity).toBe("idle");
    expect(view.runs.find((run) => run.id === runId)?.status).toBe("completed");
    const section = sectionsByRun(transcript.entries, null)
      .filter((section) => section.kind === "run")
      .find((section) => section.runId === runId);
    expect(section?.summary?.outputContentRef).toBeTruthy();
    expect(section?.reply).toMatchObject({
      role: "assistant", runId, contentRef: section?.summary?.outputContentRef,
    });
    expect(section?.work).not.toContain(section?.reply);
  });

  it("copies profile metadata and selects only its default environment attachment", async () => {
    const { call } = await boot();
    const base = `/api/ui/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}`;
    const profile = (await call("GET", `${base}/profiles/implementer`)).json as { config: Record<string, unknown> };
    const defaultId = defaultEnvironmentAttachment(profile.config)?.environmentId;
    expect(defaultId).toBeDefined();

    const created = await call("POST", `${base}/modules/sessions`, {
      profile: { kind: "named", profileId: "implementer" },
      metadata: { campaign: "explicit-campaign" },
    });
    expect(created.status).toBe(200);
    expect(created.json as SessionView).toMatchObject({
      activeEnvironmentId: defaultId,
      metadata: {
        agent: "lightspeed-software-factory-agent-with-existing-incus-environment",
        campaign: "explicit-campaign",
        profileRole: "parallel-task-implementation-and-pull-request-authoring",
      },
    });

    const config = structuredClone(profile.config);
    const attachments = environmentAttachments(config);
    for (const attachment of attachments) delete attachment.default;
    const withoutDefault = await call("POST", `${base}/modules/sessions`, {
      profile: { kind: "inline", profile: { config } },
    });
    expect(withoutDefault.status).toBe(200);
    expect((withoutDefault.json as SessionView).activeEnvironmentId).toBeNull();
  });

  it.each([{ type: "none" }, { type: "existing", environmentId: "runner" }, null])(
    "rejects a removed creation-time environment field: %j", async (environment) => {
      const { call } = await boot();
      const base = `/api/ui/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}`;
      const response = await call("POST", `${base}/modules/sessions`, {
        profile: { kind: "named", profileId: "implementer" }, environment,
      });
      expect(response.status).toBe(400);
    },
  );

  it("session start, close, and deletion leave environments independently managed", async () => {
    const { call, store } = await boot();
    const base = `/api/ui/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}`;
    const before = ((await call("GET", `${base}/environments`)).json as { environments: Environment[] }).environments;
    const profile = (await call("GET", `${base}/profiles/implementer`)).json as { config: unknown };
    const existing = before.find((environment) => environment.environmentId === defaultEnvironmentAttachment(profile.config)?.environmentId)!;
    expect(existing).toBeDefined();
    const created = await call("POST", `${base}/modules/sessions`, {
      profile: { kind: "named", profileId: "implementer" },
    });
    expect(created.status).toBe(200);
    const session = created.json as SessionView;
    expect((((await call("GET", `${base}/environments`)).json as { environments: Environment[] }).environments).map((env) => env.environmentId))
      .toEqual(before.map((env) => env.environmentId));
    expect((await call("POST", `${base}/sessions/${session.id}/close`, { force: true })).status).toBe(200);
    expect(store.auditEvents.filter((event) => event.targetId === session.id)).toEqual([]);
    expect((await call("DELETE", `${base}/sessions/${session.id}`)).status).toBe(200);
    expect(store.auditEvents.filter((event) => event.targetId === session.id)).toEqual([]);
    expect((await call("GET", `${base}/sessions/${session.id}`)).status).toBe(404);
    const adminBase = `/api/ui/admin/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}`;
    const deleted = (await call("GET", `${adminBase}/deleted-sessions`)).json as { sessions: { sessionId: string }[] };
    expect(deleted.sessions.map((entry) => entry.sessionId)).toContain(session.id);
    expect(store.deletedSessions.size).toBe(1);
    expect((await call("POST", `${adminBase}/sessions/${session.id}/purge`, {})).status).toBe(200);
    expect(store.deletedSessions.size).toBe(0);
    const { events: audit } = (await call("GET", "/api/ui/admin/audit")).json as { events: { action: string; targetId: string }[] };
    expect(audit.filter((event) => event.targetId === session.id).map((event) => event.action)).toEqual(["session.purge"]);
    const after = ((await call("GET", `${base}/environments`)).json as { environments: Environment[] }).environments;
    expect(after.find((env) => env.environmentId === existing.environmentId)).toEqual(existing);
  });

  it("mints a registration key under a chosen id and refuses an id a key already holds", async () => {
    const { call } = await boot();
    const keys = `/api/ui/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}/environment-registration-keys`;
    const body = { identityMode: "ephemeral", displayName: "Sandboxes", registrationKeyId: " sandboxes " };
    const created = await call("POST", keys, body);
    expect(created.status).toBe(201);
    expect(created.json).toMatchObject({ registrationKey: { registrationKeyId: "sandboxes", status: "active" } });
    const taken = await call("POST", keys, body);
    expect(taken.status).toBe(409);
    expect(taken.json).toEqual({
      error: 'key ID "sandboxes" is already in use; leave it blank to generate a new ID or choose another',
    });
    // Revoking never frees the id.
    expect((await call("POST", `${keys}/sandboxes/revoke`, {})).status).toBe(200);
    expect((await call("POST", keys, body)).json).toEqual({
      error: 'key ID "sandboxes" belongs to a revoked key and cannot be reused; leave it blank to generate a new ID or choose another',
    });
    expect(((await call("GET", keys)).json as EnvironmentRegistrationKey[])
      .filter((key) => key.registrationKeyId === "sandboxes")).toHaveLength(1);
  });

  it("declares registered environments under persistent keys and releases their daemons", async () => {
    const { call, store } = await boot();
    const base = `/api/ui/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}`;
    const keys = `${base}/environment-registration-keys`;
    const mint = async (body: Record<string, unknown>) =>
      ((await call("POST", keys, body)).json as { registrationKey: EnvironmentRegistrationKey }).registrationKey;
    expect((await call("POST", keys, { identityMode: "ephemeral", displayName: "Sandboxes", declaredOnly: true })).status).toBe(400);
    const ephemeral = await mint({ identityMode: "ephemeral", displayName: "Sandboxes" });
    const key = await mint({ identityMode: "persistent", displayName: "Build machines", declaredOnly: true });
    expect(key.declaredOnly).toBe(true);
    // An update is the complete policy: leaving the field out turns it off.
    const renamed = (await call("PUT", `${keys}/${key.registrationKeyId}`, { displayName: "Build fleet", declaredOnly: true })).json;
    expect(renamed).toMatchObject({ displayName: "Build fleet", declaredOnly: true });
    const cleared = (await call("PUT", `${keys}/${key.registrationKeyId}`, { displayName: "Build fleet" })).json as EnvironmentRegistrationKey;
    expect(cleared.declaredOnly).toBeUndefined();

    const declaration = { environmentId: "build-box-01", registrationKeyId: key.registrationKeyId, name: "build-01" };
    expect((await call("POST", `${base}/environments/registered`, { ...declaration, registrationKeyId: ephemeral.registrationKeyId })).status).toBe(400);
    expect((await call("POST", `${base}/environments/registered`, { ...declaration, name: " " })).status).toBe(400);
    const declared = await call("POST", `${base}/environments/registered`, declaration);
    expect(declared.status).toBe(201);
    expect(declared.json).toMatchObject({
      environmentId: "build-box-01", state: "open", status: "away",
      source: { type: "registered", registrationKeyId: key.registrationKeyId, name: "build-01", identityMode: "persistent", connected: false },
    });
    expect((declared.json as Environment).source).not.toHaveProperty("daemonId");
    const listed = ((await call("GET", `${base}/environments`)).json as { environments: Environment[] }).environments;
    expect(listed.map((environment) => environment.environmentId)).toContain("build-box-01");
    expect(((await call("GET", keys)).json as EnvironmentRegistrationKey[])
      .find((candidate) => candidate.registrationKeyId === key.registrationKeyId)).toMatchObject({ activeEnvironmentCount: 1 });

    // Declaring again converges the display name; the key and name are fixed.
    const again = await call("POST", `${base}/environments/registered`, { ...declaration, displayName: "Build box 01" });
    expect(again.status).toBe(200);
    expect(again.json).toMatchObject({ environmentId: "build-box-01", displayName: "Build box 01" });
    expect((await call("POST", `${base}/environments/registered`, { ...declaration, name: "build-02" })).status).toBe(409);
    expect((await call("POST", `${base}/environments/registered`, { ...declaration, environmentId: "build-box-02" })).status).toBe(409);

    // Releasing forgets the bound daemon and keeps the environment.
    const release = `${base}/environments/build-box-01/release`;
    expect((await call("POST", release)).json).toEqual(again.json);
    const source = store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)!.environments.get("build-box-01")!.source;
    if (source.type !== "registered") throw new Error("expected a registered environment");
    source.daemonId = "daemon_bound";
    source.connected = true;
    const released = await call("POST", release);
    expect(released.status).toBe(200);
    expect(released.json).toMatchObject({ environmentId: "build-box-01", status: "away", source: { name: "build-01", connected: false } });
    expect((released.json as Environment).source).not.toHaveProperty("daemonId");
    const undeclared = listed.find((environment) => environment.source.type !== "registered")!;
    expect((await call("POST", `${base}/environments/${undeclared.environmentId}/release`)).status).toBe(400);
  });

  it("rejects unlisted activation and clears removed selections without filling defaults", async () => {
    const { call } = await boot();
    const base = `/api/ui/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}`;
    const profile = (await call("GET", `${base}/profiles/implementer`)).json as { config: Record<string, unknown> };
    const defaultId = defaultEnvironmentAttachment(profile.config)!.environmentId!;
    const environments = ((await call("GET", `${base}/environments`)).json as { environments: Environment[] }).environments;
    const other = environments.find((environment) => environment.environmentId !== defaultId && environment.status === "ready")!;
    expect(other).toBeDefined();
    const created = (await call("POST", `${base}/modules/sessions`, { profile: { kind: "named", profileId: "implementer" } })).json as SessionView;
    expect(created.activeEnvironmentId).toBe(defaultId);
    expect((await call("POST", `${base}/sessions/${created.id}/environments/${other.environmentId}/activate`, {})).status).toBe(409);
    const config = { ...profile.config, features: { environments: { environments: [{ environmentId: other.environmentId, access: "read", default: true }] } } };
    const changed = await call("PUT", `${base}/sessions/${created.id}/config`, { config, expectedConfigRevision: created.configRevision });
    expect(changed.status).toBe(200);
    const session = (await call("GET", `${base}/sessions/${created.id}`)).json as SessionView;
    expect(session.activeEnvironmentId).toBeNull();
    expect((await call("POST", `${base}/sessions/${created.id}/environments/${other.environmentId}/activate`, {})).status).toBe(200);
  });

  it("sets and clears session-tree retention through the web routes", async () => {
    const { call } = await boot();
    const [universe] = (await call("GET", "/api/ui/universes")).json as Universe[];
    const created = await call("POST", `/api/ui/universes/${universe!.id}/modules/sessions`, {
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
      `/api/ui/universes/${universe!.id}/sessions/${session.id}/retention`,
      { deleteAfterCloseMs: null },
    );
    expect(cleared.status).toBe(200);
    expect((cleared.json as { retention: { deleteAfterCloseMs: null } }).retention.deleteAfterCloseMs).toBeNull();

    const inherited = await call("POST", `/api/ui/universes/${universe!.id}/modules/sessions`, {
      profile: {
        kind: "inline",
        profile: { retention: { deleteAfterCloseMs: 172_800_000 } },
      },
    });
    expect((inherited.json as SessionView).retention.deleteAfterCloseMs).toBe(172_800_000);

    const overriddenToKeep = await call("POST", `/api/ui/universes/${universe!.id}/modules/sessions`, {
      deleteAfterCloseMs: null,
      profile: {
        kind: "inline",
        profile: { retention: { deleteAfterCloseMs: 172_800_000 } },
      },
    });
    expect((overriddenToKeep.json as SessionView).retention.deleteAfterCloseMs).toBeNull();
  });

  it("admits a manual bot event and resolves its outcome", async ({ onTestFinished }) => {
    vi.useFakeTimers();
    onTestFinished(() => {
      vi.clearAllTimers();
      vi.useRealTimers();
    });
    const { call } = await boot();
    const [universe] = (await call("GET", "/api/ui/universes")).json as Universe[];
    const [bot] = ((await call("GET", `/api/ui/universes/${universe!.id}/modules/bots`)).json as { bots: BotListItem[] }).bots;
    const before = (await call("GET", `/api/ui/universes/${universe!.id}/modules/bots/${bot!.botId}/events`)).json as {
      events: Array<{ seq: number }>;
    };
    const admitted = await call("POST", `/api/ui/universes/${universe!.id}/modules/bots/${bot!.botId}/events`, {
      event: { kind: "smoke.test", summary: "smoke test event", data: { hello: "world" } },
    });
    expect(admitted.status).toBe(202);
    expect((admitted.json as { duplicate: boolean }).duplicate).toBe(false);
    await vi.runAllTimersAsync();
    const after = (await call("GET", `/api/ui/universes/${universe!.id}/modules/bots/${bot!.botId}/events`)).json as {
      events: Array<{ seq: number; outcome: string | null }>;
    };
    expect(after.events.length).toBe(before.events.length + 1);
    const newest = after.events.reduce((a, b) => (a.seq > b.seq ? a : b));
    expect(newest.outcome).not.toBeNull();
  });

  it("keeps ingest paths out of trigger reads and serves them from their own route", async () => {
    const { store, call } = await boot();
    type Trigger = { triggerId: string; kind: string; revision: number };
    type Secrets = { ingestPath?: string };
    let webhooks = 0;
    for (const universe of (await call("GET", "/api/ui/universes")).json as Universe[]) {
      const bots = ((await call("GET", `/api/ui/universes/${universe.id}/modules/bots`)).json as { bots: BotListItem[] }).bots;
      for (const bot of bots) {
        const base = `/api/ui/universes/${universe.id}/modules/bots/${bot.botId}/triggers`;
        const listed = await call("GET", base);
        const text = JSON.stringify(listed.json);
        expect(text, `${bot.botId}: list`).not.toMatch(/ingestPath|\/ingress\/webhooks\//);
        for (const trigger of (listed.json as { triggers: Trigger[] }).triggers) {
          const read = await call("GET", `${base}/${trigger.triggerId}`);
          expect(JSON.stringify(read.json), `${bot.botId}/${trigger.triggerId}: read`).not.toMatch(
            /ingestPath|\/ingress\/webhooks\//,
          );
          const secrets = await call("GET", `${base}/${trigger.triggerId}/secrets`);
          expect(secrets.status, `${bot.botId}/${trigger.triggerId}: secrets`).toBe(200);
          const { ingestPath } = secrets.json as Secrets;
          const stored = store.universe(universe.id)!.bots.get(bot.botId)!.triggers.get(trigger.triggerId)!;
          if (trigger.kind === "webhook") {
            webhooks += 1;
            expect(ingestPath).toBe(stored.ingestPath);
            expect(ingestPath).toMatch(/^\/ingress\/webhooks\/bots\//);
          } else {
            expect(ingestPath).toBeUndefined();
          }
        }
      }
    }
    expect(webhooks).toBeGreaterThan(0);
    expect((await call("GET", `/api/ui/universes/${SOFTWARE_FACTORY_UNIVERSE_ID}/modules/bots/no-such-bot/triggers/x/secrets`)).status).toBe(404);
  });

  it("answers a trigger write with its secrets and keeps the URL token across edits", async ({ onTestFinished }) => {
    vi.useFakeTimers();
    onTestFinished(() => {
      vi.clearAllTimers();
      vi.useRealTimers();
    });
    const { call } = await boot();
    type Written = { trigger: Record<string, unknown> & { revision: number }; secrets?: { ingestPath?: string } };
    const [universe] = (await call("GET", "/api/ui/universes")).json as Universe[];
    const bot = ((await call("GET", `/api/ui/universes/${universe!.id}/modules/bots`)).json as { bots: BotListItem[] }).bots
      .find((entry) => entry.enabled !== false && entry.closedAtMs == null)!;
    const base = `/api/ui/universes/${universe!.id}/modules/bots/${bot.botId}/triggers`;
    const created = await call("PUT", `${base}/secrets-hook`, { trigger: { kind: "webhook" } });
    expect(created.status).toBe(200);
    const first = created.json as Written;
    expect(JSON.stringify(first.trigger)).not.toMatch(/ingestPath|\/ingress\/webhooks\//);
    expect(first.secrets?.ingestPath).toMatch(/^\/ingress\/webhooks\/bots\/.+\/secrets-hook\/[0-9a-f]{48}$/);
    // An edit keeps the URL token, and the answer still carries the path.
    const edited = (await call("PUT", `${base}/secrets-hook`, {
      trigger: { kind: "webhook", filter: "event.kind == 'x'" },
      expectedRevision: first.trigger.revision,
    })).json as Written;
    expect(edited.secrets?.ingestPath).toBe(first.secrets?.ingestPath);
    expect(((await call("GET", `${base}/secrets-hook/secrets`)).json as { ingestPath: string }).ingestPath).toBe(
      first.secrets?.ingestPath,
    );
    // The path it answered with is the one the public route accepts.
    expect((await call("POST", first.secrets!.ingestPath!, { kind: "sample" })).status).toBe(202);
    const removedPath = first.secrets!.ingestPath!.replace("/ingress/webhooks/", "/hooks/");
    expect((await call("POST", removedPath, { kind: "sample" })).status).toBe(404);
    await vi.runAllTimersAsync();
    // A schedule has no secret to answer with.
    const schedule = (await call("PUT", `${base}/secrets-nightly`, {
      trigger: { kind: "schedule", cron: "0 3 * * *", summary: "Nightly" },
    })).json as Written;
    expect(schedule.secrets).toBeUndefined();
    expect((await call("GET", `${base}/secrets-nightly/secrets`)).json).toEqual({});
  });

  it("names the missing stub instead of hanging", async () => {
    const { call } = await boot();
    const result = await call("GET", "/api/ui/nope");
    expect(result.status).toBe(404);
    expect((result.json as { error: string }).error).toMatch(/demo: no stub/);
  });
});


it("supports service user lifecycle and grants in the organization demo", async () => {
  const { store, call } = await boot();
  const path = "/api/v1/platform/organizations/demo-organization/service-users";
  const created = await call("POST", path, { name: "CI robot", role: "creator" });
  expect(created.status).toBe(201);
  const serviceUser = (created.json as { serviceUser: { id: string } }).serviceUser;
  const universe = store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)!.universe;
  const grants = `/api/v1/platform/universes/${universe.lightspeedUniverseId}/grants`;
  expect((await call("PUT", grants, { principal: { kind: "serviceUser", id: serviceUser.id }, role: "operator" })).status).toBe(200);
  expect((await call("GET", grants)).json).toMatchObject({ grants: expect.arrayContaining([expect.objectContaining({ principal: { kind: "serviceUser", id: serviceUser.id }, role: "operator" })]) });
  expect((await call("PATCH", `${path}/${serviceUser.id}`, { enabled: false })).status).toBe(200);
  expect((await call("PUT", grants, { principal: { kind: "serviceUser", id: serviceUser.id }, role: "owner" })).status).toBe(400);
  expect((await call("GET", `${path}/${serviceUser.id}/credentials`)).json).toEqual({ credentials: [] });
  expect((await call("POST", `${path}/${serviceUser.id}/credentials`, { name: "automation" })).status).toBe(400);
});

it("clears expiry explicitly before enabling protection in the demo", async () => {
  const { store, call } = await boot();
  const universe = store.universe(SOFTWARE_FACTORY_UNIVERSE_ID)!.universe;
  const path = `/api/v1/platform/universes/${universe.lightspeedUniverseId}`;
  universe.expiresAtMs = Date.now() + 60_000;
  expect((await call("PATCH", path, { protected: true })).status).toBe(409);
  expect((await call("PATCH", path, { expiresAtMs: null })).status).toBe(200);
  expect((await call("PATCH", path, { protected: true })).status).toBe(200);
  expect((await call("DELETE", `/api/ui/universes/${universe.id}`, { handle: universe.handle })).status).toBe(409);
});
