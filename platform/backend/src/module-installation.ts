import { and, eq } from "drizzle-orm";
import { schema, type Db } from "@lightspeed-ai/platform-db";
import type { PlatformModule } from "@lightspeed-ai/platform-shared";

/** A distribution provides the module and its default. An organization can
 * override that default only for its own installation. */
export async function organizationModuleEnabled(db: Db, organizationId: string, module: PlatformModule | undefined): Promise<boolean> {
  if (!module) return false;
  const table = schema.organizationModules;
  const [setting] = await db.select({ enabled: table.enabled }).from(table).where(and(
    eq(table.organizationId, organizationId), eq(table.moduleId, module.id),
  )).limit(1);
  return setting?.enabled ?? module.defaultEnabled;
}
