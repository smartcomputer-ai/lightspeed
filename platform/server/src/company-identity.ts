import { createHash } from "node:crypto";
import { and, eq, sql } from "drizzle-orm";
import { createRemoteJWKSet, decodeJwt, jwtVerify, type JWTPayload } from "jose";
import { z } from "zod";
import { schema, type Db } from "@lightspeed/platform-db";
import type { OidcConfig } from "./env.js";
import { auditIdentity } from "./identity-audit.js";

export function companyProviderId(config: OidcConfig): string {
  // Better Auth keys accounts by provider id + subject. Pin that namespace
  // to the exact issuer so changing deployments cannot take over accounts.
  return `company-${createHash("sha256").update(config.issuer).digest("hex").slice(0, 24)}`;
}

export interface LoginIdentity {
  userId: string;
  accessVersion: number;
  checkedAt: Date;
  source: "company" | "password";
}

export function applicationAccess(claims: JWTPayload, config: OidcConfig) {
  const value = claims[config.groupsClaim];
  const groups = typeof value === "string" ? [value] : value;
  // Claim names and values are opaque. No splitting, prefixes or role parsing.
  if (!Array.isArray(groups) || !groups.every((group) => typeof group === "string")) {
    return { admitted: false, admin: false };
  }
  const admin = groups.includes(config.adminGroup);
  return { admitted: admin || groups.includes(config.userGroup), admin };
}

export function companyClaimsReader(config: OidcConfig) {
  let accessKeys: ReturnType<typeof createRemoteJWKSet> | undefined;
  return async (tokens: { idToken?: string; accessToken?: string }) => {
    // Generic OAuth calls this only after its signature, issuer, audience and
    // nonce verification. Requiring the token closes its UserInfo fallback.
    if (!tokens.idToken) throw new Error("missing_id_token");
    const identity = decodeJwt(tokens.idToken);
    if (identity.iss !== config.issuer || typeof identity.sub !== "string" || !identity.sub ||
        typeof identity.exp !== "number" || typeof identity.iat !== "number" || identity.iat > Date.now() / 1000 + 30 ||
        identity.exp <= Date.now() / 1000 ||
        (Array.isArray(identity.aud) && identity.aud.length > 1 && identity.azp === undefined) ||
        (identity.azp !== undefined && identity.azp !== config.clientId)) throw new Error("invalid_identity");
    let claims = identity;
    if (config.claimsToken === "access") {
      if (!tokens.accessToken || !config.audience) throw new Error("missing_access_token");
      if (!accessKeys) {
        const response = await fetch(config.discoveryUrl, { signal: AbortSignal.timeout(10000), redirect: "error" });
        if (!response.ok) throw new Error("discovery_unavailable");
        const metadata = await response.json() as { issuer?: unknown; jwks_uri?: unknown };
        if (metadata.issuer !== config.issuer || typeof metadata.jwks_uri !== "string") throw new Error("invalid_discovery");
        const url = new URL(metadata.jwks_uri);
        if (url.protocol !== "https:" && !(url.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname))) throw new Error("invalid_jwks_url");
        accessKeys = createRemoteJWKSet(url);
      }
      const verified = await jwtVerify(tokens.accessToken, accessKeys, {
        issuer: config.issuer, audience: config.audience,
        algorithms: ["RS256", "RS384", "RS512", "PS256", "PS384", "PS512", "ES256", "ES384", "ES512"],
        requiredClaims: ["sub", "exp", "iat"],
      });
      if (verified.payload.sub !== identity.sub || typeof verified.payload.iat !== "number" || verified.payload.iat > Date.now() / 1000 + 30) throw new Error("invalid_access_identity");
      claims = verified.payload;
    }
    return { identity, access: applicationAccess(claims, config) };
  };
}

export async function admitCompanyIdentity(
  db: Db, config: OidcConfig, identity: JWTPayload,
  access: { admitted: boolean; admin: boolean },
): Promise<LoginIdentity | null> {
  const now = new Date();
  return db.transaction(async (tx) => {
    const [existing] = await tx.select().from(schema.user)
      .where(and(eq(schema.user.oidcIssuer, config.issuer), eq(schema.user.oidcSubject, identity.sub!)))
      .for("update");
    if (!access.admitted) {
      if (existing) {
        await tx.update(schema.user).set({ companyAdmitted: false, role: "user", accessVersion: sql`${schema.user.accessVersion} + 1` })
          .where(eq(schema.user.id, existing.id));
        await tx.delete(schema.session).where(eq(schema.session.userId, existing.id));
        await auditIdentity(tx, { actorId: existing.id, action: "company.access", targetId: existing.id, outcome: "denied", details: { admitted: false, admin: false } });
      }
      return null;
    }
    if (existing?.banned) return null;
    // Apply a valid admission/demotion decision even when profile data later
    // prevents this particular sign-in. Older sessions must not keep admin.
    if (existing) {
      await tx.update(schema.user).set({ companyAdmitted: true, role: access.admin ? "admin" : "user", providerCheckedAt: now })
        .where(eq(schema.user.id, existing.id));
      if (!existing.companyAdmitted || (existing.role === "admin") !== access.admin) {
        await auditIdentity(tx, { actorId: existing.id, action: "company.access", targetId: existing.id, details: { admitted: true, admin: access.admin } });
      }
    }
    const email = z.email().safeParse(identity.email);
    if (!email.success) return null;
    const normalizedEmail = email.data.toLowerCase();
    const name = typeof identity.name === "string" && identity.name.trim() ? identity.name :
      typeof identity.preferred_username === "string" && identity.preferred_username ? identity.preferred_username : normalizedEmail;
    const [collision] = await tx.select({ id: schema.user.id }).from(schema.user).where(eq(schema.user.email, normalizedEmail));
    if (collision && collision.id !== existing?.id) return null;
    const id = existing?.id ?? crypto.randomUUID();
    const profile = { name, email: normalizedEmail, role: access.admin ? "admin" : "user", companyAdmitted: true, providerCheckedAt: now, updatedAt: now };
    if (existing) {
      await tx.update(schema.user).set(profile).where(eq(schema.user.id, id));
    } else {
      await tx.insert(schema.user).values({
        id, ...profile, identitySource: "company", oidcIssuer: config.issuer, oidcSubject: identity.sub!,
        emailVerified: false, createdAt: now,
      });
      await tx.insert(schema.account).values({
        id: crypto.randomUUID(), userId: id, providerId: companyProviderId(config), accountId: identity.sub!,
        createdAt: now, updatedAt: now,
      });
    }
    if (!existing) {
      await auditIdentity(tx, { actorId: id, action: "company.access", targetId: id, details: { admitted: true, admin: access.admin } });
    }
    return { userId: id, accessVersion: existing?.accessVersion ?? 0, checkedAt: now, source: "company" };
  });
}
