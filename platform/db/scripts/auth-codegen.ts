// Codegen-only better-auth config: mirrors the server's plugin set so
// `@better-auth/cli generate` emits the matching drizzle schema. Not
// imported at runtime — the runtime config lives in @lightspeed/platform-server.
import { betterAuth } from "better-auth";
import { drizzleAdapter } from "better-auth/adapters/drizzle";
import { drizzle } from "drizzle-orm/node-postgres";
import { admin } from "better-auth/plugins/admin";
import { bearer } from "better-auth/plugins/bearer";
import { organization } from "better-auth/plugins/organization";

const db = drizzle("postgres://codegen-only");

export const auth = betterAuth({
  database: drizzleAdapter(db, { provider: "pg" }),
  emailAndPassword: { enabled: true },
  plugins: [organization(), admin(), bearer()],
});
