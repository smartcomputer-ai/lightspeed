import { describe, expect, it } from "vitest";
import {
  CREDENTIAL_ONLY_HEADER_NAMES,
  MCP_SERVER_HEADER_OWNER,
  MODEL_ENDPOINT_HEADER_OWNER,
  isCredentialOnlyHeader,
  requestHeaderNeedsCredential,
  requestHeaderRows,
  requestHeadersInput,
  requestHeaderSummary,
  type RequestHeaderOwner,
} from "./request-headers";

const row = (key: number, name: string, source: "literal" | "credential", value: string) =>
  ({ key, name, source, value });

const OWNERS: [string, RequestHeaderOwner][] = [
  ["an MCP server", MCP_SERVER_HEADER_OWNER],
  ["a model endpoint", MODEL_ENDPOINT_HEADER_OWNER],
];

describe.each(OWNERS)("request headers of %s", (_label, owner) => {
  it("round-trips headers through editable rows", () => {
    const headers = {
      "x-tenant": { type: "literal" as const, value: "acme" },
      "x-api-key": { type: "credential" as const, grantId: "authgrant_upstream" },
    };
    expect(requestHeadersInput(owner, requestHeaderRows(headers))).toEqual({ headers });
    expect(requestHeaderRows(undefined)).toEqual([]);
    expect(requestHeaderRows(null)).toEqual([]);
  });

  it("drops blank rows and trims names and values", () => {
    expect(requestHeadersInput(owner, [
      row(1, "", "literal", ""),
      row(2, " X-Tenant ", "literal", " acme "),
    ])).toEqual({ headers: { "X-Tenant": { type: "literal", value: "acme" } } });
  });

  it("refuses incomplete and repeated headers", () => {
    expect(requestHeadersInput(owner, [row(1, "", "literal", "value")]).error).toContain("name");
    expect(requestHeadersInput(owner, [row(1, "X-Tenant", "literal", "")]).error).toContain("Enter a value");
    expect(requestHeadersInput(owner, [row(1, "X-Token", "credential", "")]).error)
      .toContain("Choose a credential");
    expect(requestHeadersInput(owner, [
      row(1, "X-Tenant", "literal", "a"),
      row(2, "x-tenant", "literal", "b"),
    ]).error).toContain("more than once");
  });

  it("takes a header that carries a secret only as a credential", () => {
    for (const name of ["api-key", "X-Api-Key"]) {
      expect(requestHeaderNeedsCredential(owner, name)).toBe(true);
      const plain = requestHeadersInput(owner, [row(1, name, "literal", "sk-plain")]);
      expect(plain.error).toContain("carries a secret");
      expect(plain.error).not.toContain("sk-plain");
      expect(requestHeadersInput(owner, [row(1, name, "credential", "authgrant_upstream")])).toEqual({
        headers: { [name]: { type: "credential", grantId: "authgrant_upstream" } },
      });
    }
    expect(requestHeaderNeedsCredential(owner, "x-tenant")).toBe(false);
  });

  it("keeps Host for the transport whatever the value source", () => {
    for (const source of ["literal", "credential"] as const) {
      expect(requestHeadersInput(owner, [row(1, "Host", source, "value")]).error)
        .toContain("set by Lightspeed");
    }
  });
});

describe("what each owner reserves", () => {
  it("sends an MCP server's Authorization through its authentication setting", () => {
    expect(requestHeadersInput(MCP_SERVER_HEADER_OWNER, [row(1, "Authorization", "literal", "Token x")]).error)
      .toContain("Authentication setting");
    for (const name of ["X-Lightspeed-Universe", "Mcp-Session-Id", "Proxy-Authorization"]) {
      expect(requestHeadersInput(MCP_SERVER_HEADER_OWNER, [row(1, name, "credential", "authgrant_1")]).error)
        .toContain("set by Lightspeed");
    }
    // Reserved outright, so the editor does not steer it to a credential.
    expect(requestHeaderNeedsCredential(MCP_SERVER_HEADER_OWNER, "Proxy-Authorization")).toBe(false);
    expect(requestHeadersInput(MCP_SERVER_HEADER_OWNER, [row(1, "Cookie", "literal", "a=b")]).error)
      .toBeUndefined();
  });

  it("sends a model endpoint's Authorization as the provider's API key", () => {
    expect(
      requestHeadersInput(MODEL_ENDPOINT_HEADER_OWNER, [row(1, "Authorization", "credential", "authgrant_1")]).error,
    ).toContain("API key");
    expect(requestHeadersInput(MODEL_ENDPOINT_HEADER_OWNER, [row(1, "Cookie", "literal", "a=b")]).error)
      .toContain("set by Lightspeed");
    // A model endpoint may sit behind a proxy that takes its own credential.
    expect(requestHeaderNeedsCredential(MODEL_ENDPOINT_HEADER_OWNER, "Proxy-Authorization")).toBe(true);
    expect(
      requestHeadersInput(MODEL_ENDPOINT_HEADER_OWNER, [row(1, "Proxy-Authorization", "literal", "Basic abc")]).error,
    ).toContain("carries a secret");
    expect(
      requestHeadersInput(MODEL_ENDPOINT_HEADER_OWNER, [row(1, "Mcp-Session-Id", "literal", "s")]).error,
    ).toBeUndefined();
  });
});

describe("shared header rules", () => {
  it("names the headers that carry a secret in any spelling", () => {
    expect([...CREDENTIAL_ONLY_HEADER_NAMES].sort()).toEqual(["api-key", "proxy-authorization", "x-api-key"]);
    for (const name of ["api-key", " X-Api-Key ", "PROXY-AUTHORIZATION"]) {
      expect(isCredentialOnlyHeader(name)).toBe(true);
    }
    expect(isCredentialOnlyHeader("x-title")).toBe(false);
  });

  it("summarises a credential header by its grant, never a value", () => {
    expect(requestHeaderSummary({ type: "literal", value: "Lightspeed" })).toBe("Lightspeed");
    expect(requestHeaderSummary({ type: "credential", grantId: "authgrant_router" }))
      .toBe("credential authgrant_router");
  });
});
