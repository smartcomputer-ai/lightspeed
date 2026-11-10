import { describe, expect, it } from "vitest";
import { customHeadersError, isReusableCredential } from "./CredentialsPage";

describe("custom headers credentials", () => {
  it("accepts several headers, including a non-Bearer Authorization", () => {
    expect(customHeadersError([
      { name: "X-Upstream-Url", value: "https://tracker.example.com" },
      { name: "X-Upstream-Token", value: "secret" },
      { name: "Authorization", value: "Token abc" },
    ])).toBeNull();
  });

  it("rejects reserved, duplicate, and malformed headers", () => {
    expect(customHeadersError([])).toContain("at least one");
    expect(customHeadersError([{ name: "Host", value: "x" }])).toContain("reserved");
    expect(customHeadersError([{ name: "Mcp-Session-Id", value: "x" }])).toContain("reserved");
    expect(customHeadersError([{ name: "X-Lightspeed-Universe", value: "x" }])).toContain("reserved");
    expect(customHeadersError([
      { name: "X-Token", value: "a" },
      { name: "x-token", value: "b" },
    ])).toContain("twice");
    expect(customHeadersError([{ name: "X Token", value: "a" }])).toContain("valid header name");
    expect(customHeadersError([{ name: "X-Token", value: "" }])).toContain("needs a value");
    expect(customHeadersError([{ name: "X-Token", value: "a\nb" }])).toContain("printable ASCII");
  });

  it("lists custom headers credentials on the Credentials page", () => {
    expect(isReusableCredential({ providerKind: "staticHeaders", providerId: "static", metadata: {} }))
      .toBe(true);
  });
});
