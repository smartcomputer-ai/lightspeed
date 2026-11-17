/** Demo organization state stays in memory; it cannot issue usable credentials. */
import { Hono } from "hono";
import { demoDeploymentAllows, demoOrganizationActions, demoUniverseQuota, demoUniverseView } from "../access";
import { PLATFORM_SETTINGS, platformSettingPutSchema, type PlatformSettingKey, organizationCreateSchema, organizationUpdateSchema, organizationMemberPutSchema, serviceUserCreateSchema, serviceUserUpdateSchema, organizationUniverseCreateSchema, universeGrantPutSchema, universeLifecycleUpdateSchema, type PlatformOperationResult } from "@lightspeed-ai/platform-shared";
import type { DemoStore } from "../store";
type Organization = PlatformOperationResult<"platform/organizations/read">["organization"];
type ServiceUser = PlatformOperationResult<"platform/organizations/service-users/list">["serviceUsers"][number];
type Grant = PlatformOperationResult<"platform/universes/grants/list">["grants"][number];
export function organizationRoutes(store: DemoStore) {
  const app = new Hono();
  const { organizations, organizationMemberships: memberships, serviceUsers, serviceUserGrants } = store;
  if (!organizations.has("demo-organization")) {
    organizations.set("demo-organization", { id: "demo-organization", name: "Demo", slug: "demo", personal: false, role: "admin", actions: [], universeQuota: 100, universeQuotaSource: "deployment", canCreateUniverse: true, baseUniverseRole: "viewer", defaultProtected: false, requireProtected: false, createdAt: new Date().toISOString() });
    memberships.set("demo-organization", new Map([...store.users.values()].map(u => [u.id, u.id === store.currentUser.id ? "admin" : "member"])));
  }
  const stateFor = (id: string) => [...store.universes.values()].find(s => s.universe.lightspeedUniverseId === id || s.universe.id === id || s.universe.handle === id);
  app.use("/organizations/:organizationId/*", async (c, next) => { if (!organizations.has(c.req.param("organizationId")!)) return c.json({ error: "Organization not found" }, 404); await next(); });
  // An organization without its own limit follows the deployment default for its kind.
  const setting = (key: PlatformSettingKey) => store.platformSettings.get(key) ?? PLATFORM_SETTINGS[key].defaultValue;
  const quota = (organization: Organization) => demoUniverseQuota(store, organization);
  const view = (organization: Organization): Organization => {
    const actions = [...demoOrganizationActions(store, organization.role)];
    const universeCount = [...store.universes.values()].filter(state => state.universe.organizationId === organization.id).length;
    return { ...organization, universeQuota: quota(organization), actions, universeCount, canCreateUniverse: actions.includes("createUniverse") && universeCount < quota(organization) };
  };
  app.get("/settings", c => {
    if (!demoDeploymentAllows(store, "configureDeployment")) return c.json({ error: "deployment permission required: configureDeployment" }, 403);
    return c.json({ settings: (Object.keys(PLATFORM_SETTINGS) as PlatformSettingKey[]).map(settingView) });
  });
  app.put("/settings/:key", async c => {
    if (!demoDeploymentAllows(store, "configureDeployment")) return c.json({ error: "deployment permission required: configureDeployment" }, 403);
    const parsed = platformSettingPutSchema.safeParse({ ...await c.req.json(), key: c.req.param("key") }); if (!parsed.success) return c.json({ error: "Invalid setting" }, 400);
    const { key, value } = parsed.data;
    if (value === null) store.platformSettings.delete(key);
    else if (PLATFORM_SETTINGS[key].schema.safeParse(value).success) store.platformSettings.set(key, value);
    else return c.json({ error: `invalid value for ${key}` }, 400);
    return c.json({ setting: settingView(key) });
  });
  const settingView = (key: PlatformSettingKey) => ({ key, value: setting(key), source: store.platformSettings.has(key) ? "platform" as const : "default" as const, defaultValue: PLATFORM_SETTINGS[key].defaultValue, environmentVariable: PLATFORM_SETTINGS[key].environmentVariable, updatedAt: null });
  app.get("/organizations", c => c.json({ organizations: [...organizations.values()].map(view), canCreate: demoDeploymentAllows(store, "createOrganization") }));
  app.post("/organizations", async c => {
    const parsed = organizationCreateSchema.safeParse(await c.req.json()); if (!parsed.success) return c.json({ error: "Invalid organization" }, 400);
    if ([...organizations.values()].some(a => a.slug === parsed.data.slug)) return c.json({ error: "Organization handle already exists" }, 409);
    const { initialAdminUserId = store.currentUser.id, ...input } = parsed.data;
    if (!store.users.has(initialAdminUserId)) return c.json({ error: "User not found" }, 404);
    const organization: Organization = { ...input, id: crypto.randomUUID(), personal: false, role: "admin", actions: [], universeQuota: input.universeQuota ?? 0, universeQuotaSource: input.universeQuota == null ? "deployment" : "organization", canCreateUniverse: true, baseUniverseRole: input.baseUniverseRole === undefined ? "viewer" : input.baseUniverseRole, defaultProtected: input.defaultProtected ?? true, requireProtected: input.requireProtected ?? false, createdAt: new Date().toISOString() };
    organizations.set(organization.id, organization); memberships.set(organization.id, new Map([[initialAdminUserId, "admin"]])); return c.json({ organization: view(organization) }, 201);
  });
  app.get("/organizations/:organizationId", c => { const organization = organizations.get(c.req.param("organizationId")); return organization ? c.json({ organization: view(organization) }) : c.json({ error: "Organization not found" }, 404); });
  app.get("/organizations/:organizationId/members", c => c.json({ members: [...(memberships.get(c.req.param("organizationId")) ?? [])].flatMap(([userId, role]) => { const u = store.users.get(userId); return u ? [{ id: userId, userId, name: u.name, email: u.email, role, createdAt: organizations.get(c.req.param("organizationId"))!.createdAt }] : []; }) }));
  app.put("/organizations/:organizationId/members/:userId", async c => {
    const parsed = organizationMemberPutSchema.safeParse({ ...await c.req.json(), userId: c.req.param("userId") }); if (!parsed.success) return c.json({ error: "Invalid member" }, 400);
    const user = store.users.get(parsed.data.userId); if (!user) return c.json({ error: "User not found" }, 404);
    const roles = memberships.get(c.req.param("organizationId"))!;
    if (roles.get(user.id) === "admin" && parsed.data.role !== "admin" && ![...roles].some(([id, role]) => id !== user.id && role === "admin")) return c.json({ error: "Keep at least one Organization Admin" }, 409);
    roles.set(user.id, parsed.data.role); return c.json({ member: { id: user.id, userId: user.id, name: user.name, email: user.email, role: parsed.data.role, createdAt: new Date().toISOString() } });
  });
  app.delete("/organizations/:organizationId/members/:userId", c => {
    const roles = memberships.get(c.req.param("organizationId"))!; const userId = c.req.param("userId");
    if (roles.get(userId) === "admin" && ![...roles].some(([id, role]) => id !== userId && role === "admin")) return c.json({ error: "Keep at least one Organization Admin" }, 409);
    roles.delete(userId); return c.json({ ok: true });
  });
  app.get("/organizations/:organizationId/service-users", c => c.json({ serviceUsers: [...serviceUsers.values()].filter(a => a.organizationId === c.req.param("organizationId")) }));
  app.post("/organizations/:organizationId/service-users", async c => {
    const parsed = serviceUserCreateSchema.safeParse(await c.req.json()); if (!parsed.success) return c.json({ error: "Invalid service user" }, 400);
    const serviceUser: ServiceUser = { ...parsed.data, id: crypto.randomUUID(), organizationId: c.req.param("organizationId"), enabled: true, createdAt: new Date().toISOString() };
    serviceUsers.set(serviceUser.id, serviceUser); return c.json({ serviceUser }, 201);
  });
  app.patch("/organizations/:organizationId/service-users/:serviceUserId", async c => {
    const serviceUser = serviceUsers.get(c.req.param("serviceUserId")); if (!serviceUser || serviceUser.organizationId !== c.req.param("organizationId")) return c.json({ error: "Service user not found" }, 404);
    const parsed = serviceUserUpdateSchema.safeParse(await c.req.json()); if (!parsed.success) return c.json({ error: "Invalid service user" }, 400);
    Object.assign(serviceUser, parsed.data); return c.json({ serviceUser });
  });
  app.patch("/organizations/:organizationId", async c => {
    const organization = organizations.get(c.req.param("organizationId")); if (!organization) return c.json({ error: "Organization not found" }, 404);
    const parsed = organizationUpdateSchema.safeParse(await c.req.json()); if (!parsed.success) return c.json({ error: "Invalid organization" }, 400);
    const { universeQuota, ...rest } = parsed.data;
    Object.assign(organization, rest);
    if (universeQuota !== undefined) Object.assign(organization, universeQuota === null ? { universeQuotaSource: "deployment" } : { universeQuota, universeQuotaSource: "organization" });
    return c.json({ organization: view(organization) });
  });
  app.post("/organizations/:organizationId/universes", async c => {
    const organization = organizations.get(c.req.param("organizationId"))!;
    const parsed = organizationUniverseCreateSchema.safeParse(await c.req.json()); if (!parsed.success) return c.json({ error: "Invalid universe" }, 400);
    const input = parsed.data; const owner = input.initialOwner ?? { kind: "user", id: store.currentUser.id };
    const ownerApp = serviceUsers.get(owner.id); const ownerUser = store.users.get(owner.id);
    if (owner.kind === "user" ? !memberships.get(organization.id)?.has(owner.id) : !ownerApp?.enabled || ownerApp.organizationId !== organization.id) return c.json({ error: "Owner must be an active organization principal" }, 400);
    const protectedValue = input.protected ?? (organization.requireProtected || organization.defaultProtected);
    if (organization.requireProtected && !protectedValue) return c.json({ error: "Organization requires protection" }, 403);
    if (protectedValue && input.expiresAtMs != null) return c.json({ error: "Protected universes cannot expire" }, 409);
    if (input.expiresAtMs != null && input.expiresAtMs <= Date.now()) return c.json({ error: "Expiry must be in the future" }, 400);
    if ([...store.universes.values()].filter(s => s.universe.organizationId === organization.id).length >= quota(organization)) return c.json({ error: "Organization universe quota reached" }, 409);
    const handle = input.handle ?? input.name.toLowerCase().replace(/[^a-z0-9]+/g, "-");
    if (store.universeByHandle(handle)) return c.json({ error: "Handle already exists" }, 409);
    const state = store.addUniverse({ name: input.name, handle, role: "owner", lightspeedUniverseId: input.universeId });
    Object.assign(state.universe, { organizationId: organization.id, protected: protectedValue, expiresAtMs: input.expiresAtMs ?? null });
    const createdAt = new Date().toISOString(); const id = store.nextId("grant");
    if (owner.kind === "user") state.members.push({ id, userId: owner.id, name: ownerUser!.name, role: "owner", createdAt });
    else serviceUserGrants.set(state.universe.id, [{ id, principal: owner, universeId: state.universe.id, name: ownerApp!.name, role: "owner", active: true, createdAt }]);
    return c.json({ universe: demoUniverseView(store, state) }, 201);
  });
  app.patch("/universes/:universe", async c => {
    const state = stateFor(c.req.param("universe")); if (!state) return c.json({ error: "Universe not found" }, 404);
    const parsed = universeLifecycleUpdateSchema.safeParse(await c.req.json()); if (!parsed.success) return c.json({ error: "Invalid update" }, 400);
    const expiry = parsed.data.expiresAtMs === undefined ? state.universe.expiresAtMs : parsed.data.expiresAtMs;
    if ((parsed.data.protected ?? state.universe.protected) && expiry != null) return c.json({ error: "Protected universes cannot expire" }, 409);
    if (organizations.get(state.universe.organizationId!)?.requireProtected && parsed.data.protected === false) return c.json({ error: "Organization requires protection" }, 403);
    if (parsed.data.expiresAtMs != null && parsed.data.expiresAtMs <= Date.now()) return c.json({ error: "Expiry must be in the future" }, 400);
    Object.assign(state.universe, parsed.data); return c.json({ universe: demoUniverseView(store, state) });
  });
  const grantsFor = (state: NonNullable<ReturnType<typeof stateFor>>): Grant[] => [
    ...state.members.map(m => ({ id: m.id, universeId: state.universe.id, principal: { kind: "user" as const, id: m.userId }, role: m.role, name: m.name, active: memberships.get(state.universe.organizationId!)?.has(m.userId) ?? false, createdAt: m.createdAt })),
    ...(serviceUserGrants.get(state.universe.id) ?? []).map(g => ({ ...g, active: serviceUsers.get(g.principal.id)?.enabled ?? false })),
  ];
  app.get("/universes/:universe/grants", c => { const state = stateFor(c.req.param("universe")); return state ? c.json({ grants: grantsFor(state) }) : c.json({ error: "Universe not found" }, 404); });
  app.put("/universes/:universe/grants", async c => {
    const state = stateFor(c.req.param("universe")); if (!state) return c.json({ error: "Universe not found" }, 404);
    const parsed = universeGrantPutSchema.safeParse(await c.req.json()); if (!parsed.success) return c.json({ error: "Invalid grant" }, 400);
    const { principal, role } = parsed.data; const serviceUser = serviceUsers.get(principal.id); const user = store.users.get(principal.id);
    if (principal.kind === "user" ? !memberships.get(state.universe.organizationId!)?.has(principal.id) : !serviceUser?.enabled || serviceUser.organizationId !== state.universe.organizationId) return c.json({ error: "Principal must be an active organization member" }, 400);
    const grants = grantsFor(state); const prior = grants.find(g => g.principal.kind === principal.kind && g.principal.id === principal.id);
    if (prior?.role === "owner" && role !== "owner" && !grants.some(g => g.id !== prior.id && g.role === "owner" && g.active)) return c.json({ error: "Keep at least one Owner" }, 409);
    const grant: Grant = { id: prior?.id ?? store.nextId("grant"), universeId: state.universe.id, principal, role, name: principal.kind === "user" ? user!.name : serviceUser!.name, active: true, createdAt: prior?.createdAt ?? new Date().toISOString() };
    if (principal.kind === "user") { state.members = state.members.filter(m => m.userId !== principal.id); state.members.push({ id: grant.id, userId: principal.id, name: grant.name, role, createdAt: grant.createdAt }); }
    else serviceUserGrants.set(state.universe.id, [...(serviceUserGrants.get(state.universe.id) ?? []).filter(g => g.id !== grant.id), grant]);
    return c.json({ grant });
  });
  app.delete("/universes/:universe/grants/:grantId", c => {
    const state = stateFor(c.req.param("universe")); if (!state) return c.json({ error: "Universe not found" }, 404);
    const grants = grantsFor(state); const grant = grants.find(g => g.id === c.req.param("grantId"));
    if (grant?.role === "owner" && !grants.some(g => g.id !== grant.id && g.role === "owner" && g.active)) return c.json({ error: "Keep at least one Owner" }, 409);
    state.members = state.members.filter(m => m.id !== c.req.param("grantId")); serviceUserGrants.set(state.universe.id, (serviceUserGrants.get(state.universe.id) ?? []).filter(g => g.id !== c.req.param("grantId"))); return c.json({ ok: true });
  });
  for (const path of ["/credentials", "/organizations/:organizationId/service-users/:serviceUserId/credentials"]) {
    app.get(path, c => c.json({ credentials: [] }));
    app.post(path, c => c.json({ error: "API keys require a deployed Platform. The demo cannot issue credentials." }, 400));
  }
  return app;
}
