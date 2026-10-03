import {
  LightspeedRpcError,
  LightspeedTransportError,
} from "@lightspeed-ai/client";
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
  it("preserves typed Lightspeed RPC facts while redacting credentials", () => {
    const error = new LightspeedRpcError({
      code: -32010,
      message: "rejected Bearer lsk_secret_value",
      data: { kind: "rejected", message: "key lsk_secret_value was rejected" },
    });
    const result = failedToolResult(error);
    expect(result.isError).toBe(true);
    expect(JSON.stringify(result)).not.toContain("lsk_secret_value");
    expect(result.structuredContent).toBeUndefined();
    expect(
      JSON.parse((result.content[0] as { text: string }).text),
    ).toMatchObject({ code: -32010, kind: "rejected" });
  });

  it("does not expose upstream response bodies or unexpected error messages", () => {
    const transport = new LightspeedTransportError("HTTP 500", {
      status: 500,
      body: { secret: "do-not-return" },
    });
    expect(JSON.stringify(safeError(transport))).not.toContain("do-not-return");
    expect(safeError(new Error("lsk_hidden"))).toEqual({
      type: "internal",
      message: "unexpected internal error",
    });
  });
});
