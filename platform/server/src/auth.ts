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
    trustedOrigins: [
      env.baseUrl,
      // Vite dev server (proxying /api) during local frontend development.
      ...(env.baseUrl.startsWith("http://localhost") ? ["http://localhost:5173"] : []),
    ],
    database: drizzleAdapter(db, { provider: "pg", schema }),
    emailAndPassword: {
      enabled: true,
      // Invite-only platform: accounts are created by an admin (or, later,
      // through organization invitations); public signup stays closed.
      disableSignUp: true,
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
    plugins: [organization(), admin(), bearer()],
  });
}

export type Auth = ReturnType<typeof createAuth>;
export type Session = Auth["$Infer"]["Session"];
