// Codegen-only better-auth config: mirrors the server's plugin set so
// `@better-auth/cli generate` emits the matching drizzle schema. Not
// imported at runtime — the runtime config lives in @lightspeed-ai/platform-backend.
import { apiKey } from "@better-auth/api-key";
import { betterAuth } from "better-auth";
import { drizzleAdapter } from "better-auth/adapters/drizzle";
import { drizzle } from "drizzle-orm/node-postgres";
import { admin } from "better-auth/plugins/admin";
import { bearer } from "better-auth/plugins/bearer";
import { organization } from "better-auth/plugins/organization";
import { deviceAuthorization, type DeviceAuthorizationGrant } from "better-auth/plugins/device-authorization";
import { identityUserFields, identitySessionFields, organizationFields, deviceCodeFields } from "../src/auth-fields.js";

const db = drizzle("postgres://codegen-only");

export const auth = betterAuth({
  database: drizzleAdapter(db, { provider: "pg" }),
  emailAndPassword: { enabled: true },
  user: { additionalFields: identityUserFields },
  session: { additionalFields: identitySessionFields },
  plugins: [organization({ creatorRole: "admin", schema: { organization: { additionalFields: organizationFields } } }), admin(), bearer(), apiKey([{ configId: "personal", references: "user" }, { configId: "serviceUser", references: "organization" }]), deviceAuthorization<DeviceAuthorizationGrant<{}, {}>>({ grant: { requestSchemaFields: {}, deviceCodeSchemaFields: deviceCodeFields, authorizeRequest: () => undefined, assertSessionRedemption: () => {}, getVerificationContext: () => undefined } })],
});
