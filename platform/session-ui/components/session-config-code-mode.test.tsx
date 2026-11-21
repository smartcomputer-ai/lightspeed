// @vitest-environment jsdom
import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { configError, normalizeSessionConfig, SessionConfigEditor, type SessionConfig } from "./session-config-editor";

let root: Root;
let container: HTMLDivElement;
let current: SessionConfig | undefined;
let error: string | null;

afterEach(async () => {
  await act(async () => root?.unmount());
  container?.remove();
  vi.unstubAllGlobals();
});

async function setup(value: SessionConfig = {}) {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  function Harness() {
    const [config, setConfig] = useState<SessionConfig | undefined>(value);
    current = config;
    return <SessionConfigEditor value={config} onChange={setConfig} onValidityChange={(message) => { error = message; }} />;
  }
  await act(async () => root.render(<Harness />));
}

async function click(selector: string) {
  const button = container.querySelector<HTMLButtonElement>(selector);
  expect(button).not.toBeNull();
  await act(async () => button!.click());
}

async function expandFeature() {
  const button = [...container.querySelectorAll<HTMLButtonElement>("button[aria-expanded]")]
    .find((item) => item.textContent?.startsWith("Code mode"));
  expect(button).toBeDefined();
  await act(async () => button!.click());
}

async function input(label: string, value: string) {
  const fieldLabel = [...container.querySelectorAll("label")].find((item) => item.textContent === label);
  expect(fieldLabel).toBeDefined();
  const field = document.getElementById(fieldLabel!.htmlFor) as HTMLInputElement;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(field, value);
    field.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

const featureToggle = '[role="switch"][aria-label="Enable Code mode"]';
const limitsToggle = '[aria-label="Customize code mode limits"]';

it("enables all granted tools by default and reveals only three optional limits", async () => {
  await setup();
  expect(container.querySelector(featureToggle)?.getAttribute("aria-checked")).toBe("false");
  await click(featureToggle);
  expect(current).toEqual({ features: { codeMode: {} } });
  expect(error).toBeNull();
  expect(container.textContent).toContain("Default limits");
  expect(container.querySelector('input[type="number"]')).toBeNull();

  await click(limitsToggle);
  const fields = [...container.querySelectorAll<HTMLInputElement>('input[type="number"]')];
  expect(fields.map((field) => field.placeholder)).toEqual(["60000", "128", "16"]);
  expect(fields.every((field) => field.value === "")).toBe(true);
  expect(container.textContent).not.toMatch(/Allowed tools|Memory|Stack|Catalog|Source size|Result size|Output size/);
  expect(current).toEqual({ features: { codeMode: {} } });

  await input("Max tool calls", "64");
  await click(featureToggle);
  expect(current ?? {}).not.toHaveProperty("features.codeMode");
  expect(container.querySelector(limitsToggle)).toBeNull();
  await click(featureToggle);
  expect(current).toEqual({ features: { codeMode: {} } });
  expect(container.querySelector('input[type="number"]')).toBeNull();
});

it("edits and clears saved limits without exposing or resetting API-only settings", async () => {
  const hidden = { version: 1, allowedTools: ["concurrency.sleep"], maxMemoryBytes: 33_554_432, maxOutputBytes: 524_288 };
  await setup({ features: { codeMode: { ...hidden, timeoutMs: 45_000, maxToolCalls: 64, maxOutstandingToolCalls: 8 } } });
  const before = structuredClone(current);
  await expandFeature();
  expect(container.textContent).toContain("3 custom limits");
  expect(container.querySelector('input[type="number"]')).toBeNull();
  await click(limitsToggle);
  expect(current).toEqual(before);
  expect([...container.querySelectorAll<HTMLInputElement>('input[type="number"]')].map((field) => field.value))
    .toEqual(["45000", "64", "8"]);

  await input("Timeout (ms)", "60000");
  await input("Max tool calls", "32");
  await input("Max outstanding calls", "4");
  await click(limitsToggle);
  expect(container.querySelector('input[type="number"]')).toBeNull();
  expect(current).toEqual({ features: { codeMode: { ...hidden, timeoutMs: 60_000, maxToolCalls: 32, maxOutstandingToolCalls: 4 } } });

  await click(limitsToggle);
  await input("Timeout (ms)", "");
  await input("Max tool calls", "");
  await input("Max outstanding calls", "");
  expect(current).toEqual({ features: { codeMode: hidden } });
  expect(error).toBeNull();
  expect(container.textContent).toContain("Default limits");
});

it("keeps invalid limits visible and reports them instead of restoring defaults", async () => {
  await setup({ features: { codeMode: {} } });
  await expandFeature();
  await click(limitsToggle);
  await input("Timeout (ms)", "0");
  expect(current).toHaveProperty("features.codeMode.timeoutMs", 0);
  expect(error).toContain("between 1 and 600,000");
  expect(container.querySelector<HTMLButtonElement>(limitsToggle)?.disabled).toBe(true);
  await input("Timeout (ms)", "");
  await input("Max tool calls", "8");
  expect(error).toContain("cannot exceed");
  await input("Max outstanding calls", "8");
  expect(error).toBeNull();
  await click(limitsToggle);
  expect(container.querySelector('input[type="number"]')).toBeNull();
});

it("preserves pure-computation restrictions when changing another feature", async () => {
  await setup({ features: { codeMode: { allowedTools: [], maxSourceBytes: 1024 } } });
  await click('[role="switch"][aria-label="Enable Timers"]');
  expect(current).toEqual({ features: { codeMode: { allowedTools: [], maxSourceBytes: 1024 }, timers: {} } });
});

describe("code-mode limit validation", () => {
  it.each([
    { timeoutMs: 0 },
    { timeoutMs: 600_001 },
    { maxToolCalls: -1 },
    { maxToolCalls: 1_025 },
    { maxToolCalls: 1.5 },
    { maxOutstandingToolCalls: 65 },
    { maxToolCalls: 8 },
    { maxToolCalls: 32, maxOutstandingToolCalls: 33 },
  ])("rejects invalid limits %j before saving", (codeMode) => {
    const normalized = normalizeSessionConfig({ features: { codeMode } });
    expect(normalized).toEqual({ features: { codeMode } });
    expect(configError(normalized)).not.toBeNull();
  });

  it.each([{}, { timeoutMs: 600_000, maxToolCalls: 1_024, maxOutstandingToolCalls: 64 }, { maxToolCalls: 1, maxOutstandingToolCalls: 1 }])
    ("accepts defaults and valid limit boundaries %j", (codeMode) => {
      expect(configError(normalizeSessionConfig({ features: { codeMode } }))).toBeNull();
    });
});
