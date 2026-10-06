// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { SessionView } from "@/api";
import { SessionSettingsDialog } from "./session-settings-sheet";

const mocks = vi.hoisted(() => ({ api: vi.fn(), configure: false }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/permissions", () => ({ useActionPermissions: () => ({ can: (action: string) => action === "configure_session" && mocks.configure }) }));
vi.mock("@/lib/sessions/editor-options", () => ({ useSessionConfigEditorOptions: () => ({}) }));
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mocks.configure = false;
  mocks.api.mockReset().mockImplementation(async (_method: string, path: string) => path.endsWith("/instructions") ? { text: "Saved instructions", active: [] } : []);
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  client.clear();
  container.remove();
  vi.unstubAllGlobals();
});
const session = {
  id: "session", configRevision: 1,
  config: { model: { providerId: "openai", apiKind: "openai:responses", model: "saved-model" } },
  metadata: { purpose: "research" }, retention: { rootSessionId: "session" },
} as unknown as SessionView;
async function show() {
  await act(async () => root.render(<QueryClientProvider client={client}>
    <SessionSettingsDialog universeId="universe" sessionId="session" session={session} runActive={false} open onOpenChange={() => {}} />
  </QueryClientProvider>));
  await act(async () => { await new Promise((resolve) => setTimeout(resolve, 20)); });
  return document.body.querySelector<HTMLElement>('[role="dialog"]')!;
}
it("shows configuration and instructions without edit or apply controls to readers", async () => {
  const dialog = await show();
  expect(dialog.querySelector("textarea")?.readOnly).toBe(true);
  expect(dialog.querySelector("textarea")?.value).toBe("Saved instructions");
  expect([...dialog.querySelectorAll<HTMLInputElement>("input")].find((input) => input.value === "saved-model")?.readOnly).toBe(true);
  expect(dialog.textContent).not.toContain("Apply setup");
  const metadata = [...dialog.querySelectorAll<HTMLButtonElement>("button")].find((button) => button.textContent?.includes("Session metadata"))!;
  await act(async () => metadata.click());
  expect(dialog.querySelector<HTMLInputElement>('[aria-label="Metadata value 1"]')?.disabled).toBe(true);
  expect(mocks.api.mock.calls.every(([method]) => method === "GET")).toBe(true);
});
it("keeps session settings editable for operators", async () => {
  mocks.configure = true;
  const dialog = await show();
  expect(dialog.querySelector("textarea")?.readOnly).toBe(false);
  expect(dialog.textContent).toContain("Apply setup");
});
