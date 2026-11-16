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
    const input = { authorization: "Bearer lsr_service", "x-lightspeed-universe": universe, "x-lightspeed-actor": actor };
    const auth = authenticateHeaders("authenticated", input);
    expect(auth).toEqual({ mode: "authenticated", token: "lsr_service", universeId: universe, actor });
    const headers = new Headers(upstreamHeaders(auth));
    expect(headers.get("authorization")).toBe(input.authorization);
    expect(headers.get("x-lightspeed-actor")).toBe(actor);
    expect(headers.has("x-lightspeed-universe")).toBe(false);
    expect(() => authenticateHeaders("authenticated", { "x-lightspeed-universe": universe })).toThrow(/missing required/);
    expect(() => authenticateHeaders("authenticated", { ...input, "x-lightspeed-actor": "a".repeat(257) })).toThrow();
    expect(() => authenticateHeaders("authenticated", { ...input, "x-lightspeed-principal": "user:old" })).toThrow();
  });
  it("rejects malformed and duplicate credentials", () => {
    for (const authorization of ["Basic other", "Bearer bad\u0001token", ["Bearer lsr_a", "Bearer lsr_b"]]) {
      expect(() => authenticateHeaders("authenticated", { authorization } as IncomingHttpHeaders)).toThrow();
    }
    expect(authenticateHeaders("authenticated", { authorization: "Bearer lsr_only" })).toEqual({ mode:"authenticated",token:"lsr_only" });
  });
});
