import {
  LightspeedRpcError,
  LightspeedTransportError,
} from "@lightspeed-ai/sdk";
import type { CallToolResult } from "@modelcontextprotocol/sdk/types.js";

// Exactly one representation of every result: the JSON text block that all
// MCP clients read. No tool declares an outputSchema, so structuredContent
// would be a second copy of the same bytes — the duplication that once
// doubled a large session read — and content-only clients would see an
// empty result if the text block were dropped instead.
export function successfulToolResult(outcome: unknown): CallToolResult {
  return {
    content: [{ type: "text", text: JSON.stringify(outcome) }],
  };
}

export function failedToolResult(error: unknown): CallToolResult {
  const safe = safeError(error);
  return {
    content: [{ type: "text", text: JSON.stringify(safe) }],
    isError: true,
  };
}

export function safeError(error: unknown): Record<string, unknown> {
  if (error instanceof LightspeedRpcError) {
    const result: Record<string, unknown> = {
      type: "lightspeedRpc",
      code: error.code,
      kind: error.data?.kind ?? error.kind,
      message: redact(error.message),
    };
    if (error.data !== null && error.data !== undefined) {
      result.data = { ...error.data, message: redact(error.data.message) };
    }
    return result;
  }
  if (error instanceof LightspeedTransportError) {
    return {
      type: "lightspeedTransport",
      message: redact(error.message),
      ...(error.status === undefined ? {} : { status: error.status }),
    };
  }
  if (isAbortError(error)) {
    return { type: "aborted", message: "request aborted" };
  }
  return {
    type: "internal",
    message: "unexpected internal error",
  };
}

function redact(value: string): string {
  return value
    .replace(/\blsk_[A-Za-z0-9._~-]+/g, "[REDACTED]")
    .replace(/Bearer\s+\S+/gi, "Bearer [REDACTED]");
}

function isAbortError(error: unknown): boolean {
  return (
    (error instanceof DOMException && error.name === "AbortError") ||
    (error instanceof Error && /abort/i.test(error.message))
  );
}
