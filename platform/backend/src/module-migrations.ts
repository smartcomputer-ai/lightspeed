import { sql } from "drizzle-orm";
import { readMigrationFiles } from "drizzle-orm/migrator";
import type { ApiModule, ModuleDatabase } from "./module-api.js";

/** Apply each bundled module's SQL before serving requests, independently of organization activation. */
export async function migrateModuleDatabases(db: ModuleDatabase, modules: readonly ApiModule[]): Promise<void> {
  for (const { id, migrationsFolder } of modules) {
    if (!migrationsFolder) continue;
    // PostgreSQL truncates identifiers longer than 63 bytes. Keep module ledgers distinct.
    if (!/^[a-z][a-z0-9-]{0,55}$/.test(id)) throw new Error(`Invalid database module ID: ${id}`);
    const migrations = readMigrationFiles({ migrationsFolder });
    const ledger = sql`"drizzle".${sql.identifier("module_" + id)}`;
    await db.transaction(async (tx) => {
      // A second replica must read the ledger after the first replica commits.
      await tx.execute(sql`SELECT pg_advisory_xact_lock(hashtextextended(${"lightspeed.module." + id}, 0))`);
      await tx.execute(sql`CREATE SCHEMA IF NOT EXISTS "drizzle"`);
      await tx.execute(sql`CREATE TABLE IF NOT EXISTS ${ledger} (version integer PRIMARY KEY, hash text NOT NULL)`);
      const { rows } = await tx.execute<{ version: number; hash: string }>(sql`SELECT version, hash FROM ${ledger} ORDER BY version`);
      for (const [index, applied] of rows.entries()) {
        if (applied.version !== index || applied.hash !== migrations[index]?.hash) {
          throw new Error(`Module ${id} migration ${applied.version} does not match the installed history`);
        }
      }
      for (let version = rows.length; version < migrations.length; version++) {
        const migration = migrations[version]!;
        for (const statement of migration.sql) {
          await tx.execute(sql.raw(statement));
        }
        await tx.execute(sql`INSERT INTO ${ledger} (version, hash) VALUES (${version}, ${migration.hash})`);
      }
    });
  }
}
