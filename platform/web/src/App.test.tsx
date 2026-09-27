// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, useLocation } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { App } from "./App";

const mocks = vi.hoisted(() => ({
  config: { data: { sso: false, autoSignIn: false }, isPending: false },
}));
vi.mock("./auth.js", () => ({
  authClient: { useSession: () => ({ data: null, isPending: false }) },
  useLoginConfig: () => mocks.config,
  isPlatformAdmin: () => false,
}));
vi.mock("@/pages/LoginPage", () => ({ LoginPage: () => <div>Login page</div> }));

let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  mocks.config.data = { sso: false, autoSignIn: false };
  mocks.config.isPending = false;
  client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } });
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
function Location() {
  const location = useLocation();
  return <output>{location.pathname}{location.search}{location.hash}</output>;
}
async function render(path = "/app/") {
  await act(async () => root.render(
    <QueryClientProvider client={client}>
      <MemoryRouter basename="/app" initialEntries={[path]}><App /><Location /></MemoryRouter>
    </QueryClientProvider>,
  ));
}

it.each([
  [false, false, "/login"],
  [false, true, "/login"],
  [true, false, "/login"],
  [true, true, "/login?auto=1"],
] as const)("chooses the login URL for SSO=%s and autoSignIn=%s", async (sso, autoSignIn, expected) => {
  mocks.config.data = { sso, autoSignIn };
  await render();
  expect(container.querySelector("output")?.textContent).toBe(expected);
  expect(container.textContent).toContain("Login page");
});

it("keeps an explicit login visit manual even with automatic SSO", async () => {
  mocks.config.data = { sso: true, autoSignIn: true };
  await render("/app/login");
  expect(container.querySelector("output")?.textContent).toBe("/login");
});

it("removes a stale auto marker while preserving the login error and fragment", async () => {
  await render("/app/login?auto=1&error=company_sign_in_failed#help");
  expect(container.querySelector("output")?.textContent).toBe("/login?error=company_sign_in_failed#help");
});

it("waits for runtime configuration before selecting the login URL", async () => {
  mocks.config.isPending = true;
  await render();
  expect(container.querySelector("output")?.textContent).toBe("/");
  expect(container.textContent).toContain("Loading…");
  mocks.config.data = { sso: true, autoSignIn: true };
  mocks.config.isPending = false;
  await render();
  expect(container.querySelector("output")?.textContent).toBe("/login?auto=1");
});
