// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { SessionUser } from "@/auth";
import { SettingsPage, SettingsRedirect } from "./SettingsPage";

vi.mock("@/components/settings/account-settings", () => ({ AccountSettings: () => <div id="profile">account sections</div> }));
vi.mock("@/components/settings/appearance-settings", () => ({ AppearanceSettings: () => <div id="appearance">appearance sections</div> }));

let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

function Location() {
  const location = useLocation();
  return <output>{location.pathname + location.hash}</output>;
}

it("puts the account and appearance on one page, each under its heading", async () => {
  await act(async () => root.render(<MemoryRouter><SettingsPage user={{ id: "user", name: "Ada", email: "ada@example.test" } as SessionUser} /></MemoryRouter>));
  expect(container.querySelector("h1")?.textContent).toBe("Settings");
  expect([...container.querySelectorAll("h2")].map((heading) => heading.textContent)).toEqual(["Account", "Appearance"]);
  expect(container.textContent).toContain("account sections");
  expect(container.textContent).toContain("appearance sections");
});

it.each([["/account#api-keys", "/settings#api-keys"], ["/preferences", "/settings"]])("sends %s to %s", async (from, to) => {
  await act(async () => root.render(
    <MemoryRouter initialEntries={[from]}>
      <Routes>
        <Route path="/account" element={<SettingsRedirect />} />
        <Route path="/preferences" element={<SettingsRedirect />} />
        <Route path="/settings" element={<Location />} />
      </Routes>
    </MemoryRouter>,
  ));
  expect(container.querySelector("output")?.textContent).toBe(to);
});
