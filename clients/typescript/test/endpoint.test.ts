import { readFile } from "node:fs/promises";
import { expect, it } from "vitest";
import { LightspeedClient, normalizeEndpoint, prepareHttpRequest } from "../src/index.js";
const fixtures = JSON.parse(await readFile(new URL("../../fixtures/endpoints.json", import.meta.url), "utf8")) as { accepted: { input: string; normalized: string }[]; rejected: string[] };
it.each(fixtures.accepted)("normalizes $input consistently for runtime and native calls", async ({ input, normalized }) => {
  expect(normalizeEndpoint(input)).toBe(normalized);
  expect(prepareHttpRequest("profiles/list", {}, { endpoint: input, universe: "u" }).url.href).toBe(`${normalized}/universes/u/profiles`);
  const calls: string[] = [];
  const client = new LightspeedClient({ endpoint: input, fetch: async url => { calls.push(String(url)); return Response.json({ accounts: [] }); } });
  await client.platform.listAccounts();
  expect(calls).toEqual([`${normalized}/platform/accounts`]);
});
it.each(fixtures.rejected)("refuses ambiguous or credential-bearing endpoint %s", input => {
  expect(() => new LightspeedClient(input)).toThrow();
  expect(() => prepareHttpRequest("initialize", {}, { endpoint: input })).toThrow();
});
