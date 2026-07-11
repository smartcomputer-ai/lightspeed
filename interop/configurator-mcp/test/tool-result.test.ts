import {
  LightspeedRpcError,
  LightspeedTransportError,
} from "@lightspeed/agent-client";
import { describe, expect, it } from "vitest";
import { failedToolResult, safeError } from "../src/tool-result.js";

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
    expect(result.structuredContent).toMatchObject({
      error: { code: -32010, kind: "rejected" },
    });
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
