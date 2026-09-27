// @vitest-environment jsdom
import { act, StrictMode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { LoginPage } from "./LoginPage";
import { finishAutomaticSignIn } from "@/lib/automatic-sign-in";
const mocks = vi.hoisted(() => ({
  config: { data: { sso: true, autoSignIn: true, providerId: "company-test", password: "break-glass" }, isPending: false, error: null },
  social: vi.fn(), email: vi.fn(),
}));
vi.mock("@/auth", () => ({ useLoginConfig: () => mocks.config, authClient: { signIn: { social: mocks.social, email: mocks.email } } }));
let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  window.history.replaceState({}, "", "/app/login");
  sessionStorage.clear();
  mocks.config.data = { sso: true, autoSignIn: true, providerId: "company-test", password: "break-glass" };
  mocks.social.mockReset().mockResolvedValue({});
  mocks.email.mockReset();
  container = document.createElement("div"); document.body.append(container); root = createRoot(container);
});
afterEach(async () => { await act(async () => root.unmount()); container.remove(); vi.unstubAllGlobals(); });
async function render() { await act(async () => root.render(<LoginPage />)); }
function button(text: string) { return [...container.querySelectorAll("button")].find((button) => button.textContent?.includes(text))!; }
it("offers company sign-in first and a separate explicit emergency form", async () => {
  await render();
  expect(container.querySelector('input[type="password"]')).toBeNull();
  await act(async () => button("Sign in with your company account").click());
  expect(mocks.social).toHaveBeenCalledWith(expect.objectContaining({ provider: "company-test" }));
  await act(async () => button("Admins").click());
  expect(container.querySelector('input[type="password"]')).not.toBeNull();
  expect(container.textContent).toContain("Only designated local emergency administrators");
});
it("has no password path when disabled", async () => {
  mocks.config.data.password = "off";
  await render();
  expect(container.textContent).not.toContain("Admins");
  expect(container.querySelector("form")).toBeNull();
});
it("retains the local sign-in form without SSO", async () => {
  mocks.config.data.sso = false;
  mocks.config.data.password = "local";
  await render();
  expect(container.querySelector('input[type="password"]')).not.toBeNull();
  expect(container.textContent).not.toContain("Sign in with your company account");
});
it("shows a provider failure without silently falling back to passwords", async () => {
  mocks.social.mockResolvedValue({ error: { message: "provider unavailable" } });
  await render();
  await act(async () => button("Sign in with your company account").click());
  expect(container.textContent).toContain("Company sign-in is unavailable");
  expect(container.querySelector('input[type="password"]')).toBeNull();
});

it.each(["auto=1", "expired=1"])("starts automatic company sign-in once for %s", async (query) => {
  window.history.replaceState({}, "", `/app/login?${query}`);
  await act(async () => root.render(<StrictMode><LoginPage /></StrictMode>));
  expect(mocks.social).toHaveBeenCalledTimes(1);
  await act(async () => root.render(<StrictMode><LoginPage /></StrictMode>));
  expect(mocks.social).toHaveBeenCalledTimes(1);
});
it("keeps an explicit visit to the login page available for logout and admin access", async () => {
  await render();
  expect(mocks.social).not.toHaveBeenCalled();
  await act(async () => button("Admins").click());
  expect(container.querySelector('input[type="password"]')).not.toBeNull();
  expect(mocks.social).not.toHaveBeenCalled();
});
it("does not restart SSO after a provider callback error", async () => {
  window.history.replaceState({}, "", "/app/login?auto=1&error=company_sign_in_failed");
  await render();
  expect(mocks.social).not.toHaveBeenCalled();
  expect(container.textContent).toContain("Company sign-in failed or access was not granted");
});
it("does not loop after an unsuccessful automatic attempt, but permits an explicit retry", async () => {
  window.history.replaceState({}, "", "/app/login?auto=1");
  mocks.social.mockResolvedValue({ error: { message: "unavailable" } });
  await render();
  expect(mocks.social).toHaveBeenCalledTimes(1);
  expect(container.textContent).toContain("Company sign-in is unavailable");
  await act(async () => root.unmount());
  root = createRoot(container);
  await render();
  expect(mocks.social).toHaveBeenCalledTimes(1);
  await act(async () => button("Sign in with your company account").click());
  expect(mocks.social).toHaveBeenCalledTimes(2);
});
it("keeps local login manual even when the app requests automatic sign-in", async () => {
  window.history.replaceState({}, "", "/app/login?auto=1");
  mocks.config.data.sso = false;
  mocks.config.data.password = "local";
  await render();
  expect(mocks.social).not.toHaveBeenCalled();
  expect(container.querySelector('input[type="password"]')).not.toBeNull();
});

it("allows automatic renewal again after an authenticated session clears the attempt", async () => {
  window.history.replaceState({}, "", "/app/login?auto=1");
  await render();
  expect(mocks.social).toHaveBeenCalledTimes(1);
  await act(async () => root.unmount());
  finishAutomaticSignIn();
  window.history.replaceState({}, "", "/app/login?expired=1");
  root = createRoot(container);
  await render();
  expect(mocks.social).toHaveBeenCalledTimes(2);
});

it.each(["auto=1", "expired=1"])("waits for a click when automatic sign-in is disabled for %s", async (query) => {
  window.history.replaceState({}, "", `/app/login?${query}`);
  mocks.config.data.autoSignIn = false;
  await render();
  expect(mocks.social).not.toHaveBeenCalled();
  await act(async () => button("Sign in with your company account").click());
  expect(mocks.social).toHaveBeenCalledTimes(1);
  expect(mocks.social).toHaveBeenCalledWith(expect.objectContaining({ provider: "company-test" }));
});
