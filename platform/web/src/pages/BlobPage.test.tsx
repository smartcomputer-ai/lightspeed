// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { BlobPage } from "./BlobPage";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("@/api", async (original) => ({ ...await original<typeof import("@/api")>(), api: mocks.api }));
vi.mock("@/lib/universes", () => ({ useActiveUniverse: () => ({ universe: { id: "universe", role: "viewer", slug: "acme", name: "Acme" }, slug: "acme", isLoading: false }) }));

const digest = "c".repeat(64);
let root: Root;
let container: HTMLDivElement;
let stored: string;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  stored = "# Instructions\n\n<script>alert(1)</script>";
  mocks.api.mockReset().mockImplementation(async (_method: string, path: string) => {
    if (path.endsWith("/access")) return { actions: ["read"], resources: [] };
    if (path.endsWith("/sessions/s1")) return { id: "s1", displayName: "HIT login debugging" };
    if (path.endsWith("/sessions/private")) throw new Error("not found");
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
  container.remove();
  vi.unstubAllGlobals();
});

async function open(url: string) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  await act(async () => root.render(
    <QueryClientProvider client={client}>
      <PermissionIdentityProvider userId="user" platformAdmin={false}>
        <MemoryRouter initialEntries={[url]}>
          <Routes><Route path="/u/:slug/blobs/:digest" element={<BlobPage admin={false} />} /></Routes>
        </MemoryRouter>
      </PermissionIdentityProvider>
    </QueryClientProvider>,
  ));
  await vi.waitFor(() => expect(container.querySelector("pre, img, iframe")).not.toBeNull());
}

it("shows stored text exactly as stored, named from the link, with its source session", async () => {
  await open(`/u/acme/blobs/${digest}?name=Profile%20instructions&type=text/markdown&session=s1`);
  expect(container.querySelector("h1")!.textContent).toBe("Profile instructions");
  expect(container.textContent).toContain("Markdown · ");
  expect(container.querySelector("pre")!.textContent).toBe(stored);
  expect(container.querySelector("script")).toBeNull();
  await vi.waitFor(() => expect(container.textContent).toContain("Linked from session: HIT login debugging"));
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
  await vi.waitFor(() => expect(container.textContent).toContain("Linked from session: private"));
  expect(container.querySelector(`header a[href*="/sessions/"]`)).toBeNull();
});

it("refuses addresses that are not digests without reading anything", async () => {
  const client = new QueryClient();
  await act(async () => root.render(
    <QueryClientProvider client={client}>
      <PermissionIdentityProvider userId="user" platformAdmin={false}>
        <MemoryRouter initialEntries={["/u/acme/blobs/not-a-digest"]}>
          <Routes><Route path="/u/:slug/blobs/:digest" element={<BlobPage admin={false} />} /></Routes>
        </MemoryRouter>
      </PermissionIdentityProvider>
    </QueryClientProvider>,
  ));
  await vi.waitFor(() => expect(container.textContent).toContain("This is not a blob address."));
  expect(mocks.api.mock.calls.some(([, path]) => String(path).includes("/blobs/"))).toBe(false);
});
