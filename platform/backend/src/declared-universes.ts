import { isDeepStrictEqual } from "node:util";
import { eq, or, isNull, sql } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import type { DeploymentUniverseConfigurationPutParams } from "@lightspeed-ai/sdk";
import { roleAtLeast, universeRoleSchema, type UniverseRole } from "@lightspeed-ai/platform-shared";
import type { AppContext } from "./context.js";
import { deploymentClient } from "./runtime-client.js";

export type UniverseConfiguration = Omit<DeploymentUniverseConfigurationPutParams, "universe">;

/** Highest applicable role; declarations cannot grant platform administration. */
export function effectiveDeclaredRole(configuration: UniverseConfiguration, identity: {
  email: string; emailVerified: boolean; oidcIssuer: string | null; oidcSubject: string | null;
  identitySource: string; companyAdmitted: boolean; directoryGroups: string[]; banned?: boolean | null;
}, manual?: string | null): UniverseRole | undefined {
  if (identity.banned) return undefined;
  let role = universeRoleSchema.safeParse(manual).data;
  const grant = (candidate: unknown) => {
    const parsed = universeRoleSchema.safeParse(candidate).data;
    if (parsed && (!role || roleAtLeast(parsed, role))) role = parsed;
  };
  const company = identity.identitySource === "company" && identity.companyAdmitted;
  for (const member of configuration.members ?? []) {
    const email = member.email && !member.issuer && !member.subject && identity.emailVerified
      && member.email.toLowerCase() === identity.email.toLowerCase();
    const subject = !member.email && member.issuer && member.subject && company
      && member.issuer === identity.oidcIssuer && member.subject === identity.oidcSubject;
    if (email || subject) grant(member.role);
  }
  for (const mapping of configuration.directoryGroupMappings ?? []) {
    if (company && mapping.issuer === identity.oidcIssuer && identity.directoryGroups.includes(mapping.group)) grant(mapping.role);
  }
  return role;
}

export async function declaredRole(ctx: AppContext, userId: string, configuration: UniverseConfiguration | undefined, manual?: string | null) {
  if (!configuration?.members?.length && !configuration?.directoryGroupMappings?.length) return universeRoleSchema.safeParse(manual).data;
  const [identity] = await ctx.db.select().from(schema.user).where(eq(schema.user.id, userId)).limit(1);
  return identity ? effectiveDeclaredRole(configuration, identity, manual) : undefined;
}

/** Reconcile only the configured Runtime. A database lock serializes replicas,
 * including the inventory read, so an older snapshot cannot overwrite a newer one. */
export async function syncDeclaredUniverses(ctx: AppContext): Promise<{ adopted: number; updated: number }> {
  if (!ctx.env.lightspeedApiUrl || !ctx.env.lightspeedApiKey) return { adopted: 0, updated: 0 };
  return ctx.db.transaction(async tx => {
    const locked = await tx.execute(sql`SELECT pg_try_advisory_xact_lock(731845920114) AS locked`);
    if (!locked.rows[0]?.locked) return { adopted: 0, updated: 0 };
    const response = await deploymentClient(ctx.env).call("deployment/universes/list", {}, { signal: AbortSignal.timeout(10000) });
    const inventory = response.result.universes ?? [];
    const rows = await tx.select({ universe: schema.universes, handle: schema.organization.slug }).from(schema.universes)
      .innerJoin(schema.organization, eq(schema.organization.id, schema.universes.organizationId))
      .where(or(isNull(schema.universes.gatewayUrl), eq(schema.universes.gatewayUrl, ctx.env.lightspeedApiUrl!)));
    const linked = new Map(rows.map(row => [row.universe.lightspeedUniverseId, { ...row.universe, handle: row.handle }]));
    const selected = inventory.filter(u => u.displayName != null || linked.has(u.universeId));
    const handles = new Set<string>();
    for (const universe of selected) {
      if (handles.has(universe.handle)) throw new Error("Runtime inventory contains conflicting universe handles");
      handles.add(universe.handle);
    }
    // Clear managed handles inside the transaction to permit handle swaps.
    for (const universe of selected) {
      const row = linked.get(universe.universeId);
      if (row && row.handle !== universe.handle) await tx.update(schema.organization).set({ slug: `runtime-sync-${crypto.randomUUID()}` }).where(eq(schema.organization.id, row.organizationId));
    }
    let adopted = 0;
    let updated = 0;
    for (const universe of selected) {
      const configuration: UniverseConfiguration = {
        ...(universe.displayName != null ? { displayName: universe.displayName } : {}),
        ...(universe.members != null ? { members: universe.members } : {}),
        ...(universe.directoryGroupMappings != null ? { directoryGroupMappings: universe.directoryGroupMappings } : {}),
      };
      const row = linked.get(universe.universeId);
      if (row) {
        const name = universe.displayName ?? row.name;
        if (name !== row.name || universe.handle !== row.handle) {
          await tx.update(schema.organization).set({ name, slug: universe.handle }).where(eq(schema.organization.id, row.organizationId));
        }
        if (name !== row.name || !isDeepStrictEqual(row.runtimeConfiguration, configuration)) {
          await tx.update(schema.universes).set({ name, runtimeConfiguration: configuration }).where(eq(schema.universes.id, row.id));
          updated++;
        }
      } else if (universe.displayName) {
        const organizationId = crypto.randomUUID();
        await tx.insert(schema.organization).values({ id: organizationId, name: universe.displayName, slug: universe.handle, createdAt: new Date() });
        await tx.insert(schema.universes).values({ organizationId, lightspeedUniverseId: universe.universeId, name: universe.displayName, runtimeConfiguration: configuration });
        adopted++;
      }
    }
    return { adopted, updated };
  });
}

/** The first pass runs immediately; errors retry on the next pass. */
export function startDeclaredUniverseSync(ctx: AppContext): () => void {
  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const run = async () => {
    try { await syncDeclaredUniverses(ctx); }
    catch (error) { console.error("[universes] Runtime adoption failed; retrying in 30 seconds", error instanceof Error ? error.message : "unknown error"); }
    if (!stopped) { timer = setTimeout(() => { void run(); }, 30000); timer.unref(); }
  };
  void run();
  return () => { stopped = true; if (timer) clearTimeout(timer); };
}
