// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { LoginConfig, SessionUser } from "@/auth";
import { AccountPage } from "./AccountPage";
import { waitForUi } from "@/test/wait-for-ui";

const mocks = vi.hoisted(() => ({
  updateUser: vi.fn(),
  changePassword: vi.fn(),
  config: { sso: false, autoSignIn: false, providerId: null, password: "local" } as LoginConfig,
}));
vi.mock("@/auth", () => ({
  authClient: { updateUser: mocks.updateUser, changePassword: mocks.changePassword },
  useLoginConfig: () => ({ data: mocks.config }),
}));

const local: SessionUser = { id: "user", name: "Ada", email: "ada@example.test", identitySource: "local" };
let root: Root;
let container: HTMLDivElement;
let client: QueryClient;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("matchMedia", (query: string) => ({ matches: false, media: query, addEventListener: () => undefined, removeEventListener: () => undefined }));
  mocks.config = { sso: false, autoSignIn: false, providerId: null, password: "local" };
  mocks.updateUser.mockReset().mockResolvedValue({ error: null });
  mocks.changePassword.mockReset().mockResolvedValue({ error: null });
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

async function show(user: SessionUser = local) {
  await act(async () => root.render(
    <QueryClientProvider client={client}><AccountPage user={user} /></QueryClientProvider>,
  ));
}
const input = (id: string) => container.querySelector<HTMLInputElement>(`#${id}`);
const button = (label: string) => [...container.querySelectorAll<HTMLButtonElement>("button")].find((node) => node.textContent?.trim() === label);
async function type(id: string, value: string) {
  const node = input(id)!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(node, value);
    node.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
const submit = (card: string) => act(async () => container.querySelector<HTMLFormElement>(`#${card} form`)!.requestSubmit());

it("saves a trimmed name only once it differs from the current one", async () => {
  await show();
  expect(input("account-email")!.disabled).toBe(true);
  expect(button("Save")!.disabled).toBe(true);
  await type("account-name", "  Ada  ");
  expect(button("Save")!.disabled).toBe(true);
  await type("account-name", "  Ada Lovelace ");
  expect(button("Save")!.disabled).toBe(false);
  await submit("profile");
  await waitForUi(() => expect(container.querySelector("#profile")!.textContent).toContain("Saved."));
  expect(mocks.updateUser).toHaveBeenCalledWith({ name: "Ada Lovelace" });
});

it("shows the reason a profile update was refused", async () => {
  mocks.updateUser.mockResolvedValue({ error: { message: "name is reserved" } });
  await show();
  await type("account-name", "Root");
  await submit("profile");
  await waitForUi(() => expect(container.querySelector("#profile")!.textContent).toContain("name is reserved"));
  expect(container.querySelector("#profile")!.textContent).not.toContain("Saved.");
});

it("leaves a company-managed profile read-only and hides the password form", async () => {
  await show({ ...local, identitySource: "company" });
  expect(input("account-name")!.disabled).toBe(true);
  expect(button("Save")!.disabled).toBe(true);
  expect(container.textContent).toContain("managed by your company");
  expect(container.querySelector("#security")).toBeNull();
});

it("refuses mismatched new passwords without calling the server", async () => {
  await show();
  await type("current-password", "old-password");
  await type("new-password", "new-password-1");
  await type("confirm-password", "new-password-2");
  await submit("security");
  expect(container.querySelector("#security")!.textContent).toContain("New passwords do not match.");
  expect(mocks.changePassword).not.toHaveBeenCalled();
});

it("changes the password, signs out other sessions, and clears the form", async () => {
  await show();
  await type("current-password", "old-password");
  await type("new-password", "new-password-1");
  await type("confirm-password", "new-password-1");
  await submit("security");
  await waitForUi(() => expect(container.querySelector("#security")!.textContent).toContain("Password changed."));
  expect(mocks.changePassword).toHaveBeenCalledWith({ currentPassword: "old-password", newPassword: "new-password-1", revokeOtherSessions: true });
  expect([input("current-password")!.value, input("new-password")!.value, input("confirm-password")!.value]).toEqual(["", "", ""]);
});

it("keeps a refused password change on the form with its reason", async () => {
  mocks.changePassword.mockResolvedValue({ error: { message: "invalid password" } });
  await show();
  await type("current-password", "wrong");
  await type("new-password", "new-password-1");
  await type("confirm-password", "new-password-1");
  await submit("security");
  await waitForUi(() => expect(container.querySelector("#security")!.textContent).toContain("invalid password"));
  expect(container.querySelector("#security")!.textContent).not.toContain("Password changed.");
  expect(input("current-password")!.value).toBe("wrong");
});

it.each([
  { name: "password sign-in is off", config: { password: "off" }, user: local, shown: false },
  { name: "SSO is on", config: { sso: true, password: "break-glass" }, user: local, shown: false },
  { name: "SSO is on for an emergency admin", config: { sso: true, password: "break-glass" }, user: { ...local, emergencyAdmin: true }, shown: true },
] as const)("offers the password form only where it applies: $name", async ({ config, user, shown }) => {
  mocks.config = { ...mocks.config, ...config };
  await show(user);
  expect(container.querySelector("#security") !== null).toBe(shown);
});
