import { serve } from "@hono/node-server";
import { createDb, migrateDb } from "@lightspeed-ai/platform-db";
import { buildApp } from "./api.js";
import { createAuth } from "./auth.js";
import { bootstrapAdmin } from "./bootstrap.js";
import { startDeclaredUniverseSync } from "./declared-universes.js";
import { seedDevelopment } from "./development-seed.js";
import { loadEnv } from "./env.js";
import type { ApiModule } from "./module-api.js";
import { migrateModuleDatabases } from "./module-migrations.js";

export { buildApp };
export { migrateModuleDatabases };
export type { AppContext } from "./context.js";

/** Start the Platform with the private routes chosen by the distribution. */
export async function startPlatformServer(modules: readonly ApiModule[]): Promise<void> {
  const env = loadEnv();
  const { db, pool } = createDb(env.databaseUrl, env.databaseWorkloadIdentity);

  console.log("[main] applying migrations…");
  await migrateDb({ db, pool });
  await migrateModuleDatabases(db, modules);
  await bootstrapAdmin(db, env);
  await seedDevelopment(db, env);

  const auth = createAuth(db, env);
  const context = { db, pool, auth, env };
  const app = buildApp(context, modules);
  const stopUniverseSync = startDeclaredUniverseSync(context);

  const server = serve({ fetch: app.fetch, port: env.port }, (info) => {
    console.log(`[main] Lightspeed platform listening on :${info.port}`);
  });

  for (const signal of ["SIGINT", "SIGTERM"] as const) {
    process.on(signal, () => {
      console.log(`[main] ${signal} — shutting down`);
      stopUniverseSync();
      server.close(() => {
        void pool.end().finally(() => process.exit(0));
      });
    });
  }
}
