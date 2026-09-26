import { betterAuth } from "better-auth";
import { drizzleAdapter } from "better-auth/adapters/drizzle";
import { admin } from "better-auth/plugins/admin";
import { bearer } from "better-auth/plugins/bearer";
import { organization } from "better-auth/plugins/organization";
import { schema, type Db } from "@lightspeed/platform-db";
import type { ServerEnv } from "./env.js";

export function createAuth(db: Db, env: ServerEnv) {
  return betterAuth({
    baseURL: env.baseUrl,
    secret: env.authSecret,
    basePath: "/api/auth",
    trustedOrigins: [...new Set([env.baseUrl, ...env.trustedOrigins])],
    database: drizzleAdapter(db, { provider: "pg", schema }),
    emailAndPassword: {
      enabled: true,
      // Invite-only platform: accounts are created by a platform admin (or,
      // later, through organization invitations); public signup stays closed.
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
    // Organizations are universes. Their tables are the membership record;
    // the plugin's own endpoints are not served (see `buildApp`): members and
    // roles change only through the universe routes.
    plugins: [organization({ allowUserToCreateOrganization: false }), admin(), bearer()],
  });
}

export type Auth = ReturnType<typeof createAuth>;
export type Session = Auth["$Infer"]["Session"];
