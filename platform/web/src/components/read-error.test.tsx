// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ApiError } from "@/api";
import { ReadError } from "./read-error";

let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  container = document.createElement("div");
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  vi.useRealTimers();
  vi.unstubAllGlobals();
});
async function render(error: unknown, options: { retrying?: boolean; loading?: boolean; graceMs?: number } = {}) {
  await act(async () => root.render(<ReadError error={error} {...options} />));
}
async function advance(ms: number) {
  await act(async () => vi.advanceTimersByTime(ms));
}

it("waits ten seconds for a transcript retry notice, without restarting for repeated failures", async () => {
  await render(new TypeError("Failed to fetch"), { retrying: true, graceMs: 10_000 });
  expect(container.textContent).toBe("");
  await advance(9_000);
  await render(new TypeError("Load failed"), { retrying: true, graceMs: 10_000 });
  await advance(999);
  expect(container.textContent).toBe("");
  await advance(1);
  expect(container.textContent).toBe("Connection lost — retrying…");
  expect(container.querySelector('[role="status"]')?.className).toContain("text-muted-foreground");
  expect(container.querySelector(".text-destructive")).toBeNull();
  await render(null);
  expect(container.textContent).toBe("");
});

it("never flashes an error for a brief interruption and starts a new grace period for the next one", async () => {
  await render(new TypeError("Offline"));
  await advance(1_000);
  await render(null);
  await advance(3_000);
  expect(container.textContent).toBe("");
  await render(new TypeError("Offline again"));
  expect(container.textContent).toBe("");
  await advance(3_000);
  expect(container.textContent).toBe("Connection unavailable.");
});

it("keeps initial loading visible during the grace period without promising more list retries", async () => {
  await render(new ApiError(503, { error: "Unavailable" }), { loading: true });
  expect(container.textContent).toBe("Loading…");
  await advance(3_000);
  expect(container.textContent).toBe("Connection unavailable.");
  expect(container.textContent).not.toContain("retrying");
});

it.each([
  new ApiError(401, { error: "Sign in required" }),
  new ApiError(403, { error: "Permission denied" }),
  new ApiError(400, { error: "Invalid request" }),
  new SyntaxError("Invalid response JSON"),
  new Error("The session event stream is not contiguous"),
])("shows actionable errors immediately: $message", async (error) => {
  await render(new TypeError("Offline"));
  await render(error);
  expect(container.querySelector('[role="alert"]')?.textContent).toBe(error.message);
  expect(container.querySelector('[role="alert"]')?.className).toContain("text-destructive");
});
