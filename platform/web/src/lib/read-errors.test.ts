import { expect, it } from "vitest";
import { ApiError } from "@/api";
import { isTransientReadError, retryRead } from "./read-errors";

it.each([new TypeError("Failed to fetch"), new DOMException("Timed out", "TimeoutError"),
  ...[408, 429, 500, 502, 503].map((status) => new ApiError(status, null)),
])("keeps one retry for transient read failure $message", (error) => {
  expect(isTransientReadError(error)).toBe(true);
  expect(retryRead(0, error)).toBe(true);
  expect(retryRead(1, error)).toBe(false);
});

it.each([new SyntaxError("Invalid JSON"), new Error("Invalid event sequence"),
  ...[400, 401, 403, 404].map((status) => new ApiError(status, null)),
])("does not delay actionable read failure $message", (error) => {
  expect(isTransientReadError(error)).toBe(false);
  expect(retryRead(0, error)).toBe(false);
});
