// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { MetaToolbar } from "@/components/meta-toolbar";
import { sessionResources } from "@session-ui/model/session-resources";
import { sessionResourceItems } from "./session-resources";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));

let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mocks.api.mockReset().mockResolvedValue([
    { profileId: "repo-explorer", displayName: "Repo explorer", description: "Answers repository questions with file:line citations.", revision: 1, updatedAtMs: 0 },
    { profileId: "test-writer", revision: 1, updatedAtMs: 0 },
  ]);
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

it("names and describes the sub-agent profiles, reading them only once their popover opens", async () => {
  const resources = sessionResources({ features: { subagents: { agents: [{ profileId: "repo-explorer" }, { profileId: "test-writer" }] } } }, null);
  const items = sessionResourceItems({ resources, handle: "demo", universeId: "universe" });
  await act(async () => root.render(
    <QueryClientProvider client={client}><MemoryRouter>
      <MetaToolbar items={items} place="strip" />
    </MemoryRouter></QueryClientProvider>,
  ));
  expect(mocks.api).not.toHaveBeenCalled();

  await act(async () => container.querySelector<HTMLButtonElement>('[data-meta-item="resource:agents"]')!.click());
  await act(async () => { await Promise.resolve(); });
  expect(mocks.api).toHaveBeenCalledWith("GET", "/api/ui/universes/universe/profiles");
  const rows = [...document.body.querySelectorAll<HTMLAnchorElement>('ul[aria-label="Sub-agent profiles"] a')];
  expect(rows.map((row) => row.getAttribute("href"))).toEqual(["/demo/profiles/repo-explorer", "/demo/profiles/test-writer"].map((path) => `/u${path}`));
  // A profile with a name shows it and its description; one without, its id.
  expect(rows[0]!.textContent).toBe("Repo explorerAnswers repository questions with file:line citations.");
  expect(rows[1]!.textContent).toBe("test-writer");
});
