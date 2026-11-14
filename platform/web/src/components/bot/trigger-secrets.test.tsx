// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { TriggersSection } from "./triggers";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({ useFeature: () => true }));

const TOKEN = "0123456789abcdef0123456789abcdef0123456789abcdef";
const INGEST_PATH = `/hooks/bots/core-universe/ada/github/${TOKEN}`;
const PAIRING_CODE = "PairCode2345";
const base = { botId: "ada", enabled: true, createdAtMs: 0, updatedAtMs: 0 };
// The triggers as the list returns them: no ingest path, no pairing code.
const webhook = { ...base, triggerId: "github", revision: 3, kind: "webhook", verification: { scheme: "token" }, preset: null };
const chat = { ...base, triggerId: "telegram", revision: 7, kind: "chat", accountId: "tg-main", pairing: "code", priority: 100, route: { policy: "perKey", key: null } };
const openChat = { ...chat, triggerId: "open-chat", pairing: "open" };
const schedule = { ...base, triggerId: "nightly", revision: 1, kind: "schedule", cron: "0 3 * * *", timezone: "UTC", summary: "Nightly" };

let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
let chatRevision: number;
let pairingCode: string;

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  chatRevision = chat.revision;
  pairingCode = PAIRING_CODE;
  mocks.api.mockReset().mockImplementation(async (method: string, path: string, body?: { trigger?: { pairingCode?: string } }) => {
    if (method === "GET" && path.endsWith("/triggers")) {
      return { triggers: [webhook, { ...chat, revision: chatRevision }, openChat, schedule] };
    }
    if (method === "GET" && path.endsWith("/triggers/github/secrets")) return { ingestPath: INGEST_PATH };
    if (method === "GET" && path.endsWith("/triggers/telegram/secrets")) return { pairingCode };
    if (method === "PUT" && path.endsWith("/triggers/telegram")) {
      chatRevision += 1;
      pairingCode = body?.trigger?.pairingCode ?? pairingCode;
      return { trigger: { ...chat, revision: chatRevision }, secrets: { pairingCode } };
    }
    if (path.endsWith("/bots")) return { bots: [] };
    if (path.endsWith("/channel-accounts")) return { accounts: [] };
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
  expect(container.textContent).toContain("telegram");
  expect(secretReads()).toEqual([]);
  expect(container.textContent).not.toContain(TOKEN);
  expect(container.textContent).not.toContain(PAIRING_CODE);
  expect(container.querySelector('[aria-label="Copy ingest URL"]')).toBeNull();
  expect(container.querySelector('[aria-label="Copy pairing code"]')).toBeNull();
  expect(container.textContent).toContain("shown to people who manage this bot");
});

it("reads the secrets of webhook and paired chat triggers for a member who manages the bot", async () => {
  await show(true);
  // One read each for the two triggers that have a secret; none for an open
  // chat connection or a schedule.
  expect(secretReads().sort()).toEqual([
    "/api/v1/universes/universe/bots/ada/triggers/github/secrets",
    "/api/v1/universes/universe/bots/ada/triggers/telegram/secrets",
  ]);
  expect(container.textContent).toContain(`${window.location.origin}${INGEST_PATH}`);
  expect(container.textContent).toContain(PAIRING_CODE);
  expect(container.querySelector('[aria-label="Copy ingest URL"]')).not.toBeNull();
  expect(container.querySelector('[aria-label="Copy pairing code"]')).not.toBeNull();
});

it("does not read the secrets again while the trigger list refreshes unchanged", async () => {
  await show(true);
  await act(async () => { await vi.advanceTimersByTimeAsync(11_000); });
  await settle();
  expect(mocks.api.mock.calls.filter(([method, path]) => method === "GET" && String(path).endsWith("/triggers")).length)
    .toBeGreaterThan(1);
  expect(secretReads()).toHaveLength(2);
});

it("names a code only when rotating, then reads the new one", async () => {
  await show(true);
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Rotate pairing code"]')!.click());
  await settle();
  const puts = mocks.api.mock.calls.filter(([method]) => method === "PUT");
  expect(puts).toHaveLength(1);
  const rotated = puts[0]![2].trigger.pairingCode as string;
  expect(rotated).toHaveLength(12);
  expect(rotated).not.toBe(PAIRING_CODE);
  expect(container.textContent).toContain(rotated);
  expect(container.textContent).not.toContain(PAIRING_CODE);
  expect(secretReads().filter((path) => path.endsWith("/telegram/secrets"))).toHaveLength(2);
});

it("leaves the pairing code out when it pauses a chat trigger", async () => {
  await show(true);
  const pause = [...container.querySelectorAll<HTMLButtonElement>('[aria-label="Pause trigger"]')];
  // Cards are in list order: github, telegram, open-chat, nightly.
  await act(async () => pause[1]!.click());
  await settle();
  const [, path, body] = mocks.api.mock.calls.find(([method]) => method === "PUT")!;
  expect(String(path)).toContain("/triggers/telegram");
  expect(body.trigger).toMatchObject({ triggerId: "telegram", kind: "chat", pairing: "code", enabled: false });
  expect("pairingCode" in body.trigger).toBe(false);
});
