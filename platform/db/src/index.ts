import { drizzle, type NodePgDatabase } from "drizzle-orm/node-postgres";
import { migrate } from "drizzle-orm/node-postgres/migrator";
import { fileURLToPath } from "node:url";
import path from "node:path";
import pg from "pg";
import * as schema from "./schema/index.js";

export { schema };
export type Db = NodePgDatabase<typeof schema>;

export interface DbHandle {
  db: Db;
  pool: pg.Pool;
}

export function createDb(connectionString: string): DbHandle {
  const pool = new pg.Pool({ connectionString });
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
  await migrate(handle.db, { migrationsFolder });
}
