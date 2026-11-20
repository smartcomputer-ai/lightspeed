import { act } from "react";

/** Exercise the shared select's visible trigger and portaled options. */
export async function selectOption(trigger: HTMLElement, label: string) {
  await act(async () => trigger.click());
  const option = [...document.querySelectorAll<HTMLElement>('[data-slot="select-content"]:not([data-closed]) [role="option"]')]
    .find((item) => item.textContent?.trim() === label);
  if (!option) throw new Error(`Select option not found: ${label}`);
  await act(async () => option.click());
}
