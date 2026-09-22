import { provisioningClient } from "./runtime-client.js";
import { betterAuth } from "better-auth";
import { drizzleAdapter } from "better-auth/adapters/drizzle";
import { bearer } from "better-auth/plugins/bearer";
import { schema, type Db } from "@lightspeed/platform-db";
import type { ServerEnv } from "./env.js";

export function createAuth(db: Db, env: ServerEnv) {
  return betterAuth({
    // The core principal is assigned by the create hook below, never by input.
    // Better Auth validates required fields before hooks run, so the field
    // must be optional at the input layer; the database column stays NOT NULL.
    user: { additionalFields: { corePrincipalId: { type: "string", required: false, input: false } } },
    baseURL: env.baseUrl,
    secret: env.authSecret,
    basePath: "/api/auth",
    trustedOrigins: [...new Set([env.baseUrl, ...env.trustedOrigins])],
    database: drizzleAdapter(db, { provider: "pg", schema }),
    emailAndPassword: {
      enabled: true,
      // Local accounts are created by core deployment administrators.
      disableSignUp: true,
    },
    account: {
      // An external login never attaches to an existing user by e-mail: a
      // provider account is a distinct identity until an administrator says
      // otherwise. Local users are inserted with a verified e-mail, which
      // would otherwise make them link targets.
      accountLinking: { enabled: false },
    },
    ...(env.github
      ? {
          socialProviders: {
            github: {
              clientId: env.github.clientId,
              clientSecret: env.github.clientSecret,
            },
          },
        }
      : {}),
    databaseHooks: {
      user: { create: { before: async (user) => {
        const corePrincipalId = crypto.randomUUID();
        await provisioningClient(env).call("deployment/identity/apply", {
          operation: "create_principal", id: corePrincipalId, kind: "user",
          displayName: user.name, managementScope: { kind: "deployment" },
        });
        return { data: { ...user, corePrincipalId } };
      } } },
    },
    plugins: [bearer()],
  });
}

export type Auth = ReturnType<typeof createAuth>;
export type Session = Auth["$Infer"]["Session"];
