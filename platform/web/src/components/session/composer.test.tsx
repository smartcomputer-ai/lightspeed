// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it, vi } from "vitest";
import { SessionComposer } from "./composer";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let root: ReturnType<typeof createRoot>;
let container: HTMLDivElement;
afterEach(async () => {
  await act(async () => root?.unmount());
  container?.remove();
  localStorage.clear();
});
async function setup(runActive = true, canSteer = true, disabled = false, text = "  Change direction  ") {
  localStorage.setItem("composer-test", text);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  const onSend = vi.fn();
  await act(async () => root.render(<SessionComposer draftKey="composer-test" runActive={runActive}
    canSteer={canSteer} disabled={disabled} error={null} onSend={onSend} onStop={vi.fn()} />));
  return onSend;
}
it.each([[true, true, "Queue message", "queue"], [false, false, "Send message", null]] as const)(
  "keeps the primary send action for runActive=%s", async (active, steerable, label, mode) => {
    const onSend = await setup(active, steerable);
    await act(async () => container.querySelector<HTMLButtonElement>(`[aria-label="${label}"]`)!.click());
    expect(onSend).toHaveBeenCalledWith("Change direction", mode);
    expect(container.querySelector("textarea")!.value).toBe("");
    expect(localStorage.getItem("composer-test")).toBeNull();
  },
);
it("preserves keyboard steering and does not send while composing text", async () => {
  const onSend = await setup();
  const input = container.querySelector("textarea")!;
  await act(async () => input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", isComposing: true, bubbles: true })));
  expect(onSend).not.toHaveBeenCalled();
  await act(async () => input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", ctrlKey: true, bubbles: true })));
  expect(onSend).toHaveBeenCalledWith("Change direction", "steer");
});
