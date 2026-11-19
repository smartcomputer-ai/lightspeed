import { readFile } from "node:fs/promises";
import http from "node:http";
import https from "node:https";
import { createLocalJWKSet, type JSONWebKeySet } from "jose";
import { keySourceUrl, type TrustedIssuer } from "./federation-config.js";

const REFRESH_INTERVAL = 30_000;
const CACHE_TTL = 60 * 60_000;
const MAX_CACHE_AGE = 24 * CACHE_TTL;

async function getJson(url: URL, ca?: Buffer, bearer?: string): Promise<unknown> {
  const get = url.protocol === "https:" ? https.get : http.get;
  const response = await new Promise<http.IncomingMessage>((resolve, reject) => {
    get(url, { ca, headers: bearer ? { authorization: `Bearer ${bearer}` } : {},
      signal: AbortSignal.timeout(5000) }, resolve).on("error", reject);
  });
  try {
    if (response.statusCode !== 200) throw new Error("Key source unavailable");
    const chunks: Buffer[] = [];
    let size = 0;
    for await (const chunk of response) {
      size += chunk.length;
      if (size > 1024 * 1024) throw new Error("Oversized key source");
      chunks.push(chunk);
    }
    return JSON.parse(Buffer.concat(chunks).toString("utf8"));
  } finally { response.destroy(); }
}

async function loadKeys(config: TrustedIssuer) {
  const source = config.keys;
  let keys: unknown;
  if ("jwksFile" in source) {
    keys = JSON.parse(await readFile(source.jwksFile, "utf8"));
  } else {
    const discoveryUrl = new URL(source.discoveryUrl ?? config.issuer.replace(/\/$/, "") + "/.well-known/openid-configuration");
    const ca = source.caFile ? await readFile(source.caFile) : undefined;
    const bearer = source.bearerTokenFile ? (await readFile(source.bearerTokenFile, "utf8")).trim() : undefined;
    const discovery = await getJson(discoveryUrl, ca, bearer) as { issuer?: unknown; jwks_uri?: unknown } | null;
    if (discovery?.issuer !== config.issuer) throw new Error("Discovery issuer mismatch");
    const url = new URL(source.jwksUrl ?? keySourceUrl.parse(discovery.jwks_uri));
    // Only explicitly configured origins may receive the operator's credential.
    keys = await getJson(url, ca, source.jwksUrl || url.origin === discoveryUrl.origin ? bearer : undefined);
  }
  const resolve = createLocalJWKSet(keys as JSONWebKeySet);
  return { resolve, kids: new Set((keys as JSONWebKeySet).keys.map(key => key.kid)), loaded: Date.now() };
}

/** One cache per configured issuer, shared by ordinary admission and enrollment. */
export class IssuerKeys {
  private cached?: Awaited<ReturnType<typeof loadKeys>>;
  private pending?: Promise<void>;
  private attempted = -Infinity;
  constructor(private readonly issuer: TrustedIssuer) {}

  async resolve(kid: string) {
    const now = Date.now();
    if (!this.cached || now - this.cached.loaded >= CACHE_TTL || !this.cached.kids.has(kid)) {
      if (!this.pending && now - this.attempted >= REFRESH_INTERVAL) {
        this.attempted = now;
        this.pending = loadKeys(this.issuer)
          .then(cached => { this.cached = cached; })
          .finally(() => { this.pending = undefined; });
      }
      try { await this.pending; } catch { /* Serve the last good set within its maximum age. */ }
    }
    if (!this.cached || Date.now() - this.cached.loaded >= MAX_CACHE_AGE) throw new Error("No current issuer keys");
    return this.cached.resolve;
  }
}
