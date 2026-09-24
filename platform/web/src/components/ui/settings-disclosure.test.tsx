// @vitest-environment jsdom
import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it } from "vitest";
import { SettingsDisclosure, SettingsGroup } from "./settings-disclosure";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});

function toggle() {
  return container.querySelector<HTMLButtonElement>("button[aria-expanded]")!;
}

/** A disclosure over one limit that is invalid below 1. */
function Limits({ initial = "" }: { initial?: string }) {
  const [limit, setLimit] = useState(initial);
  const invalid = limit !== "" && Number(limit) < 1;
  return (
    <SettingsDisclosure
      summary={limit ? `Max depth ${limit}` : "Default limits"}
      action="Customize limits"
      label="Customize sub-agent limits"
      forceOpen={invalid}
    >
      <SettingsGroup title="Nesting" description="How deep sub-agents may go.">
        <input
          aria-label="Max depth"
          value={limit}
          onChange={(event) => setLimit(event.target.value)}
        />
      </SettingsGroup>
    </SettingsDisclosure>
  );
}

async function type(value: string) {
  const input = container.querySelector<HTMLInputElement>('[aria-label="Max depth"]')!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

it("reads as the summary and an action while closed, and reveals the fields inline", async () => {
  await act(async () => root.render(<Limits />));
  expect(container.textContent).toBe("Default limits·Customize limits");
  expect(toggle().getAttribute("aria-expanded")).toBe("false");
  expect(toggle().getAttribute("aria-label")).toBe("Customize sub-agent limits");
  expect(container.querySelector('[aria-label="Max depth"]')).toBeNull();

  await act(async () => toggle().click());
  expect(toggle().getAttribute("aria-expanded")).toBe("true");
  expect(toggle().textContent).toBe("Hide");
  const content = document.getElementById(toggle().getAttribute("aria-controls")!);
  expect(content?.querySelector('[aria-label="Max depth"]')).not.toBeNull();
  const group = content!.querySelector('[role="group"]')!;
  expect(document.getElementById(group.getAttribute("aria-labelledby")!)?.textContent).toBe("Nesting");

  await type("3");
  expect(container.textContent).toContain("Max depth 3·Hide");
  await act(async () => toggle().click());
  expect(container.textContent).toBe("Max depth 3·Customize limits");
  expect(container.querySelector('[aria-label="Max depth"]')).toBeNull();
});

it("stays closed when saved values already differ from their defaults", async () => {
  await act(async () => root.render(<Limits initial="3" />));
  expect(toggle().getAttribute("aria-expanded")).toBe("false");
  expect(container.textContent).toBe("Max depth 3·Customize limits");
});

it("opens on its own while a field inside is invalid and keeps the fixed field in view", async () => {
  await act(async () => root.render(<Limits initial="0" />));
  expect(toggle().getAttribute("aria-expanded")).toBe("true");
  expect(toggle().disabled).toBe(true);
  expect(container.querySelector('[aria-label="Max depth"]')).not.toBeNull();

  await type("2");
  expect(toggle().disabled).toBe(false);
  expect(toggle().getAttribute("aria-expanded")).toBe("true");
  expect(container.querySelector('[aria-label="Max depth"]')).not.toBeNull();
  await act(async () => toggle().click());
  expect(container.querySelector('[aria-label="Max depth"]')).toBeNull();
});

it("names the visible action when no label is given and omits an empty summary", async () => {
  await act(async () =>
    root.render(
      <SettingsDisclosure action="Change">
        <p>Fields</p>
      </SettingsDisclosure>,
    ),
  );
  expect(container.textContent).toBe("Change");
  expect(toggle().hasAttribute("aria-label")).toBe(false);
});

it("follows a controlled open state", async () => {
  const changes: boolean[] = [];
  function Controlled() {
    const [open, setOpen] = useState(false);
    return (
      <SettingsDisclosure
        summary="All tools"
        action="Customize tools"
        open={open}
        onOpenChange={(next) => {
          changes.push(next);
          setOpen(next);
        }}
      >
        <p>{open ? "loading tools" : "idle"}</p>
      </SettingsDisclosure>
    );
  }
  await act(async () => root.render(<Controlled />));
  await act(async () => toggle().click());
  expect(changes).toEqual([true]);
  expect(container.textContent).toContain("loading tools");
});
