// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { TriggersSection } from "./triggers";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));

const TOKEN = "0123456789abcdef0123456789abcdef0123456789abcdef";
const INGEST_PATH = `/ingress/webhooks/bots/core-universe/ada/github/${TOKEN}`;
const base = { botId: "ada", enabled: true, createdAtMs: 0, updatedAtMs: 0 };
// The triggers as the list returns them: the ingest path is separate.
const webhook = { ...base, triggerId: "github", revision: 3, kind: "webhook", verification: { scheme: "token" }, preset: null };
const schedule = { ...base, triggerId: "nightly", revision: 1, kind: "schedule", cron: "0 3 * * *", timezone: "UTC", summary: "Nightly" };

let root: Root;
let container: HTMLDivElement;
let client: QueryClient;

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mocks.api.mockReset().mockImplementation(async (method: string, path: string) => {
    if (method === "GET" && path.endsWith("/triggers")) {
      return { triggers: [webhook, schedule] };
    }
    if (method === "GET" && path.endsWith("/triggers/github/secrets")) return { ingestPath: INGEST_PATH };
    if (path.endsWith("/bots")) return { bots: [] };
    throw new Error(`Unexpected request: ${method} ${path}`);
  });
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  client.clear();
  container.remove();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

async function settle() {
  for (let turn = 0; turn < 5; turn += 1) {
    await act(async () => { await vi.advanceTimersByTimeAsync(10); });
  }
}

async function show(manage: boolean) {
  await act(async () => root.render(
    <QueryClientProvider client={client}>
      <TriggersSection universeId="universe" botId="ada" manage={manage} env={{ kind: "unknown" }} />
    </QueryClientProvider>,
  ));
  await settle();
}

const secretReads = () => mocks.api.mock.calls
  .filter(([method, path]) => method === "GET" && String(path).endsWith("/secrets"))
  .map(([, path]) => String(path));

it("shows a member who cannot manage the bot no secret and never asks for one", async () => {
  await show(false);
  expect(container.textContent).toContain("github");
  expect(secretReads()).toEqual([]);
  expect(container.textContent).not.toContain(TOKEN);
  expect(container.querySelector('[aria-label="Copy ingest URL"]')).toBeNull();
  expect(container.textContent).toContain("shown to people who manage this bot");
});

it("reads the webhook ingest path for a member who manages the bot", async () => {
  await show(true);
  expect(secretReads().sort()).toEqual([
    "/api/ui/universes/universe/modules/bots/ada/triggers/github/secrets",
  ]);
  expect(container.textContent).toContain(`${window.location.origin}${INGEST_PATH}`);
  expect(container.querySelector('[aria-label="Copy ingest URL"]')).not.toBeNull();
});

it("does not read the secrets again while the trigger list refreshes unchanged", async () => {
  await show(true);
  await act(async () => { await vi.advanceTimersByTimeAsync(11_000); });
  await settle();
  expect(mocks.api.mock.calls.filter(([method, path]) => method === "GET" && String(path).endsWith("/triggers")).length)
    .toBeGreaterThan(1);
  expect(secretReads()).toHaveLength(1);
});
