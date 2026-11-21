// @vitest-environment jsdom
import { act, useRef, type ReactElement, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ReplyCopyMenu } from "./reply-copy-menu";
import { MarkdownContent } from "./markdown-content";

// Exercise our menu actions without Floating UI's popup geometry in jsdom.
vi.mock("@/components/ui/dropdown-menu", async () => {
  const { createContext, useContext, useState, cloneElement } =
    await import("react");
  const Menu = createContext({ open: false, setOpen: (_open: boolean) => {} });
  return {
    DropdownMenu: ({ children }: { children: ReactNode }) => {
      const [open, setOpen] = useState(false);
      return (
        <Menu.Provider value={{ open, setOpen }}>{children}</Menu.Provider>
      );
    },
    DropdownMenuTrigger: ({
      render,
      children,
    }: {
      render: ReactElement;
      children: ReactNode;
    }) => {
      const { open, setOpen } = useContext(Menu);
      return cloneElement(
        render as ReactElement<{ onClick: () => void }>,
        { onClick: () => setOpen(!open) },
        children,
      );
    },
    DropdownMenuContent: ({ children }: { children: ReactNode }) =>
      useContext(Menu).open ? <div role="menu">{children}</div> : null,
    DropdownMenuItem: ({
      children,
      onClick,
      disabled,
    }: {
      children: ReactNode;
      onClick: () => void;
      disabled?: boolean;
    }) => {
      const { setOpen } = useContext(Menu);
      return (
        <button
          role="menuitem"
          disabled={disabled}
          onClick={() => {
            setOpen(false);
            onClick();
          }}
        >
          {children}
        </button>
      );
    },
    DropdownMenuSeparator: () => <hr />,
  };
});

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
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function TestReply({ text, citations }: { text: string; citations?: Array<{ url: string; title: string }> }) {
  const contentRef = useRef<HTMLDivElement>(null);
  return <div>
    <p>Interim progress note.</p>
    <div ref={contentRef}><MarkdownContent>{text}</MarkdownContent></div>
    <ReplyCopyMenu contentRef={contentRef} text={text} citations={citations} />
  </div>;
}
async function render(text = "All fixed.", citations?: Array<{ url: string; title: string }>) {
  await act(async () => root.render(<TestReply text={text} citations={citations} />));
}

async function chooseCopy(label: string) {
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="Copy reply"]')!.click());
  const items = [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')];
  expect(items.map((item) => item.textContent)).toEqual(["Copy as Markdown", "Copy formatted"]);
  await act(async () => items.find((item) => item.textContent === label)!.click());
}

it("copies final Markdown and citations without activity, then confirms success", async () => {
  const writeText = vi.fn().mockResolvedValue(undefined);
  vi.stubGlobal("navigator", { clipboard: { writeText } });
  await render("**All fixed.**", [{ url: "https://example.com", title: "Source" }]);
  await chooseCopy("Copy as Markdown");
  expect(writeText).toHaveBeenCalledWith("**All fixed.**\n\nSources\n\n- [Source](https://example.com/)");
  expect(container.querySelector('[role="status"]')?.textContent).toBe("Copied");
});

it("writes both HTML and plain text for formatted copy, excluding the menu and activity", async () => {
  const write = vi.fn().mockResolvedValue(undefined);
  vi.stubGlobal("navigator", { clipboard: { write } });
  vi.stubGlobal("ClipboardItem", class { constructor(public data: Record<string, Blob>) {} });
  await render("**All fixed.**\n\nNext paragraph.");
  await chooseCopy("Copy formatted");
  const item = write.mock.calls[0]![0][0] as { data: Record<string, Blob> };
  const readBlob = (blob: Blob) => new Promise<string>((resolve) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result as string);
    reader.readAsText(blob);
  });
  expect(await readBlob(item.data["text/html"]!)).toContain("<strong>All fixed.</strong>");
  expect(await readBlob(item.data["text/plain"]!)).toBe("All fixed.\n\nNext paragraph.");
  expect(await readBlob(item.data["text/html"]!)).not.toMatch(/Copy|Interim|cargo/);
});

it("reports clipboard rejection and allows another attempt", async () => {
  const writeText = vi.fn().mockRejectedValueOnce(new Error("denied")).mockResolvedValueOnce(undefined);
  vi.stubGlobal("navigator", { clipboard: { writeText } });
  await render();
  await chooseCopy("Copy as Markdown");
  expect(container.querySelector('[role="status"]')?.textContent).toContain("Could not copy");
  await chooseCopy("Copy as Markdown");
  expect(container.querySelector('[role="status"]')?.textContent).toBe("Copied");
});

