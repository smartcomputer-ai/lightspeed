// Codegen-only better-auth config: mirrors the server's plugin set so
// `@better-auth/cli generate` emits the matching drizzle schema. Not
// imported at runtime — the runtime config lives in @lightspeed/platform-server.
import { betterAuth } from "better-auth";
import { drizzleAdapter } from "better-auth/adapters/drizzle";
import { drizzle } from "drizzle-orm/node-postgres";
import { bearer } from "better-auth/plugins/bearer";

const db = drizzle("postgres://codegen-only");

export const auth = betterAuth({
  user: { additionalFields: { corePrincipalId: { type: "string", required: true, input: false } } },
  database: drizzleAdapter(db, { provider: "pg" }),
  emailAndPassword: { enabled: true },
  plugins: [bearer()],
});
