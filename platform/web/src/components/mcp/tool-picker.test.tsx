// @vitest-environment jsdom
import { act, useState, type ComponentProps } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it, vi } from "vitest";
import type { McpToolDiscovery } from "@/api";
import { McpToolPicker } from "./tool-picker";

// Exercise selection behavior with a native control; jsdom cannot lay out the floating popup.
vi.mock("@/components/ui/select", async () => {
  const React = await import("react");
  const Part = () => null;
  return {
    Select: ({
      value,
      onValueChange,
      children,
    }: {
      value: string;
      onValueChange: (value: string) => void;
      children: React.ReactNode;
    }) => {
      const options: { value: string; label: React.ReactNode }[] = [];
      const visit = (children: React.ReactNode) =>
        React.Children.forEach(children, (child) => {
          if (!React.isValidElement(child)) return;
          const props = child.props as {
            value?: string;
            children?: React.ReactNode;
          };
          if (props.value !== undefined)
            options.push({ value: props.value, label: props.children });
          else if (props.children) visit(props.children);
        });
      visit(children);
      return (
        <select
          aria-label="Tool selection mode"
          value={value}
          onChange={(event) => onValueChange(event.target.value)}
        >
          {options.map((option) => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
        </select>
      );
    },
    SelectContent: Part,
    SelectItem: Part,
    SelectTrigger: Part,
    SelectValue: Part,
  };
});

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
Object.assign(window, { PointerEvent: MouseEvent });
let root: ReturnType<typeof createRoot>;
let container: HTMLDivElement;
let selected: string[] | undefined;
type Props = ComponentProps<typeof McpToolPicker>;
afterEach(async () => {
  await act(async () => root?.unmount());
  container?.remove();
});

function Harness(props: Omit<Props, "onChange">) {
  const [value, setValue] = useState(props.value);
  selected = value;
  return <McpToolPicker {...props} value={value} onChange={setValue} />;
}

async function setup(props: Omit<Props, "onChange">) {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () => root.render(<Harness {...props} />));
}

async function button(name: string) {
  const target = [
    ...container.querySelectorAll<HTMLButtonElement>("button"),
  ].find((button) => button.textContent === name)!;
  expect(target).toBeDefined();
  await act(async () => target.click());
}

async function mode(label: string) {
  const select = container.querySelector<HTMLSelectElement>("select")!;
  const option = [...select.options].find(
    (option) => option.textContent === label,
  )!;
  expect(option).toBeDefined();
  await act(async () => {
    select.value = option.value;
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

async function checkbox(name: string) {
  const target = container.querySelector<HTMLButtonElement>(
    `[role="checkbox"][aria-label="${name}"]`,
  )!;
  expect(target).not.toBeNull();
  await act(async () => target.click());
}

it.each(["server", "session"] as const)(
  "shares metadata, search, and selection drafts in the %s picker",
  async (scope) => {
    const discover = vi.fn(async (): Promise<McpToolDiscovery> => ({
      status: "success",
      tools: [
        {
          name: "search",
          title: "Find issues",
          description: "Locate tickets by keyword.",
          annotations: { readOnlyHint: true },
        },
        {
          name: "delete",
          title: "Delete issues",
          annotations: { destructiveHint: true },
        },
      ],
    }));
    await setup({
      scope,
      serverId: "catalog",
      allowedTools: ["search"],
      source: { universeId: "test", discover },
    });
    if (scope === "session") {
      expect(discover).not.toHaveBeenCalled();
      await button("Customize tools");
    } else {
      expect(container.textContent).not.toContain("Customize tools");
      expect(container.textContent).not.toContain("Hide tool settings");
    }
    expect(discover).toHaveBeenCalledWith("catalog");
    expect(container.textContent).toContain("Find issues");
    expect(container.textContent).toContain("read only");
    if (scope === "session")
      expect(container.textContent).not.toContain("Delete issues");
    else expect(container.textContent).toContain("destructive");
    const search = container.querySelector<HTMLInputElement>(
      '[aria-label="Search MCP tools"]',
    )!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(
        HTMLInputElement.prototype,
        "value",
      )!.set!.call(search, "tickets");
      search.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(container.textContent).toContain("Find issues");
    expect(container.textContent).not.toContain("Delete issues");
    await mode("Selected tools");
    expect(selected).toEqual([]);
    expect(container.querySelector('[role="alert"]')?.textContent).toContain(
      "Select at least one tool",
    );
    await checkbox("search");
    expect(selected).toEqual(["search"]);
    await mode(
      scope === "server" ? "All advertised tools" : "All server-allowed tools",
    );
    expect(selected).toBeUndefined();
    await mode("Selected tools");
    expect(selected).toEqual(["search"]);
    expect(discover).toHaveBeenCalledTimes(1);
  },
);

it.each(["server", "session"] as const)(
  "keeps unavailable %s selections removable",
  async (scope) => {
    await setup({
      scope,
      serverId: "catalog",
      value: ["missing", "denied"],
      allowedTools: ["missing"],
      source: {
        universeId: "test",
        discover: async () => ({
          status: "success",
          tools: [{ name: "denied" }],
        }),
      },
    });
    expect(container.textContent).toContain("Not currently advertised");
    if (scope === "session")
      expect(container.textContent).toContain("No longer allowed");
    await checkbox("missing");
    expect(selected).toEqual(["denied"]);
    await checkbox("denied");
    expect(selected).toEqual([]);
    if (scope === "session")
      expect(container.querySelector('[aria-label="denied"]')).toBeNull();
  },
);

it("preserves selections through structured failures, transport errors, and refreshed inventory", async () => {
  const discover = vi
    .fn<() => Promise<McpToolDiscovery>>()
    .mockResolvedValueOnce({
      status: "failure",
      code: "additionalConsentRequired",
      message: "Consent needed.",
      requiredScopes: ["issues:read"],
    })
    .mockRejectedValueOnce(new Error("Server unavailable"))
    .mockResolvedValueOnce({
      status: "success",
      tools: [{ name: "search" }, { name: "new_tool" }],
    });
  await setup({
    scope: "session",
    serverId: "catalog",
    value: ["search"],
    source: { universeId: "test", discover },
  });
  expect(container.querySelector('[role="alert"]')?.textContent).toContain(
    "Reconnect",
  );
  expect(container.textContent).toContain("issues:read");
  expect(container.textContent).toContain("Not verified");
  expect(selected).toEqual(["search"]);
  await button("Refresh tools");
  expect(container.querySelector('[role="alert"]')?.textContent).toContain(
    "Server unavailable",
  );
  expect(selected).toEqual(["search"]);
  await button("Refresh tools");
  expect(
    container
      .querySelector('[aria-label="new_tool"]')
      ?.getAttribute("aria-checked"),
  ).toBe("false");
  expect(selected).toEqual(["search"]);
});

function deferred() {
  let resolve!: (result: McpToolDiscovery) => void;
  const promise = new Promise<McpToolDiscovery>((complete) => {
    resolve = complete;
  });
  return { promise, resolve };
}

it.each(["universe", "server", "revision"])(
  "discards discovery from an earlier %s",
  async (change) => {
    const first = deferred();
    const second = deferred();
    const discover = vi
      .fn()
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);
    const props: Omit<Props, "onChange"> = {
      scope: "session",
      serverId: "catalog",
      revision: 1,
      value: ["search"],
      source: { universeId: "first", discover },
    };
    await setup(props);
    const next = {
      ...props,
      ...(change === "universe"
        ? { source: { universeId: "second", discover } }
        : {}),
      ...(change === "server" ? { serverId: "other" } : {}),
      ...(change === "revision" ? { revision: 2 } : {}),
    };
    await act(async () => root.render(<Harness {...next} />));
    await act(async () =>
      first.resolve({
        status: "failure",
        code: "unauthorized",
        message: "Stale failure",
      }),
    );
    expect(container.textContent).not.toContain("Stale failure");
    await act(async () =>
      second.resolve({ status: "success", tools: [{ name: "current_tool" }] }),
    );
    expect(container.textContent).toContain("current_tool");
    expect(selected).toEqual(["search"]);
  },
);

it("blocks discovery for unsaved connections and discards an in-flight observation when edited", async () => {
  const request = deferred();
  const discover = vi
    .fn()
    .mockReturnValueOnce(request.promise)
    .mockResolvedValueOnce({
      status: "success",
      tools: [{ name: "saved_connection" }],
    });
  const props: Omit<Props, "onChange"> = {
    scope: "server",
    serverId: "catalog",
    value: ["search"],
    source: { universeId: "test", discover },
    discoveryDisabledReason: "Save connection changes first.",
  };
  await setup(props);
  expect(discover).not.toHaveBeenCalled();
  expect(container.textContent).toContain("Save connection changes first.");
  await act(async () =>
    root.render(<Harness {...props} discoveryDisabledReason={undefined} />),
  );
  expect(discover).toHaveBeenCalledTimes(1);
  await act(async () => root.render(<Harness {...props} />));
  await act(async () =>
    request.resolve({ status: "success", tools: [{ name: "old_connection" }] }),
  );
  expect(container.textContent).not.toContain("old_connection");
  await act(async () =>
    root.render(<Harness {...props} discoveryDisabledReason={undefined} />),
  );
  expect(container.textContent).toContain("saved_connection");
  expect(selected).toEqual(["search"]);
});
