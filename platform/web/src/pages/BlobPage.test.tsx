// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { waitForUi } from "@/test/wait-for-ui";
import { BlobPage } from "./BlobPage";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({ useActiveUniverse: () => ({ universe: { id: "universe", role: "viewer", slug: "acme", name: "Acme" }, slug: "acme", isLoading: false }) }));

const digest = "c".repeat(64);
const filePath = "docs/résumé #?.txt";
let root: Root;
let container: HTMLDivElement;
let stored: string;
let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
  stored = "# Instructions\n\n<script>alert(1)</script>";
  mocks.api.mockReset().mockImplementation(async (_method: string, path: string) => {
    if (path.endsWith("/access")) return { actions: ["read"], resources: [] };
    if (path.endsWith("/sessions/s1")) return { id: "s1", displayName: "HIT login debugging" };
    if (path.endsWith("/sessions/private")) throw new Error("not found");
    if (path.endsWith("/workspaces/ws/tree")) return {
      workspace: { workspaceId: "ws", displayName: "Documents" },
      manifest: { root: { entries: {
        docs: { kind: "directory", entries: {
          "résumé #?.txt": { kind: "file", blob_ref: `sha256:${digest}` },
        } },
      } } },
    };
    if (path.endsWith("/workspaces/private/tree")) throw new Error("not found");
    if (path.endsWith(`/blobs/${encodeURIComponent(`sha256:${digest}`)}`)) {
      const bytes = new TextEncoder().encode(stored);
      return { bytesBase64: btoa(String.fromCharCode(...bytes)), bytes: bytes.length };
    }
    throw new Error(`unexpected ${path}`);
  });
  URL.createObjectURL = vi.fn(() => "blob:local");
  URL.revokeObjectURL = vi.fn();
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

async function open(url: string) {
  await act(async () => root.render(
    <QueryClientProvider client={client}>
      <PermissionIdentityProvider userId="user" platformAdmin={false}>
        <MemoryRouter initialEntries={[url]}>
          <Routes><Route path="/u/:slug/blobs/:digest" element={<BlobPage admin={false} />} /></Routes>
        </MemoryRouter>
      </PermissionIdentityProvider>
    </QueryClientProvider>,
  ));
  await waitForUi(() => {
    expect(container.querySelector("pre, img, iframe")).not.toBeNull();
    expect(client.isFetching()).toBe(0);
  });
}

it("shows stored text exactly as stored, named from the link, with its source session", async () => {
  await open(`/u/acme/blobs/${digest}?name=Profile%20instructions&type=text/markdown&session=s1`);
  expect(container.querySelector("h1")!.textContent).toBe("Profile instructions");
  expect(container.textContent).toContain("Markdown · ");
  expect(container.querySelector("pre")!.textContent).toBe(stored);
  expect(container.querySelector("script")).toBeNull();
  await waitForUi(() => expect(container.textContent).toContain("Linked from session: HIT login debugging"));
  expect(container.querySelector('header a[href="/u/acme/sessions/s1"]')!.textContent).toBe("HIT login debugging");
  expect(document.title).toBe("Profile instructions");
});

it("hands images to the browser and names unnamed blobs by digest", async () => {
  stored = "\u0089PNG\r\n";
  mocks.api.mockImplementation(async (_method: string, path: string) => {
    if (path.endsWith("/access")) return { actions: ["read"], resources: [] };
    return { bytesBase64: btoa(String.fromCharCode(0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a)), bytes: 6 };
  });
  await open(`/u/acme/blobs/${digest}`);
  expect(container.querySelector("img")!.getAttribute("src")).toBe("blob:local");
  expect(container.querySelector("h1")!.textContent).toBe(`sha256:${digest.slice(0, 12)}…`);
});

it("names a source session the viewer cannot read by its id, without a link", async () => {
  await open(`/u/acme/blobs/${digest}?session=private`);
  await waitForUi(() => expect(container.textContent).toContain("Linked from session: private"));
  expect(container.querySelector(`header a[href*="/sessions/"]`)).toBeNull();
});

it("links to the source workspace and file alongside an optional session", async () => {
  await open(`/u/acme/blobs/${digest}?workspace=ws&path=${encodeURIComponent(filePath)}&session=s1`);
  expect(container.textContent).toContain("Linked from workspace: Documents / " + filePath);
  expect(container.querySelector('header a[href="/u/acme/workspaces/ws"]')!.textContent).toBe("Documents");
  const fileLink = container.querySelector<HTMLAnchorElement>('header a[href*="/files/"]')!;
  expect(fileLink.getAttribute("href")).toBe(`/u/acme/workspaces/ws/files/${filePath.split("/").map(encodeURIComponent).join("/")}`);
  expect(container.querySelector('header a[href="/u/acme/sessions/s1"]')).not.toBeNull();
});

it.each([undefined, "docs/missing.txt", "docs", "../sessions/s1", "docs/__proto__"])(
  "keeps the workspace backlink without linking a missing or invalid file (%s)", async (path) => {
    await open(`/u/acme/blobs/${digest}?workspace=ws${path ? `&path=${encodeURIComponent(path)}` : ""}`);
    expect(container.querySelector('header a[href="/u/acme/workspaces/ws"]')).not.toBeNull();
    expect(container.querySelector('header a[href*="/files/"]')).toBeNull();
    expect(container.querySelector("pre")!.textContent).toBe(stored);
  },
);

it("keeps the blob viewable when its source workspace is unavailable", async () => {
  await open(`/u/acme/blobs/${digest}?workspace=private&path=${encodeURIComponent(filePath)}`);
  expect(container.textContent).toContain("Linked from workspace: private / " + filePath);
  expect(container.querySelector('header a[href*="/workspaces/"]')).toBeNull();
  expect(container.querySelector("pre")!.textContent).toBe(stored);
});

it("does not look up workspaces when the link has no workspace source", async () => {
  await open(`/u/acme/blobs/${digest}`);
  expect(container.textContent).not.toContain("Linked from workspace:");
  expect(mocks.api.mock.calls.some(([, path]) => String(path).includes("/workspaces/"))).toBe(false);
});

it("refuses addresses that are not digests without reading anything", async () => {
  await act(async () => root.render(
    <QueryClientProvider client={client}>
      <PermissionIdentityProvider userId="user" platformAdmin={false}>
        <MemoryRouter initialEntries={["/u/acme/blobs/not-a-digest"]}>
          <Routes><Route path="/u/:slug/blobs/:digest" element={<BlobPage admin={false} />} /></Routes>
        </MemoryRouter>
      </PermissionIdentityProvider>
    </QueryClientProvider>,
  ));
  await waitForUi(() => expect(container.textContent).toContain("This is not a blob address."));
  expect(mocks.api.mock.calls.some(([, path]) => String(path).includes("/blobs/"))).toBe(false);
});
