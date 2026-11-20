import { exportJWK, generateKeyPair, SignJWT, type JWTPayload } from "jose";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { federationConfig } from "../src/federation-config.js";
import { WorkloadFederation } from "../src/federation.js";

export async function workloadIssuer() {
  const issuer = "https://workloads.example.test/";
  const audience = "lightspeed-test";
  const subject = "system:serviceaccount:team-research:apply";
  const dir = await mkdtemp(join(tmpdir(), "lightspeed-issuer-"));
  const jwksFile = join(dir, "keys.json");
  const pair = await generateKeyPair("ES256");
  const jwk = { ...await exportJWK(pair.publicKey), kid: "first" };
  await writeFile(jwksFile, JSON.stringify({ keys: [jwk] }));
  const trusted = { name: "cluster", issuer, audiences: [audience], algorithms: ["ES256"],
    keys: { jwksFile }, maxTokenLifetimeSeconds: 600, clockSkewSeconds: 0 };
  function mint(claims: JWTPayload = {}, kid = jwk.kid) {
    const now = Math.floor(Date.now() / 1000);
    return new SignJWT({ iss: issuer, aud: audience, sub: subject, iat: now, exp: now + 300, ...claims })
      .setProtectedHeader({ alg: "ES256", kid }).sign(pair.privateKey);
  }
  return { issuer, audience, subject, dir, jwksFile, jwk, trusted, mint,
    federation: new WorkloadFederation(federationConfig.parse({ issuers: [trusted] })),
    close: () => rm(dir, { force: true, recursive: true }) };
}
