import { describe, expect, it } from "vitest";
import {
  authenticateHeaders,
  HttpAuthError,
  upstreamHeaders,
} from "../src/request-auth.js";

describe("request authentication", () => {
  it("keeps single mode header-free and rejects identity smuggling", () => {
    expect(authenticateHeaders("single", {})).toEqual({ mode: "single" });
    expect(upstreamHeaders({ mode: "single" })).toBeUndefined();
    expect(() =>
      authenticateHeaders("single", { "x-lightspeed-universe": universeA }),
    ).toThrow(HttpAuthError);
    expect(() => authenticateHeaders("single", { authorization: "Bearer lsk_a" })).toThrow(
      /not accepted/,
    );
  });

  it("requires and forwards the trusted universe and principal", () => {
    const auth = authenticateHeaders("trusted-header", {
      "x-lightspeed-universe": universeA,
      "x-lightspeed-principal": "service_account:configurator",
    });
    expect(auth).toEqual({
      mode: "trusted-header",
      universeId: universeA,
      principal: "service_account:configurator",
    });
    const headers = new Headers(upstreamHeaders(auth));
    expect(headers.get("x-lightspeed-universe")).toBe(universeA);
    expect(headers.get("x-lightspeed-principal")).toBe("service_account:configurator");
    expect(() => authenticateHeaders("trusted-header", {})).toThrow(/missing required/);
    expect(() =>
      authenticateHeaders("trusted-header", {
        "x-lightspeed-universe": universeA,
        authorization: "Bearer lsk_a",
      }),
    ).toThrow(/not accepted/);
  });

  it("requires an lsk bearer and rejects tenant headers in api-key mode", () => {
    const auth = authenticateHeaders("api-key", { authorization: "Bearer lsk_secret" });
    expect(auth).toEqual({ mode: "api-key", apiKey: "lsk_secret" });
    expect(new Headers(upstreamHeaders(auth)).get("authorization")).toBe("Bearer lsk_secret");
    expect(() => authenticateHeaders("api-key", {})).toThrow(/missing required/);
    expect(() => authenticateHeaders("api-key", { authorization: "Bearer other" })).toThrow(
      /Lightspeed bearer/,
    );
    expect(() =>
      authenticateHeaders("api-key", {
        authorization: "Bearer lsk_secret",
        "x-lightspeed-universe": universeA,
      }),
    ).toThrow(/not accepted/);
  });
});

const universeA = "6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f";
