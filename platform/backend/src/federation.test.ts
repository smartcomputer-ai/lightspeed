import { createServer } from "node:http";
import { once } from "node:events";
import type { AddressInfo } from "node:net";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";
import { afterAll, beforeAll, expect, it, vi } from "vitest";
import { workloadIssuer } from "../test/workload-issuer.js";
import { federationConfig, readFederationConfig } from "./federation-config.js";
import { WorkloadFederation } from "./federation.js";

let source: Awaited<ReturnType<typeof workloadIssuer>>;
beforeAll(async () => { source = await workloadIssuer(); });
afterAll(async () => { await source.close(); });

it("verifies configured issuer, signature, audience, time claims and lifetime before accepting the subject", async () => {
  const { mint, federation, issuer, audience, subject } = source;
  expect(await federation.verify(await mint({ aud: ["other", audience] }))).toEqual({ issuer, subject });
  const now = Math.floor(Date.now() / 1000);
  for (const claims of [{ iss: issuer.slice(0, -1) }, { aud: "other" }, { iat: undefined }, { exp: undefined },
    { exp: now }, { sub: "" }, { iat: now + 90 }, { nbf: now + 90 }, { exp: now + 800 }]) {
    expect(await federation.verify(await mint(claims))).toBeNull();
  }
  const other = await workloadIssuer();
  try { expect(await federation.verify(await other.mint())).toBeNull(); } finally { await other.close(); }
  expect(await federation.verify(await mint({}, "absent"))).toBeNull();
  expect(await federation.verify(await mint({ extra: "x".repeat(16384) }))).toBeNull();
  expect(await federation.verify("malformed")).toBeNull();

  const skew = new WorkloadFederation(federationConfig.parse({ issuers: [{ ...source.trusted, clockSkewSeconds: 60 }] }));
  expect(await skew.verify(await mint({ iat: now - 300, exp: now - 20 }))).toEqual({ issuer, subject });
  const otherAlgorithm = new WorkloadFederation(federationConfig.parse({ issuers: [{ ...source.trusted, algorithms: ["RS256"] }] }));
  expect(await otherAlgorithm.verify(await mint())).toBeNull();
});

it("loads YAML at startup and refuses ambiguous issuer trust and invalid enrollment rules", async () => {
  const path = join(source.dir, "federation.yaml");
  await writeFile(path, 'issuers:\n  - name: cluster\n    issuer: https://issuer.test\n    audiences: [platform]\n');
  expect(readFederationConfig(path).issuers[0]?.algorithms).toEqual(["RS256"]);
  for (const config of [
    { issuers: [source.trusted, source.trusted] },
    { issuers: [{ ...source.trusted, algorithms: ["HS256"] }] },
    { issuers: [{ ...source.trusted, keys: { jwksFile: "keys", jwksUrl: source.issuer } }] },
    { issuers: [source.trusted], enrollment: [{ issuer: "missing", subjectPattern: "*", maxOrganizations: 2 }] },
  ]) expect(federationConfig.safeParse(config).success).toBe(false);
});

it("coalesces discovery, rotates the projected fetch credential and bounds stale-key use", async () => {
  let requests = 0;
  let expected = "first";
  let servedIssuer = source.issuer;
  let key = source.jwk;
  const server = createServer((req, res) => {
    requests++;
    if (req.headers.authorization !== `Bearer ${expected}`) { res.writeHead(403).end(); return; }
    res.end(JSON.stringify(req.url === "/keys" ? { keys: [key] } : { issuer: servedIssuer, jwks_uri: origin + "/keys" }));
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const origin = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  const path = join(source.dir, "token");
  const clock = vi.spyOn(Date, "now");
  try {
    await writeFile(path, expected);
    const federation = new WorkloadFederation(federationConfig.parse({ issuers: [
      { ...source.trusted, keys: { discoveryUrl: origin + "/discovery", bearerTokenFile: path } },
    ] }));
    const first = await source.mint();
    const second = await source.mint({}, "second");
    const start = Date.now();
    expect(await Promise.all([federation.verify(first), federation.verify(first)])).toEqual([
      { issuer: source.issuer, subject: source.subject }, { issuer: source.issuer, subject: source.subject },
    ]);
    expect(await federation.verify(second)).toBeNull();
    expect(requests).toBe(2);
    clock.mockReturnValue(start + 31_000);
    key = { ...key, kid: "second" };
    expected = "rotated";
    await writeFile(path, expected);
    expect(await federation.verify(second)).toBeTruthy();
    expect(await federation.verify(first)).toBeNull();
    expect(requests).toBe(4);
    servedIssuer = "https://other.example.test/";
    clock.mockReturnValue(start + 2 * 3600_000);
    expect(await federation.verify(second)).toBeTruthy();
    expect(requests).toBe(5);
    clock.mockReturnValue(start + 25 * 3600_000);
    expect(await federation.verify(second)).toBeNull();
  } finally {
    clock.mockRestore();
    server.close();
    await once(server, "close");
  }
});
