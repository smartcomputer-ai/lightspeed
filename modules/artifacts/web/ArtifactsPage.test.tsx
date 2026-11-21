// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { notifyManager, QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { Hono } from "hono";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { DemoStore } from "@/demo/store";
import { workspaceRoutes } from "@/demo/routes/workspaces";
import { waitForUi } from "@/test/wait-for-ui";
import { ArtifactsPage } from "./ArtifactsPage";

const access = vi.hoisted(() => ({ write: true }));
vi.mock("@/lib/permissions", () => ({
  useActionPermissions: () => ({ can: (action: string) => action === "read" || access.write }),
}));
vi.mock("@/lib/universes", () => ({
  useActiveUniverse: () => ({ universe: { id: "test-universe", handle: "test" } }),
}));

const workspaces = "/api/ui/universes/test-universe/workspaces";
let store: DemoStore;
let server: Hono;
let client: QueryClient;
let root: Root;
let container: HTMLDivElement;

async function post(path: string, body: unknown) {
  const response = await server.request(path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  expect(response.ok).toBe(true);
}

beforeEach(async () => {
  access.write = true;
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  notifyManager.setNotifyFunction((notify) => act(notify));
  store = new DemoStore({
    id: "author",
    name: "Author",
    email: "author@example.org",
    role: "user",
    emailVerified: true,
    banned: false,
    image: null,
    createdAt: "",
    updatedAt: "",
  });
  store.addUniverse({ id: "test-universe", handle: "test", name: "Test" });
  server = new Hono().route("/api/ui/universes", workspaceRoutes(store));
  vi.stubGlobal("fetch", (path: string, init: RequestInit) => server.request(path, init));
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await post(workspaces, { workspaceId: "designs", displayName: "Designs" });
});

afterEach(async () => {
  await act(async () => root.unmount());
  client.clear();
  container.remove();
  notifyManager.setNotifyFunction((notify) => notify());
  vi.unstubAllGlobals();
});

async function open(workspaceId = "designs") {
  await act(async () =>
    root.render(
      <QueryClientProvider client={client}>
        <MemoryRouter initialEntries={[`/u/test/artifacts?workspace=${workspaceId}`]}>
          <ArtifactsPage />
        </MemoryRouter>
      </QueryClientProvider>,
    ),
  );
}

const entry = (path: string, text: string) => ({
  kind: "file",
  path,
  contentBase64: btoa(text),
  mediaType: "text/plain",
});

it("shows only files under artifacts/ and links readers to each saved blob", async () => {
  access.write = false;
  const name = "résumé #1?.txt";
  await post(`${workspaces}/designs/upload`, {
    expectedRevision: 0,
    replace: false,
    entries: [entry(`artifacts/drafts/${name}`, "draft"), entry("internal.txt", "not published")],
  });
  await open();
  const link = await waitForUi(() => {
    const anchor = container.querySelector("li a");
    expect(anchor?.textContent).toBe(`drafts/${name}`);
    return new URL(anchor!.getAttribute("href")!, "https://example.org");
  });
  expect(link.pathname).toMatch(/^\/u\/test\/blobs\/[0-9a-f]{64}$/);
  expect(link.searchParams.get("workspace")).toBe("designs");
  expect(link.searchParams.get("path")).toBe(`artifacts/drafts/${name}`);
  expect(container.textContent).not.toContain("internal.txt");
  expect(container.querySelector("input[type=file]")).toBeNull();
});

it("retains the selection after a stale write and publishes against the refreshed revision", async () => {
  await open();
  await waitForUi(() => expect(container.textContent).toContain("No published artifacts"));
  const input = container.querySelector<HTMLInputElement>("input[type=file]")!;
  const selected = new File(["v1"], "release.md", { type: "text/markdown" });
  Object.defineProperty(input, "files", { configurable: true, value: [selected] });
  await act(async () => input.dispatchEvent(new Event("change", { bubbles: true })));
  const button = container.querySelector<HTMLButtonElement>("button[type=submit]")!;

  await post(`${workspaces}/designs/upload`, {
    expectedRevision: 0,
    replace: false,
    entries: [entry("concurrent.txt", "another user's edit")],
  });
  await act(async () => button.click());
  await waitForUi(() => {
    expect(container.querySelector("[role=alert]")?.textContent).toMatch(/changed/i);
    expect(button.disabled).toBe(false);
  });
  await act(async () => button.click());
  await waitForUi(() => expect(container.querySelector("li a")?.textContent).toBe("release.md"));

  const saved = store.universe("test-universe")!.workspaces.get("designs")!;
  expect(saved.row.revision).toBe(2);
  expect(saved.manifest.root.entries["concurrent.txt"]).toBeDefined();
  const directory = saved.manifest.root.entries.artifacts;
  const file = directory?.kind === "directory" && directory.entries["release.md"];
  expect(file && file.kind === "file" && store.readText(file.blob_ref)).toBe("v1");
});

it("never substitutes another workspace for an unavailable deep link", async () => {
  await open("missing");
  await waitForUi(() => expect(container.textContent).toContain("This workspace is unavailable"));
  expect(container.querySelector("input[type=file]")).toBeNull();
});
