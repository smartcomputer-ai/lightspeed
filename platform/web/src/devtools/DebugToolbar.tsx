import { useCallback, useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { DebugIcon, DesignIcon, DismissIcon, FeedbackIcon } from "@/icons";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { CodeCopy } from "./CodeCopy";
import { describeElement, formatReport, type ElementReport } from "./element-report";
import {
  DESIGN_STORAGE_KEY,
  DESIGN_WINDOW_NAME,
  applyOverrides,
  designConfigurationUrl,
  isDesignConfigurationWindow,
  isEmpty,
  loadOverrides,
  openChannel,
  type DesignMessage,
} from "./design-tokens";

/// Marks the toolbar's own DOM so the picker never selects it.
const DEVTOOLS_ATTRIBUTE = "data-devtools";
const OPEN_KEY = "lightspeed:debug-toolbar";

/// The debug toolbar: Ctrl+D shows or hides it in the bottom-right corner.
/// Feedback picks an element on the page and opens a report about it to
/// copy; Design opens the design-token editor window, whose changes every
/// app window applies live, toolbar shown or not. Mounted by main.tsx only
/// when debug mode is enabled. The editor window itself gets neither.
export default function DebugToolbar() {
  return isDesignConfigurationWindow() ? null : <Toolbar />;
}

function Toolbar() {
  const designActive = useDesignOverrides();
  const [open, setOpen] = useState(() => readOpen());
  const [picking, setPicking] = useState(false);
  const [report, setReport] = useState<ElementReport | null>(null);

  const show = useCallback((next: boolean) => {
    setOpen(next);
    if (!next) setPicking(false);
    try {
      sessionStorage.setItem(OPEN_KEY, next ? "1" : "0");
    } catch {
      // Storage can be unavailable (private mode); the toolbar still works.
    }
  }, []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (!event.ctrlKey || event.altKey || event.shiftKey || event.metaKey) return;
      if (event.key.toLowerCase() !== "d") return;
      event.preventDefault();
      if (!event.repeat) show(!open);
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [open, show]);

  return createPortal(
    <>
      {open && (
        <div
          {...{ [DEVTOOLS_ATTRIBUTE]: "" }}
          role="toolbar"
          aria-label="Debug toolbar"
          className="fixed right-4 bottom-4 z-[2147483000] flex items-center gap-1 rounded-xl border bg-popover p-1 text-popover-foreground shadow-lg"
        >
          <span className="flex items-center gap-1.5 px-2 text-xs font-medium text-muted-foreground">
            <DebugIcon className="size-3.5" />
            Debug
          </span>
          <Button
            size="sm"
            variant={picking ? "secondary" : "ghost"}
            aria-pressed={picking}
            title="Pick an element to report (Esc cancels)"
            onClick={() => setPicking(!picking)}
          >
            <FeedbackIcon data-icon="inline-start" />
            Feedback
          </Button>
          <Button
            size="sm"
            variant="ghost"
            title={designActive ? "Edit design tokens (overrides active)" : "Edit design tokens"}
            onClick={() => window.open(designConfigurationUrl(), DESIGN_WINDOW_NAME, "popup,width=600,height=900")?.focus()}
          >
            <DesignIcon data-icon="inline-start" />
            Design
            {designActive && <span className="size-1.5 rounded-full bg-blue-500" aria-label="overrides active" />}
          </Button>
          <Button size="icon-sm" variant="ghost" aria-label="Close debug toolbar (Ctrl+D)" title="Close (Ctrl+D)" onClick={() => show(false)}>
            <DismissIcon />
          </Button>
        </div>
      )}
      {picking && (
        <ElementPicker
          onPick={(element) => {
            setPicking(false);
            setReport(describeElement(element));
          }}
          onCancel={() => setPicking(false)}
        />
      )}
      <FeedbackDialog
        report={report}
        onClose={() => setReport(null)}
        onPickAnother={() => {
          setReport(null);
          setPicking(true);
        }}
      />
    </>,
    document.body,
  );
}

/// Applies the saved design-token overrides now and every change the editor
/// publishes later, and answers the editor's pings so it can show where its
/// changes land. Returns whether any override is in effect.
function useDesignOverrides(): boolean {
  const [active, setActive] = useState(() => !isEmpty(loadOverrides()));
  useEffect(() => {
    const id = Math.random().toString(36).slice(2);
    applyOverrides(loadOverrides());
    const channel = openChannel();
    if (channel) {
      channel.onmessage = (event: MessageEvent<DesignMessage>) => {
        const message = event.data;
        if (message.type === "overrides") {
          applyOverrides(message.overrides);
          setActive(!isEmpty(message.overrides));
        } else if (message.type === "ping") {
          channel.postMessage({ type: "pong", id, href: window.location.href } satisfies DesignMessage);
        }
      };
    }
    // Another tab of this origin changed or cleared the saved overrides.
    const onStorage = (event: StorageEvent) => {
      if (event.key !== null && event.key !== DESIGN_STORAGE_KEY) return;
      const overrides = loadOverrides();
      applyOverrides(overrides);
      setActive(!isEmpty(overrides));
    };
    window.addEventListener("storage", onStorage);
    return () => {
      channel?.close();
      window.removeEventListener("storage", onStorage);
    };
  }, []);
  return active;
}

function readOpen(): boolean {
  try {
    return sessionStorage.getItem(OPEN_KEY) === "1";
  } catch {
    return false;
  }
}

function isDevtools(target: EventTarget | null): boolean {
  return target instanceof Element && target.closest(`[${DEVTOOLS_ATTRIBUTE}]`) !== null;
}

/// Selection mode: outlines the element under the pointer and reports the
/// one clicked. Listeners sit on window in the capture phase, ahead of the
/// app's own handlers and Base UI's outside-press detection, so the click
/// selects without activating anything, and elements inside open menus and
/// popovers can be picked too.
function ElementPicker({ onPick, onCancel }: { onPick: (element: Element) => void; onCancel: () => void }) {
  const [target, setTarget] = useState<Element | null>(null);
  const [, setFrame] = useState(0);

  useEffect(() => {
    const block = (event: Event) => {
      if (isDevtools(event.target)) return;
      event.preventDefault();
      event.stopPropagation();
      event.stopImmediatePropagation();
    };
    const onMove = (event: PointerEvent) => {
      const element = document.elementFromPoint(event.clientX, event.clientY);
      setTarget(element && !isDevtools(element) ? element : null);
    };
    const onClick = (event: MouseEvent) => {
      if (isDevtools(event.target)) return;
      block(event);
      const element = document.elementFromPoint(event.clientX, event.clientY);
      if (element && !isDevtools(element)) onPick(element);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      onCancel();
    };
    // The outline follows its element while the page scrolls or resizes.
    const onLayout = () => setFrame((frame) => frame + 1);
    const pressEvents = ["pointerdown", "mousedown", "pointerup", "mouseup", "dblclick", "contextmenu", "auxclick"] as const;
    window.addEventListener("pointermove", onMove, true);
    window.addEventListener("click", onClick, true);
    window.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("scroll", onLayout, true);
    window.addEventListener("resize", onLayout);
    for (const name of pressEvents) window.addEventListener(name, block, true);
    const cursor = document.createElement("style");
    cursor.textContent = `*:not([${DEVTOOLS_ATTRIBUTE}] *) { cursor: crosshair !important; }`;
    document.head.append(cursor);
    return () => {
      window.removeEventListener("pointermove", onMove, true);
      window.removeEventListener("click", onClick, true);
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("scroll", onLayout, true);
      window.removeEventListener("resize", onLayout);
      for (const name of pressEvents) window.removeEventListener(name, block, true);
      cursor.remove();
    };
  }, [onPick, onCancel]);

  const rect = target?.isConnected ? target.getBoundingClientRect() : null;
  return (
    <div {...{ [DEVTOOLS_ATTRIBUTE]: "" }} className="pointer-events-none fixed inset-0 z-[2147482999]">
      <div className="absolute top-3 left-1/2 -translate-x-1/2 rounded-full bg-blue-600 px-3 py-1 text-xs font-medium text-white shadow-lg">
        Click an element to report it · Esc to cancel
      </div>
      {rect && target && (
        <>
          <div
            className="absolute rounded-[2px] border-2 border-blue-500 bg-blue-500/10"
            style={{ left: rect.left, top: rect.top, width: rect.width, height: rect.height }}
          />
          <div
            className="absolute max-w-[min(32rem,90vw)] truncate rounded bg-blue-600 px-1.5 py-0.5 font-mono text-[11px] text-white shadow"
            style={{ left: Math.max(4, rect.left), top: rect.top > 24 ? rect.top - 22 : rect.bottom + 4 }}
          >
            {pickerLabel(target)} · {Math.round(rect.width)}×{Math.round(rect.height)}
          </div>
        </>
      )}
    </div>
  );
}

function pickerLabel(element: Element): string {
  const tag = element.tagName.toLowerCase();
  const slot = element.getAttribute("data-slot");
  return element.id ? `${tag}#${element.id}` : slot ? `${tag}[data-slot=${slot}]` : tag;
}

function FeedbackDialog({ report, onClose, onPickAnother }: {
  report: ElementReport | null;
  onClose: () => void;
  onPickAnother: () => void;
}) {
  return (
    <Dialog open={report !== null} onOpenChange={(next) => !next && onClose()}>
      <DialogContent {...{ [DEVTOOLS_ATTRIBUTE]: "" }} className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>Element feedback</DialogTitle>
        </DialogHeader>
        {report && <CodeCopy title="Report" code={formatReport(report)} />}
        <DialogFooter>
          <Button variant="outline" onClick={onPickAnother}>Pick another element</Button>
          <Button onClick={onClose}>Done</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
