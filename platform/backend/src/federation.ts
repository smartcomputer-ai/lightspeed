import { decodeJwt, decodeProtectedHeader, jwtVerify } from "jose";
import type { z } from "zod";
import type { federationConfig } from "./federation-config.js";
import { IssuerKeys } from "./federation-keys.js";

export interface WorkloadIdentity {
  issuer: string;
  subject: string;
  serviceAccountUid?: string;
}

export const MAX_WORKLOAD_TOKEN_LENGTH = 16 * 1024;
export const compactJws = /^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/;

export class WorkloadFederation {
  private readonly issuers;
  constructor(readonly config: z.infer<typeof federationConfig>) {
    this.issuers = new Map(config.issuers.map(issuer => [issuer.issuer, { issuer, keys: new IssuerKeys(issuer) }]));
  }

  async verify(token: string): Promise<WorkloadIdentity | null> {
    if (token.length > MAX_WORKLOAD_TOKEN_LENGTH || !compactJws.test(token)) return null;
    try {
      const { iss } = decodeJwt(token);
      const trusted = iss && this.issuers.get(iss);
      if (!trusted) return null;
      const { alg, kid } = decodeProtectedHeader(token);
      const { issuer, keys } = trusted;
      if (!kid || typeof kid !== "string" || !issuer.algorithms.some(a => a === alg)) return null;
      const { payload } = await jwtVerify(token, await keys.resolve(kid), {
        issuer: issuer.issuer, audience: issuer.audiences, algorithms: issuer.algorithms,
        requiredClaims: ["exp", "sub", ...(issuer.maxTokenLifetimeSeconds ? ["iat"] : [])],
        clockTolerance: issuer.clockSkewSeconds,
      });
      // PostgreSQL text must represent the signed subject without replacement or truncation.
      if (typeof payload.sub !== "string" || !payload.sub || /[\p{Cs}\u0000]/u.test(payload.sub) || Buffer.byteLength(payload.sub) > 1024) return null;
      if (Array.isArray(payload.aud) && !payload.aud.every(value => typeof value === "string")) return null;
      if (!Number.isFinite(payload.exp) || (payload.nbf !== undefined && !Number.isFinite(payload.nbf))) return null;
      if (payload.iat !== undefined && (!Number.isFinite(payload.iat) ||
        payload.iat > Date.now() / 1000 + issuer.clockSkewSeconds || payload.exp! <= payload.iat ||
        (issuer.maxTokenLifetimeSeconds && payload.exp! - payload.iat > issuer.maxTokenLifetimeSeconds))) return null;
      const kubernetes = payload["kubernetes.io"] as { serviceaccount?: { uid?: unknown } } | null | undefined;
      const uid = kubernetes?.serviceaccount?.uid;
      return { issuer: issuer.issuer, subject: payload.sub, ...(typeof uid === "string" ? { serviceAccountUid: uid } : {}) };
    } catch { return null; }
  }
}
