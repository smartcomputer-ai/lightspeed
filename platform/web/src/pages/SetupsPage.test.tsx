// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { SetupsPage } from "./SetupsPage";
vi.mock("@/lib/universes", () => ({ useActiveUniverse: () => ({ universe: { id: "u" }, handle: "test", isLoading: false }) }));
vi.mock("@/lib/permissions", () => ({ useActionPermissions: () => ({ can: () => true }) }));
it("shows unavailable setup without an installation or credential control", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const element = document.createElement("div");
  const root = createRoot(element);
  try {
    await act(async () => root.render(<SetupsPage admin />));
    expect(element.textContent).toContain("temporarily unavailable");
    expect(element.querySelector("button, input")).toBeNull();
  } finally {
    await act(async () => root.unmount());
    vi.unstubAllGlobals();
  }
});
