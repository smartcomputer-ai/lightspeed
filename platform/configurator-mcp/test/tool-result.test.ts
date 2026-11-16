import {
  LightspeedApiError,
  LightspeedTransportError,
} from "@lightspeed-ai/sdk";
import { describe, expect, it } from "vitest";
import {
  failedToolResult,
  safeError,
  successfulToolResult,
} from "../src/tool-result.js";

describe("single-representation MCP tool results", () => {
  it("emits a successful result exactly once, as one text content block", () => {
    const outcome = { session: { id: "session_1", runs: [] } };
    const result = successfulToolResult(outcome);
    expect(result.content).toHaveLength(1);
    expect(result.content[0]).toMatchObject({ type: "text" });
    expect(
      JSON.parse((result.content[0] as unknown as { text: string }).text),
    ).toEqual(outcome);
    // The duplication that once doubled a large session read: the same
    // outcome must not also travel as structuredContent.
    expect(result.structuredContent).toBeUndefined();
    expect(result.isError).toBeUndefined();
  });

  it("keeps the serialized result linear in the outcome size", () => {
    const big = { text: "x".repeat(100_000) };
    const encoded = JSON.stringify(successfulToolResult(big));
    expect(encoded.length).toBeLessThan(2 * JSON.stringify(big).length);
  });
});

describe("safe MCP tool errors", () => {
  it("preserves typed Lightspeed HTTP facts while redacting credentials", () => {
    const error = new LightspeedApiError({
      type: "about:blank", title: "Rejected", status: 409, kind: "rejected",
      detail: "rejected Bearer lsr_secret_value",
    });
    const result = failedToolResult(error);
    expect(result.isError).toBe(true);
    expect(JSON.stringify(result)).not.toContain("lsr_secret_value");
    expect(result.structuredContent).toBeUndefined();
    expect(
      JSON.parse((result.content[0] as { text: string }).text),
    ).toMatchObject({ status: 409, kind: "rejected" });
  });

  it("redacts arbitrary bearer tokens without requiring a generated prefix", () => {
    const token = "configured-runtime-secret-without-prefix";
    const error = new LightspeedApiError({
      type: "about:blank", title: "Rejected", status: 409, kind: "rejected",
      detail: `rejected Bearer ${token}`,
    });
    const result = safeError(error);
    expect(JSON.stringify(result)).not.toContain(token);
    expect(result.message).toBe("rejected Bearer [REDACTED]");
  });

  it("does not expose upstream response bodies or unexpected error messages", () => {
    const transport = new LightspeedTransportError("HTTP 500", {
      status: 500,
      body: { secret: "do-not-return" },
    });
    expect(JSON.stringify(safeError(transport))).not.toContain("do-not-return");
    expect(safeError(new Error("lsr_hidden"))).toEqual({
      type: "internal",
      message: "unexpected internal error",
    });
  });
});
