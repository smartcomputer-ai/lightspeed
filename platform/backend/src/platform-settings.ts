import { eq } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { PLATFORM_SETTINGS, type PlatformSettingKey, type PlatformSettings } from "@lightspeed-ai/platform-shared";
import type { AppContext } from "./context.js";
import type { ServerEnv } from "./env.js";
import { GateRefusal } from "./runtime-client.js";

type Db = Pick<AppContext["db"], "select">;
type SettingValue = PlatformSettings[PlatformSettingKey];
const KEYS = Object.keys(PLATFORM_SETTINGS) as PlatformSettingKey[];

/** The environment value for each setting, which wins over a value set here. */
function environmentValue(env: ServerEnv, key: PlatformSettingKey): SettingValue | undefined {
  switch (key) {
    case "personalOrganizations": return env.personalOrganizations;
    case "personalUniverseQuota": return env.personalOrganizationUniverseQuota;
    case "sharedUniverseQuota": return env.sharedOrganizationUniverseQuota;
  }
}

/** Every setting with the value in effect and where it comes from:
 * the environment, then a value set here, then the built-in default. */
export async function listPlatformSettings(db: Db, env: ServerEnv) {
  const rows = new Map((await db.select().from(schema.platformSettings)).map(row => [row.key, row]));
  return KEYS.map(key => {
    const definition = PLATFORM_SETTINGS[key];
    const fromEnvironment = environmentValue(env, key);
    const stored = rows.get(key);
    const parsed = stored ? definition.schema.safeParse(stored.value) : undefined;
    const source = fromEnvironment !== undefined ? "environment" as const : parsed?.success ? "platform" as const : "default" as const;
    const value: SettingValue = fromEnvironment ?? (parsed?.success ? parsed.data : definition.defaultValue);
    return { key, value, source, defaultValue: definition.defaultValue, environmentVariable: definition.environmentVariable, updatedAt: stored ? stored.updatedAt.toISOString() : null };
  });
}

/** The value in effect for every setting. */
export async function platformSettings(db: Db, env: ServerEnv): Promise<PlatformSettings> {
  return Object.fromEntries((await listPlatformSettings(db, env)).map(setting => [setting.key, setting.value])) as PlatformSettings;
}

/** Set a value here, or with null return to the default. A setting the
 * environment holds cannot be changed: the deployment configuration owns it. */
export async function putPlatformSetting(db: AppContext["db"], env: ServerEnv, actorId: string, key: PlatformSettingKey, value: SettingValue | null) {
  if (environmentValue(env, key) !== undefined) throw new GateRefusal(409, `${PLATFORM_SETTINGS[key].environmentVariable} is set in the deployment's environment; change it there`);
  if (value === null) await db.delete(schema.platformSettings).where(eq(schema.platformSettings.key, key));
  else {
    const parsed = PLATFORM_SETTINGS[key].schema.safeParse(value);
    if (!parsed.success) throw new GateRefusal(400, `invalid value for ${key}`);
    await db.insert(schema.platformSettings).values({ key, value: parsed.data, updatedBy: actorId })
      .onConflictDoUpdate({ target: schema.platformSettings.key, set: { value: parsed.data, updatedBy: actorId, updatedAt: new Date() } });
  }
  return (await listPlatformSettings(db, env)).find(setting => setting.key === key)!;
}

/** An organization's universe limit: its own, or the deployment default for its kind. */
export function effectiveUniverseQuota(organization: Pick<typeof schema.organization.$inferSelect, "universeQuota" | "personalUserId">, settings: PlatformSettings) {
  return organization.universeQuota ?? (organization.personalUserId ? settings.personalUniverseQuota : settings.sharedUniverseQuota);
}
