// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ApiKeysCard } from "./api-keys-card";
import { waitForUi } from "@/test/wait-for-ui";
const api = vi.hoisted(() => vi.fn());
vi.mock("@/lib/platform-api", () => ({ platformApi: api }));
let root: Root; let container: HTMLDivElement; let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  api.mockReset().mockImplementation(async operation => operation.endsWith("/list") ? { credentials: [] } : { secret: "one-time-secret", credential: { id: "key" } });
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  container = document.createElement("div"); document.body.append(container); root = createRoot(container);
});
afterEach(async () => { await act(async () => root.unmount()); client.clear(); container.remove(); vi.unstubAllGlobals(); });
async function show(application?: { accountId: string; applicationId: string }) { await act(async () => root.render(<QueryClientProvider client={client}><ApiKeysCard application={application} /></QueryClientProvider>)); }
async function type(label: string, value: string) { const input = container.querySelector<HTMLInputElement>(`[aria-label="${label}"]`)!; await act(async () => { Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, value); input.dispatchEvent(new Event("input", { bubbles: true })); }); }
it("uses deployment expiry defaults and clears the one-time secret when dismissed", async () => {
  await show(); await type("API key name", "Local automation");
  await act(async () => container.querySelector<HTMLButtonElement>('button[type="submit"]')!.click());
  await waitForUi(() => expect(container.textContent).toContain("one-time-secret"));
  expect(api).toHaveBeenCalledWith("platform/credentials/create", { name: "Local automation", restrictions: { readOnly: false, allowUniverseCreation: false } });
  expect(container.querySelector<HTMLButtonElement>('button[type="submit"]')?.disabled ?? [...container.querySelectorAll("button")].find(b => b.textContent === "Create API key")?.disabled).toBe(true);
  await act(async () => [...container.querySelectorAll("button")].find(b => b.textContent === "I have saved it")!.click());
  expect(container.textContent).not.toContain("one-time-secret");
  expect(JSON.stringify(client.getQueryData(["api-keys", "personal"]))).not.toContain("one-time-secret");
});
it("creates an application credential with scoped restrictions and explicit expiry", async () => {
  await show({ accountId: "team", applicationId: "robot" });
  await type("API key name", "CI"); await type("API key expiry in days", "14"); await type("Allowed universe UUIDs", "00000000-0000-4000-8000-000000000001"); await type("Allowed operations", "profiles/list, profiles/read");
  await act(async () => container.querySelector<HTMLButtonElement>('button[type="submit"]')!.click());
  await waitForUi(() => expect(api).toHaveBeenCalledWith("platform/applications/credentials/create", { accountId: "team", applicationId: "robot", name: "CI", expiresInDays: 14, restrictions: { universeIds: ["00000000-0000-4000-8000-000000000001"], operations: ["profiles/list", "profiles/read"], readOnly: false, allowUniverseCreation: false } }));
});
