// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { UserPreferencesProvider, useUserPreferences } from "./user-preferences";

let root: Root;
let container: HTMLDivElement;
const key = "lightspeed:user-preferences:alice";
function Consumer({ label }: { label: string }) {
  const { showRunStatistics, setShowRunStatistics } = useUserPreferences();
  return <button aria-label={label} onClick={() => setShowRunStatistics(!showRunStatistics)}>{String(showRunStatistics)}</button>;
}
async function render(userId = "alice") {
  await act(async () => root.render(<UserPreferencesProvider userId={userId}>
    <Consumer label="session" /><Consumer label="bot" />
  </UserPreferencesProvider>));
}
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  localStorage.clear();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  localStorage.clear();
});

it("shares the choice across consumers, persists reloads, and scopes it to the user", async () => {
  await render();
  expect(container.textContent).toBe("falsefalse");
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="bot"]')!.click());
  expect(container.textContent).toBe("truetrue");
  expect(JSON.parse(localStorage.getItem(key)!)).toEqual({ showRunStatistics: true });
  await act(async () => root.render(null));
  await render();
  expect(container.textContent).toBe("truetrue");
  await render("bob");
  expect(container.textContent).toBe("falsefalse");
  await render("alice");
  expect(container.textContent).toBe("truetrue");
});

it("updates from other tabs and resets to the default when storage is cleared", async () => {
  await render();
  localStorage.setItem(key, '{"showRunStatistics":true}');
  await act(async () => window.dispatchEvent(new StorageEvent("storage", { key })));
  expect(container.textContent).toBe("truetrue");
  localStorage.clear();
  await act(async () => window.dispatchEvent(new StorageEvent("storage", { key: null })));
  expect(container.textContent).toBe("falsefalse");
});

it.each(["broken JSON", '{"showRunStatistics":"true"}', "null"])("uses the default for invalid storage: %s", async (value) => {
  localStorage.setItem(key, value);
  await render();
  expect(container.textContent).toBe("falsefalse");
});

it("keeps the toggle working when browser storage is blocked", async () => {
  vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => { throw new Error("blocked"); });
  vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("blocked"); });
  await render();
  await act(async () => container.querySelector("button")!.click());
  expect(container.textContent).toBe("truetrue");
});

it("stores the run collapse choice beside the statistics choice and reads it back", async () => {
  function Collapse() {
    const { collapseCompletedRuns, setCollapseCompletedRuns } = useUserPreferences();
    return <button aria-label="collapse" onClick={() => setCollapseCompletedRuns(!collapseCompletedRuns)}>{String(collapseCompletedRuns)}</button>;
  }
  await act(async () => root.render(<UserPreferencesProvider userId="alice"><Collapse /><Consumer label="stats" /></UserPreferencesProvider>));
  expect(container.textContent).toBe("truefalse");
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="collapse"]')!.click());
  expect(container.textContent).toBe("falsefalse");
  expect(JSON.parse(localStorage.getItem(key)!)).toEqual({ collapseCompletedRuns: false });
  localStorage.setItem(key, '{"showRunStatistics":true}');
  await act(async () => window.dispatchEvent(new StorageEvent("storage", { key })));
  expect(container.textContent).toBe("truetrue");
});

it("keeps dragged widths on the device, reading widths stored with the preferences until set again", async () => {
  function Widths() {
    const { sidebarWidth, listWidth, setListWidth } = useUserPreferences();
    return <button aria-label="widths" onClick={() => setListWidth(null)}>{`${sidebarWidth}/${listWidth}`}</button>;
  }
  localStorage.setItem(key, JSON.stringify({ showRunStatistics: true, sidebarWidth: 280, listWidth: 400 }));
  localStorage.setItem("lightspeed:layout:alice", JSON.stringify({ sidebarWidth: 300 }));
  await act(async () => root.render(<UserPreferencesProvider userId="alice"><Widths /></UserPreferencesProvider>));
  expect(container.textContent).toBe("300/400");
  // Resetting a width moved to the device also clears the old copy, so it
  // does not come back.
  await act(async () => container.querySelector("button")!.click());
  expect(container.textContent).toBe("300/null");
  expect(JSON.parse(localStorage.getItem(key)!)).toEqual({ showRunStatistics: true, sidebarWidth: 280 });
});
