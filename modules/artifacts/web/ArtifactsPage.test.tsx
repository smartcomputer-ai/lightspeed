// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { notifyManager, QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Routes, Route } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { DemoStore } from "@/demo/store";
import { createDemoRouter } from "@/demo/router";
import { waitForUi } from "@/test/wait-for-ui";
import { artifacts } from "../module";
import artifactRoutes from "../demo";
import { ArtifactPage } from "./ArtifactPage";
import { ArtifactsPage } from "./ArtifactsPage";
import { artifactsApi } from "./queries";

vi.mock("@/lib/universes", () => ({
  useActiveUniverse: () => ({ universe: { id: "test-universe", handle: "test" } }),
}));

const collection = artifactsApi("test-universe");
const workspace = "/api/ui/universes/test-universe/workspaces";
let store: DemoStore;
let server: ReturnType<typeof createDemoRouter>;
let client: QueryClient;
let root: Root;
let container: HTMLDivElement;
let reads: string[];

function post(url: string, body: object) {
  return server.request(url, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
}
const input = { workspaceId: "designs", title: "Release notes", path: "notes.md", expectedRevision: 1 };

beforeEach(async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  notifyManager.setNotifyFunction(notify => act(notify));
  store = new DemoStore({ id: "author", name: "Author", email: "author@example.org", role: "user",
    emailVerified: true, banned: false, image: null, createdAt: "", updatedAt: "" });
  store.addUniverse({ id: "test-universe", handle: "test", name: "Test" });
  server = createDemoRouter(store, [{ ...artifacts, routes: artifactRoutes }]);
  reads = [];
  vi.stubGlobal("fetch", (path: string, init: RequestInit) => { reads.push(path); return server.request(path, init); });
  client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  expect((await post(workspace, { workspaceId: "designs" })).ok).toBe(true);
  expect((await post(workspace + "/designs/upload", { expectedRevision: 0, replace: false,
    entries: [{ kind: "file", path: input.path, contentBase64: btoa("Original bytes"), mediaType: "text/plain" }] })).ok).toBe(true);
});

afterEach(async () => {
  await act(async () => root.unmount());
  client.clear();
  container.remove();
  notifyManager.setNotifyFunction(notify => notify());
  vi.unstubAllGlobals();
});

async function open(url: string) {
  await act(async () => root.render(<QueryClientProvider client={client}><MemoryRouter initialEntries={[url]}>
    <Routes>
      <Route path="/u/test/artifacts" element={<ArtifactsPage />} />
      <Route path="/u/test/artifacts/:artifactId" element={<ArtifactPage />} />
    </Routes>
  </MemoryRouter></QueryClientProvider>));
}

it("browses published snapshots as a viewer without opening the source workspace", async () => {
  expect((await post(collection, input)).status).toBe(201);
  const universe = store.universe("test-universe")!;
  universe.universe.role = "viewer";
  universe.universe.organizationRole = "member";
  universe.workspaces.clear();
  store.blobs.clear();
  await open("/u/test/artifacts?workspace=designs");
  const link = await waitForUi(() => {
    const link = container.querySelector("li a");
    expect(link?.textContent).toBe("Release notes");
    return link as HTMLAnchorElement;
  });
  expect(container.textContent).not.toContain("Publish artifact");
  expect(container.textContent).not.toContain("Retract");
  await act(async () => link.click());
  await waitForUi(() => expect(container.querySelector("pre")?.textContent).toBe("Original bytes"));
  expect(reads.every(url => url.startsWith(collection))).toBe(true);
});

it("does not substitute another workspace when following a workspace's empty library", async () => {
  expect((await post(collection, input)).status).toBe(201);
  await open("/u/test/artifacts?workspace=other");
  await waitForUi(() => expect(container.textContent).toContain("No published artifacts"));
  expect(container.textContent).not.toContain("Release notes");
});

it("retains module records across router reassembly, rejects stale publication, and retracts only the snapshot", async () => {
  expect((await post(collection, { ...input, expectedRevision: 0 })).status).toBe(409);
  const response = await post(collection, input);
  expect(response.status).toBe(201);
  const { artifact } = await response.json();
  server = createDemoRouter(store, [{ ...artifacts, routes: artifactRoutes }]);
  expect((await server.request(collection + "/" + artifact.id)).status).toBe(200);
  expect((await server.request(collection + "/" + artifact.id, { method: "DELETE" })).status).toBe(204);
  expect((await server.request(collection + "/" + artifact.id + "/content")).status).toBe(404);
  expect(store.readText(artifact.blobRef)).toBe("Original bytes");
});

it("uses module grants for demo API calls even when a caller bypasses the UI", async () => {
  const universe = store.universe("test-universe")!.universe;
  universe.organizationRole = "member";
  universe.role = "viewer";
  expect((await post(collection, input)).status).toBe(403);
  universe.role = "contributor";
  const response = await post(collection, input);
  expect(response.status).toBe(201);
  const { artifact } = await response.json();
  const url = collection + "/" + artifact.id;
  expect((await (await server.request(collection)).json()).permissions).toEqual({ publish: true, retract: false });
  expect((await server.request(url, { method: "DELETE" })).status).toBe(403);
  universe.role = "operator";
  expect((await server.request(url, { method: "DELETE" })).status).toBe(204);
});
