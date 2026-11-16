// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { SaveButton } from "./save-button";

it("saves only while there are unsaved changes, and says where the save is", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const container = document.createElement("div");
  const root = createRoot(container);
  const onClick = vi.fn();
  const show = (props: { dirty: boolean; saving?: boolean }) =>
    act(async () => root.render(<SaveButton {...props} onClick={onClick}>Save changes</SaveButton>));
  const button = () => container.querySelector("button")!;
  // The label shown; the others keep their space but are hidden.
  const shown = () => button().querySelector("[data-save-label]:not([aria-hidden])")!;
  // The check and the loading icon share the button's right end.
  const end = () => button().lastElementChild!;
  const check = () => end().querySelector('[data-icon-role="SelectedIcon"]')!;
  const loading = () => end().querySelector('[data-slot="animated-loading-icon"]')!;

  await show({ dirty: false });
  expect(shown().textContent).toBe("Saved");
  expect(check().getAttribute("class")).not.toContain("opacity-0");
  expect(loading().hasAttribute("data-active")).toBe(false);
  // Saved, it is secondary and ignores clicks, but is not disabled.
  expect(button().disabled).toBe(false);
  expect(button().getAttribute("aria-disabled")).toBe("true");
  expect(button().className).toContain("bg-secondary");
  await act(async () => button().click());
  expect(onClick).not.toHaveBeenCalled();
  // Every state is laid out, so the width is the widest one's; a failed
  // save only while shown.
  expect([...button().querySelectorAll("[data-save-label]")].map((label) => label.textContent))
    .toEqual(["Save changes", "Saving…", "Saved"]);

  await show({ dirty: true });
  expect(shown().textContent).toBe("Save changes");
  expect(button().disabled).toBe(false);
  expect(button().hasAttribute("aria-disabled")).toBe(false);
  expect(button().className).toContain("bg-primary");
  expect(check().getAttribute("class")).not.toContain("opacity-0");
  await act(async () => button().click());
  expect(onClick).toHaveBeenCalledOnce();

  // Saving, it stays primary and ignores clicks; the check gives way to the
  // loading icon.
  await show({ dirty: true, saving: true });
  expect(shown().textContent).toBe("Saving…");
  expect(button().disabled).toBe(false);
  expect(button().getAttribute("aria-disabled")).toBe("true");
  expect(button().getAttribute("aria-busy")).toBe("true");
  expect(button().className).toContain("bg-primary");
  await act(async () => button().click());
  expect(onClick).toHaveBeenCalledOnce();
  expect(check().getAttribute("class")).toContain("opacity-0");
  expect(loading().hasAttribute("data-active")).toBe(true);

  // The save finishes: the loading icon stops and the check grows in (not
  // when shown from the start).
  await show({ dirty: false });
  expect(loading().hasAttribute("data-active")).toBe(false);
  expect(check().hasAttribute("data-just-saved")).toBe(true);
  expect(check().getAttribute("class")).toContain("zoom-in-20");

  await act(async () => root.unmount());
  vi.unstubAllGlobals();
});

it("shows a failed save in the destructive colours, pulsing, and tries again when pressed", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const container = document.createElement("div");
  const root = createRoot(container);
  const onClick = vi.fn();
  await act(async () => root.render(<SaveButton dirty error onClick={onClick} />));
  const button = container.querySelector("button")!;
  expect(button.getAttribute("data-save-state")).toBe("error");
  expect([...button.querySelectorAll("[data-save-label]")].map((label) => label.textContent))
    .toEqual(["Save", "Save failed", "Saving…", "Saved"]);
  expect(button.querySelector("[data-save-label]:not([aria-hidden])")!.textContent).toBe("Save failed");
  expect(button.className).toContain("bg-destructive/10");
  expect(button.hasAttribute("aria-disabled")).toBe(false);
  const failed = button.querySelector('[data-icon-role="FailedIcon"]')!;
  expect(failed.getAttribute("class")).toContain("motion-safe:animate-pulse");
  expect(button.querySelector('[data-icon-role="SelectedIcon"]')!.getAttribute("class")).toContain("opacity-0");
  await act(async () => button.click());
  expect(onClick).toHaveBeenCalledOnce();
  await act(async () => root.unmount());
  vi.unstubAllGlobals();
});
