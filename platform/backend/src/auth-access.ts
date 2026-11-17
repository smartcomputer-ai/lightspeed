import type { ServerEnv } from "./env.js";
import { hasPlatformAdminRole } from "./context.js";

export interface AccessSession {
  user: {
    role?: string | null; banned?: boolean | null;
    identitySource?: string | null; companyAdmitted?: boolean | null; oidcIssuer?: string | null;
    providerCheckedAt?: Date | string | null;
    emergencyAdmin?: boolean | null; accessVersion?: number | null;
  };
  session: { expiresAt: Date | string; createdAt: Date | string; accessVersion?: number | null };
}

export function passwordMode(env: ServerEnv) {
  return env.passwordSignIn ?? (env.oidc ? "break-glass" : "local");
}

export function canUsePassword(user: AccessSession["user"], env: ServerEnv): boolean {
  if (user.identitySource === "company" || user.banned || passwordMode(env) === "off") return false;
  return passwordMode(env) === "local" || (user.emergencyAdmin === true && hasPlatformAdminRole(user));
}

export function sessionAllowed(session: AccessSession, env: ServerEnv, now = Date.now()): boolean {
  const { user, session: record } = session;
  const expiresAt = new Date(record.expiresAt).getTime();
  const createdAt = new Date(record.createdAt).getTime();
  if (!Number.isFinite(expiresAt) || !Number.isFinite(createdAt) || user.banned || expiresAt <= now ||
      (user.accessVersion ?? 0) !== (record.accessVersion ?? 0)) return false;
  if (env.oidc && createdAt + env.oidc.sessionMaxAgeSeconds * 1000 <= now) return false;
  if (!env.oidc && createdAt + (env.browserSessionMaxDays ?? 90) * 86400000 <= now) return false;
  if (user.identitySource === "company") return !!env.oidc && user.oidcIssuer === env.oidc.issuer && user.companyAdmitted === true;
  return !env.oidc || canUsePassword(user, env);
}

/** Company API credentials use fresh admission state, independently of browser sessions.
 * Until an authoritative directory feed exists, sign-in must renew that state within
 * the configured company admission window. A valid JWT alone cannot refresh groups. */
export function principalAllowed(user: AccessSession["user"], env: ServerEnv, now = Date.now()): boolean {
  if (user.banned) return false;
  if (user.identitySource === "company") {
    const checked = user.providerCheckedAt ? new Date(user.providerCheckedAt).getTime() : NaN;
    return !!env.oidc && user.oidcIssuer === env.oidc.issuer && user.companyAdmitted === true &&
      Number.isFinite(checked) && checked <= now && now - checked < env.oidc.sessionMaxAgeSeconds * 1000;
  }
  return !env.oidc || canUsePassword(user, env);
}
