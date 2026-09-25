import type { IncomingHttpHeaders } from "node:http";
import { describe, expect, it } from "vitest";
import { authenticateHeaders, upstreamHeaders } from "../src/request-auth.js";
const universe = "00000000-0000-4000-8000-000000000001";
const actor = "platform:user:00000000-0000-4000-8000-000000000002";
describe("request authentication", () => {
  it("keeps local development explicit", () => {
    expect(authenticateHeaders("single", {})).toEqual({ mode: "single" });
    for (const name of ["authorization", "x-lightspeed-universe", "x-lightspeed-actor", "x-lightspeed-principal"]) {
      expect(() => authenticateHeaders("single", { [name]: "claim" })).toThrow();
    }
  });
  it("requires a bearer before forwarding selectors and opaque actor assertions", () => {
    const input = { authorization: "Bearer lsk_service", "x-lightspeed-universe": universe, "x-lightspeed-actor": actor };
    const auth = authenticateHeaders("authenticated", input);
    expect(auth).toEqual({ mode: "authenticated", apiKey: "lsk_service", universeId: universe, actor });
    const headers = new Headers(upstreamHeaders(auth));
    for (const [name,value] of Object.entries(input)) expect(headers.get(name)).toBe(value);
    expect(() => authenticateHeaders("authenticated", { "x-lightspeed-universe": universe })).toThrow(/missing required/);
    expect(() => authenticateHeaders("authenticated", { ...input, "x-lightspeed-actor": "a".repeat(257) })).toThrow();
    expect(() => authenticateHeaders("authenticated", { ...input, "x-lightspeed-principal": "user:old" })).toThrow();
  });
  it("rejects malformed and duplicate credentials", () => {
    for (const authorization of ["Bearer other", "Bearer lsk_with spaces", ["Bearer lsk_a", "Bearer lsk_b"]]) {
      expect(() => authenticateHeaders("authenticated", { authorization } as IncomingHttpHeaders)).toThrow();
    }
    expect(authenticateHeaders("authenticated", { authorization: "Bearer lsk_only" })).toEqual({ mode:"authenticated",apiKey:"lsk_only" });
  });
});
