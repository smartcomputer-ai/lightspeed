import { readMigrationFiles } from "drizzle-orm/migrator";
import { drizzle, type NodePgDatabase } from "drizzle-orm/node-postgres";
import { migrate } from "drizzle-orm/node-postgres/migrator";
import { fileURLToPath } from "node:url";
import path from "node:path";
import pg from "pg";
import * as schema from "./schema/index.js";
import { workloadIdentityPoolConfig, type DatabaseWorkloadIdentity } from "./workload-identity.js";

export { schema };
export { databaseWorkloadIdentityFromEnv, type DatabaseWorkloadIdentity } from "./workload-identity.js";
export { identityUserFields, identitySessionFields, organizationFields, deviceCodeFields } from "./auth-fields.js";
export type Db = NodePgDatabase<typeof schema>;

export interface DbHandle {
  db: Db;
  pool: pg.Pool;
}

/// Limits how often connections open, because each one costs a TLS handshake and
/// server-side authentication. `pg` does not check a connection before handing it
/// out, so keepalive is what stops long idle periods from leaving dead sockets.
export const POOL_TUNING = {
  min: 2,
  idleTimeoutMillis: 10 * 60_000,
  maxLifetimeSeconds: 60 * 60,
  keepAlive: true,
  keepAliveInitialDelayMillis: 60_000,
  // Idle connections must not keep a finished script or test process alive.
  allowExitOnIdle: true,
} satisfies pg.PoolConfig;

export function createDb(
  connectionString: string,
  workloadIdentity?: DatabaseWorkloadIdentity | null,
): DbHandle {
  const pool = new pg.Pool({
    ...POOL_TUNING,
    ...(workloadIdentity
      ? workloadIdentityPoolConfig(connectionString, workloadIdentity)
      : { connectionString }),
  });
  // Without a listener, losing an idle connection raises an uncaught error and ends
  // the process. The pool replaces the connection on demand.
  pool.on("error", (error) => {
    console.error(`[db] idle connection lost: ${error.message}`);
  });
  const db = drizzle(pool, { schema, casing: "snake_case" });
  return { db, pool };
}

const migrationsFolder = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "migrations",
);

/// Applies pending SQL migrations from db/migrations. Called on server
/// startup — the platform role owns its own database, so no separate
/// migration job is needed.
export async function migrateDb(handle: DbHandle): Promise<void> {
  const config = { migrationsFolder };
  const { rows: [ledger] } = await handle.pool.query<{ present: boolean }>(
    "SELECT to_regclass('drizzle.__drizzle_migrations') IS NOT NULL AS present",
  );
  if (ledger?.present) {
    const { rows: [firstApplied] } = await handle.pool.query<{
      hash: string;
      created_at: string | null;
    }>(
      "SELECT hash, created_at FROM drizzle.__drizzle_migrations ORDER BY id LIMIT 1",
    );
    if (firstApplied) {
      const [baseline] = readMigrationFiles(config);
      // Drizzle decides which migrations to run by timestamp. Verify the
      // baseline first so an older ledger cannot be mistaken for this one.
      if (
        !baseline ||
        firstApplied.hash !== baseline.hash ||
        Number(firstApplied.created_at) !== baseline.folderMillis
      ) {
        throw new Error("Platform database uses a different migration baseline. Initialize a fresh database for this build.");
      }
    }
  }
  await migrate(handle.db, config);
}
