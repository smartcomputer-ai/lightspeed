// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { Button } from "./button";
import { ButtonGroup } from "./button-group";

it("holds back the hover of a choice just clicked until the pointer leaves it", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  await act(async () => root.render(
    <ButtonGroup aria-label="Theme">
      <Button aria-pressed><span>Light</span></Button>
      <Button aria-pressed={false}>Dark</Button>
    </ButtonGroup>,
  ));
  const [light] = container.querySelectorAll("button");
  const label = light!.querySelector("span")!;

  // From the keyboard there is no hover to hold back.
  await act(async () => light!.dispatchEvent(new MouseEvent("click", { bubbles: true, detail: 0 })));
  expect(light!.hasAttribute("data-just-pressed")).toBe(false);

  await act(async () => label.dispatchEvent(new MouseEvent("click", { bubbles: true, detail: 1 })));
  expect(light!.hasAttribute("data-just-pressed")).toBe(true);
  // Moving within the button keeps it.
  await act(async () => label.dispatchEvent(new MouseEvent("pointerout", { bubbles: true, relatedTarget: light })));
  expect(light!.hasAttribute("data-just-pressed")).toBe(true);
  // Leaving it clears it, so the next hover shows.
  await act(async () => light!.dispatchEvent(new MouseEvent("pointerout", { bubbles: true, relatedTarget: document.body })));
  expect(light!.hasAttribute("data-just-pressed")).toBe(false);

  await act(async () => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

it("ends toggle buttons with a check only when the group asks for check marks", async () => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  const container = document.createElement("div");
  const root = createRoot(container);
  const checks = (label: string) => container.querySelectorAll(`[aria-label="${label}"] [data-icon-role="SelectedIcon"]`);
  await act(async () => root.render(
    <>
      <ButtonGroup aria-label="Plain">
        <Button aria-pressed>Light</Button>
        <Button aria-pressed={false}>Dark</Button>
      </ButtonGroup>
      <ButtonGroup aria-label="Checked" checkMarks>
        <Button aria-pressed>Light</Button>
        <Button aria-pressed={false}>Dark</Button>
        <Button>Not a toggle</Button>
      </ButtonGroup>
    </>,
  ));
  // Off by default, with no space kept for them.
  expect(checks("Plain")).toHaveLength(0);
  // One per toggle button: full for the choice, faint for the other.
  const [chosen, other] = checks("Checked");
  expect(checks("Checked")).toHaveLength(2);
  expect(chosen!.getAttribute("class")).not.toContain("opacity-10");
  expect(other!.getAttribute("class")).toContain("opacity-10");
  await act(async () => root.unmount());
  vi.unstubAllGlobals();
});
