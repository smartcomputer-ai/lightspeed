/** Demo account state stays in memory; it cannot issue usable credentials. */
import { Hono } from "hono";
import { accountCreateSchema, accountUpdateSchema, accountMemberPutSchema, applicationCreateSchema, applicationUpdateSchema, accountUniverseCreateSchema, universeGrantPutSchema, universeLifecycleUpdateSchema, type AccountRole, type PlatformOperationResult } from "@lightspeed-ai/platform-shared";
import type { DemoStore } from "../store";
type Account = PlatformOperationResult<"platform/accounts/read">["account"];
type Application = PlatformOperationResult<"platform/accounts/applications/list">["applications"][number];
type Grant = PlatformOperationResult<"platform/universes/grants/list">["grants"][number];
export function accountRoutes(store: DemoStore) {
  const app = new Hono();
  const accounts = new Map<string, Account>([["demo-account", { id: "demo-account", name: "Demo", slug: "demo", kind: "development", enabled: true, role: "admin", universeQuota: 100, defaultProtected: false, requireProtected: false, createdAt: new Date().toISOString() }]]);
  const memberships = new Map<string, Map<string, AccountRole>>([["demo-account", new Map([...store.users.values()].map(u => [u.id, u.id === store.currentUser.id ? "admin" : "member"]))]]);
  const applications = new Map<string, Application>();
  const applicationGrants = new Map<string, Grant[]>();
  const stateFor = (id: string) => [...store.universes.values()].find(s => s.universe.lightspeedUniverseId === id || s.universe.id === id || s.universe.handle === id);
  app.use("/accounts/:accountId/*", async (c, next) => { if (!accounts.has(c.req.param("accountId")!)) return c.json({ error: "Account not found" }, 404); await next(); });
  app.get("/accounts", c => c.json({ accounts: [...accounts.values()], canCreate: store.currentUser.role === "admin" }));
  app.post("/accounts", async c => {
    const parsed = accountCreateSchema.safeParse(await c.req.json()); if (!parsed.success) return c.json({ error: "Invalid account" }, 400);
    if ([...accounts.values()].some(a => a.slug === parsed.data.slug)) return c.json({ error: "Account handle already exists" }, 409);
    const { initialAdminUserId = store.currentUser.id, ...input } = parsed.data;
    if (!store.users.has(initialAdminUserId)) return c.json({ error: "User not found" }, 404);
    const account: Account = { ...input, id: crypto.randomUUID(), enabled: true, role: "admin", universeQuota: input.universeQuota ?? 100, defaultProtected: input.defaultProtected ?? input.kind !== "development", requireProtected: input.requireProtected ?? false, createdAt: new Date().toISOString() };
    accounts.set(account.id, account); memberships.set(account.id, new Map([[initialAdminUserId, "admin"]])); return c.json({ account }, 201);
  });
  app.get("/accounts/:accountId", c => { const account = accounts.get(c.req.param("accountId")); return account ? c.json({ account }) : c.json({ error: "Account not found" }, 404); });
  app.get("/accounts/:accountId/members", c => c.json({ members: [...(memberships.get(c.req.param("accountId")) ?? [])].flatMap(([userId, role]) => { const u = store.users.get(userId); return u ? [{ id: userId, userId, name: u.name, email: u.email, role, createdAt: accounts.get(c.req.param("accountId"))!.createdAt }] : []; }) }));
  app.put("/accounts/:accountId/members/:userId", async c => {
    const parsed = accountMemberPutSchema.safeParse({ ...await c.req.json(), userId: c.req.param("userId") }); if (!parsed.success) return c.json({ error: "Invalid member" }, 400);
    const user = store.users.get(parsed.data.userId); if (!user) return c.json({ error: "User not found" }, 404);
    const roles = memberships.get(c.req.param("accountId"))!;
    if (roles.get(user.id) === "admin" && parsed.data.role !== "admin" && ![...roles].some(([id, role]) => id !== user.id && role === "admin")) return c.json({ error: "Keep at least one Account Admin" }, 409);
    roles.set(user.id, parsed.data.role); return c.json({ member: { id: user.id, userId: user.id, name: user.name, email: user.email, role: parsed.data.role, createdAt: new Date().toISOString() } });
  });
  app.delete("/accounts/:accountId/members/:userId", c => {
    const roles = memberships.get(c.req.param("accountId"))!; const userId = c.req.param("userId");
    if (roles.get(userId) === "admin" && ![...roles].some(([id, role]) => id !== userId && role === "admin")) return c.json({ error: "Keep at least one Account Admin" }, 409);
    roles.delete(userId); return c.json({ ok: true });
  });
  app.get("/accounts/:accountId/applications", c => c.json({ applications: [...applications.values()].filter(a => a.accountId === c.req.param("accountId")) }));
  app.post("/accounts/:accountId/applications", async c => {
    const parsed = applicationCreateSchema.safeParse(await c.req.json()); if (!parsed.success) return c.json({ error: "Invalid application" }, 400);
    const application: Application = { ...parsed.data, id: crypto.randomUUID(), accountId: c.req.param("accountId"), enabled: true, createdAt: new Date().toISOString() };
    applications.set(application.id, application); return c.json({ application }, 201);
  });
  app.patch("/accounts/:accountId/applications/:applicationId", async c => {
    const application = applications.get(c.req.param("applicationId")); if (!application || application.accountId !== c.req.param("accountId")) return c.json({ error: "Application not found" }, 404);
    const parsed = applicationUpdateSchema.safeParse(await c.req.json()); if (!parsed.success) return c.json({ error: "Invalid application" }, 400);
    Object.assign(application, parsed.data); return c.json({ application });
  });
  app.patch("/accounts/:accountId", async c => {
    const account = accounts.get(c.req.param("accountId")); if (!account) return c.json({ error: "Account not found" }, 404);
    const parsed = accountUpdateSchema.safeParse(await c.req.json()); if (!parsed.success) return c.json({ error: "Invalid account" }, 400);
    Object.assign(account, parsed.data); return c.json({ account });
  });
  app.post("/accounts/:accountId/universes", async c => {
    const account = accounts.get(c.req.param("accountId"))!;
    const parsed = accountUniverseCreateSchema.safeParse(await c.req.json()); if (!parsed.success) return c.json({ error: "Invalid universe" }, 400);
    const input = parsed.data; const owner = input.initialOwner ?? { kind: "user", id: store.currentUser.id };
    const ownerApp = applications.get(owner.id); const ownerUser = store.users.get(owner.id);
    if (owner.kind === "user" ? !memberships.get(account.id)?.has(owner.id) : !ownerApp?.enabled || ownerApp.accountId !== account.id) return c.json({ error: "Owner must be an active account principal" }, 400);
    const protectedValue = input.protected ?? (account.requireProtected || account.defaultProtected);
    if (account.requireProtected && !protectedValue) return c.json({ error: "Account requires protection" }, 403);
    if (protectedValue && input.expiresAtMs != null) return c.json({ error: "Protected universes cannot expire" }, 409);
    if (input.expiresAtMs != null && input.expiresAtMs <= Date.now()) return c.json({ error: "Expiry must be in the future" }, 400);
    if ([...store.universes.values()].filter(s => s.universe.accountId === account.id).length >= account.universeQuota) return c.json({ error: "Account universe quota reached" }, 409);
    const handle = input.handle ?? input.name.toLowerCase().replace(/[^a-z0-9]+/g, "-");
    if (store.universeByHandle(handle)) return c.json({ error: "Handle already exists" }, 409);
    const state = store.addUniverse({ name: input.name, handle, role: "owner", lightspeedUniverseId: input.universeId });
    Object.assign(state.universe, { accountId: account.id, protected: protectedValue, expiresAtMs: input.expiresAtMs ?? null });
    const createdAt = new Date().toISOString(); const id = store.nextId("grant");
    if (owner.kind === "user") state.members.push({ id, userId: owner.id, name: ownerUser!.name, role: "owner", createdAt });
    else applicationGrants.set(state.universe.id, [{ id, principal: owner, universeId: state.universe.id, name: ownerApp!.name, role: "owner", createdAt }]);
    return c.json({ universe: state.universe }, 201);
  });
  app.patch("/universes/:universe", async c => {
    const state = stateFor(c.req.param("universe")); if (!state) return c.json({ error: "Universe not found" }, 404);
    const parsed = universeLifecycleUpdateSchema.safeParse(await c.req.json()); if (!parsed.success) return c.json({ error: "Invalid update" }, 400);
    const expiry = parsed.data.expiresAtMs === undefined ? state.universe.expiresAtMs : parsed.data.expiresAtMs;
    if ((parsed.data.protected ?? state.universe.protected) && expiry != null) return c.json({ error: "Protected universes cannot expire" }, 409);
    if (accounts.get(state.universe.accountId!)?.requireProtected && parsed.data.protected === false) return c.json({ error: "Account requires protection" }, 403);
    if (parsed.data.expiresAtMs != null && parsed.data.expiresAtMs <= Date.now()) return c.json({ error: "Expiry must be in the future" }, 400);
    Object.assign(state.universe, parsed.data); return c.json({ universe: state.universe });
  });
  const grantsFor = (state: NonNullable<ReturnType<typeof stateFor>>): Grant[] => [...state.members.map(m => ({ id: m.id, universeId: state.universe.id, principal: { kind: "user" as const, id: m.userId }, role: m.role, name: m.name, createdAt: m.createdAt })), ...(applicationGrants.get(state.universe.id) ?? [])];
  app.get("/universes/:universe/grants", c => { const state = stateFor(c.req.param("universe")); return state ? c.json({ grants: grantsFor(state) }) : c.json({ error: "Universe not found" }, 404); });
  app.put("/universes/:universe/grants", async c => {
    const state = stateFor(c.req.param("universe")); if (!state) return c.json({ error: "Universe not found" }, 404);
    const parsed = universeGrantPutSchema.safeParse(await c.req.json()); if (!parsed.success) return c.json({ error: "Invalid grant" }, 400);
    const { principal, role } = parsed.data; const application = applications.get(principal.id); const user = store.users.get(principal.id);
    if (principal.kind === "user" ? !memberships.get(state.universe.accountId!)?.has(principal.id) : !application?.enabled || application.accountId !== state.universe.accountId) return c.json({ error: "Principal must be an active account member" }, 400);
    const grants = grantsFor(state); const prior = grants.find(g => g.principal.kind === principal.kind && g.principal.id === principal.id);
    if (prior?.role === "owner" && role !== "owner" && !grants.some(g => g.id !== prior.id && g.role === "owner")) return c.json({ error: "Keep at least one Owner" }, 409);
    const grant: Grant = { id: prior?.id ?? store.nextId("grant"), universeId: state.universe.id, principal, role, name: principal.kind === "user" ? user!.name : application!.name, createdAt: prior?.createdAt ?? new Date().toISOString() };
    if (principal.kind === "user") { state.members = state.members.filter(m => m.userId !== principal.id); state.members.push({ id: grant.id, userId: principal.id, name: grant.name, role, createdAt: grant.createdAt }); }
    else applicationGrants.set(state.universe.id, [...(applicationGrants.get(state.universe.id) ?? []).filter(g => g.id !== grant.id), grant]);
    return c.json({ grant });
  });
  app.delete("/universes/:universe/grants/:grantId", c => {
    const state = stateFor(c.req.param("universe")); if (!state) return c.json({ error: "Universe not found" }, 404);
    const grants = grantsFor(state); const grant = grants.find(g => g.id === c.req.param("grantId"));
    if (grant?.role === "owner" && !grants.some(g => g.id !== grant.id && g.role === "owner")) return c.json({ error: "Keep at least one Owner" }, 409);
    state.members = state.members.filter(m => m.id !== c.req.param("grantId")); applicationGrants.set(state.universe.id, (applicationGrants.get(state.universe.id) ?? []).filter(g => g.id !== c.req.param("grantId"))); return c.json({ ok: true });
  });
  for (const path of ["/credentials", "/accounts/:accountId/applications/:applicationId/credentials"]) {
    app.get(path, c => c.json({ credentials: [] }));
    app.post(path, c => c.json({ error: "API keys require a deployed Platform. The demo cannot issue credentials." }, 400));
  }
  return app;
}
