/// Describes a clicked element precisely enough to find it again in the page
/// and in the source: where it is (URL, CSS paths), what it is (tag,
/// attributes, text, role), and which React components and files rendered
/// it. The feedback picker turns this into a plain-text block to paste into
/// an issue or a chat with a coding agent.

export interface ElementReport {
  page: string;
  title: string;
  /// Every ancestor from <html>, like the inspector's "CSS path".
  cssPath: string;
  /// The shortest suffix of that path that still matches only this element.
  selector: string;
  /// The element's opening tag with its attributes.
  element: string;
  text: string;
  value?: string;
  role?: string;
  accessibleName?: string;
  components: string[];
  sourceFiles: string[];
  box: { x: number; y: number; width: number; height: number };
  viewport: { width: number; height: number; scale: number };
  theme: "light" | "dark";
  time: string;
  userAgent: string;
}

export function describeElement(element: Element): ElementReport {
  const rect = element.getBoundingClientRect();
  const react = reactInfo(element);
  return {
    page: window.location.href,
    title: document.title,
    cssPath: cssPath(element),
    selector: uniqueSelector(element),
    element: openingTag(element),
    text: elementText(element),
    value: elementValue(element),
    role: element.getAttribute("role") ?? implicitRole(element),
    accessibleName: accessibleName(element),
    components: react.components,
    sourceFiles: react.sourceFiles,
    box: { x: Math.round(rect.left), y: Math.round(rect.top), width: Math.round(rect.width), height: Math.round(rect.height) },
    viewport: { width: window.innerWidth, height: window.innerHeight, scale: window.devicePixelRatio },
    theme: document.documentElement.classList.contains("dark") ? "dark" : "light",
    time: new Date().toISOString(),
    userAgent: navigator.userAgent,
  };
}

/// The report as `Label: value` lines, with the person's note first.
export function formatReport(report: ElementReport, note = ""): string {
  const lines: Array<[string, string | undefined]> = [
    ["Feedback", note.trim() || undefined],
    ["Page", report.page],
    ["Selector", report.cssPath],
    ["Unique selector", report.selector],
    ["Element text", report.text || "(none)"],
    ["Value", report.value],
    ["Role", report.role],
    ["Accessible name", report.accessibleName],
    ["React components", report.components.length ? report.components.join(" › ") : undefined],
    ["Source files (innermost first)", report.sourceFiles.length ? report.sourceFiles.join(" ← ") : undefined],
    ["Element", report.element],
    ["Box", `${report.box.width}×${report.box.height} at (${report.box.x}, ${report.box.y})`],
    ["Viewport", `${report.viewport.width}×${report.viewport.height} @${report.viewport.scale}x, ${report.theme} theme`],
    ["Title", report.title],
    ["Time", report.time],
    ["User agent", report.userAgent],
  ];
  return lines
    .filter((line): line is [string, string] => line[1] !== undefined && line[1] !== "")
    .map(([label, value]) => `${label}: ${value}`)
    .join("\n");
}

/// One step of a path: the tag, an id when it has one, the data-slot that
/// names shared components, and a position among its siblings when the
/// tag alone is ambiguous. Tailwind class lists are left out: they are long,
/// change with styling, and need escaping.
function segment(element: Element): string {
  const tag = element.tagName.toLowerCase();
  if (element.id) return `${tag}#${CSS.escape(element.id)}`;
  let part = tag;
  const slot = element.getAttribute("data-slot");
  if (slot) part += `[data-slot="${slot}"]`;
  const parent = element.parentElement;
  if (parent && parent.children.length > 1) {
    part += `:nth-child(${Array.prototype.indexOf.call(parent.children, element) + 1})`;
  }
  return part;
}

function ancestry(element: Element): Element[] {
  const chain: Element[] = [];
  for (let node: Element | null = element; node; node = node.parentElement) chain.unshift(node);
  return chain;
}

export function cssPath(element: Element): string {
  return ancestry(element)
    .map((node) => (node === document.documentElement || node === document.body ? node.tagName.toLowerCase() : segment(node)))
    .join(" > ");
}

export function uniqueSelector(element: Element): string {
  const chain = ancestry(element).filter((node) => node !== document.documentElement);
  const parts: string[] = [];
  for (let index = chain.length - 1; index >= 0; index--) {
    const node = chain[index]!;
    parts.unshift(node === document.body ? "body" : segment(node));
    const selector = parts.join(" > ");
    if (node.id || safeCount(selector) === 1) return selector;
  }
  return cssPath(element);
}

function safeCount(selector: string): number {
  try {
    return document.querySelectorAll(selector).length;
  } catch {
    return Number.POSITIVE_INFINITY;
  }
}

const MAX_TAG_LENGTH = 600;

function openingTag(element: Element): string {
  const attributes = Array.from(element.attributes)
    .filter((attribute) => !(element instanceof HTMLInputElement && element.type === "password" && attribute.name === "value"))
    .map((attribute) => (attribute.value === "" ? attribute.name : `${attribute.name}="${attribute.value.replace(/"/g, "&quot;")}"`));
  const tag = `<${[element.tagName.toLowerCase(), ...attributes].join(" ")}>`;
  return tag.length > MAX_TAG_LENGTH ? `${tag.slice(0, MAX_TAG_LENGTH)}…>` : tag;
}

function elementText(element: Element): string {
  // innerText follows rendering (hidden text skipped, line breaks kept); textContent covers DOMs without layout.
  const text = (element instanceof HTMLElement ? element.innerText : undefined) ?? element.textContent ?? "";
  return text.replace(/[ \t]+\n/g, "\n").replace(/\n{3,}/g, "\n\n").trim();
}

function elementValue(element: Element): string | undefined {
  if (element instanceof HTMLInputElement) {
    if (element.type === "password") return element.value ? "(hidden password)" : undefined;
    if (element.type === "checkbox" || element.type === "radio") return element.checked ? "checked" : "unchecked";
    return element.value || undefined;
  }
  if (element instanceof HTMLTextAreaElement || element instanceof HTMLSelectElement) return element.value || undefined;
  return undefined;
}

function implicitRole(element: Element): string | undefined {
  const tag = element.tagName.toLowerCase();
  const roles: Record<string, string> = {
    a: "link", button: "button", nav: "navigation", main: "main", h1: "heading", h2: "heading", h3: "heading",
    h4: "heading", ul: "list", ol: "list", li: "listitem", table: "table", tr: "row", td: "cell", th: "columnheader",
    textarea: "textbox", select: "combobox", img: "img", dialog: "dialog", form: "form", section: "region",
  };
  if (tag === "input") {
    const type = (element as HTMLInputElement).type;
    return type === "checkbox" ? "checkbox" : type === "radio" ? "radio" : type === "button" || type === "submit" ? "button" : "textbox";
  }
  return roles[tag];
}

function accessibleName(element: Element): string | undefined {
  const label = element.getAttribute("aria-label");
  if (label) return label;
  const labelledBy = element.getAttribute("aria-labelledby");
  if (labelledBy) {
    const text = labelledBy
      .split(/\s+/)
      .map((id) => document.getElementById(id)?.textContent?.trim())
      .filter(Boolean)
      .join(" ");
    if (text) return text;
  }
  if (element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement || element instanceof HTMLSelectElement) {
    const labelText = Array.from(element.labels ?? []).map((node) => node.textContent?.trim()).filter(Boolean).join(" ");
    if (labelText) return labelText;
    if ("placeholder" in element && element.placeholder) return element.placeholder;
  }
  return element.getAttribute("title") ?? element.getAttribute("alt") ?? undefined;
}

/// React's internal fiber for a DOM node. Development builds keep component
/// names and, since React 19, the stack where each element was created,
/// which names its source file. Production builds give minified names and
/// no stacks; both lists then stay short or empty.
interface Fiber {
  type: unknown;
  return: Fiber | null;
  _debugStack?: { stack?: string } | null;
}

function fiberOf(element: Element): Fiber | undefined {
  const key = Object.keys(element).find((name) => name.startsWith("__reactFiber$"));
  return key ? (element as unknown as Record<string, Fiber>)[key] : undefined;
}

function componentName(type: unknown): string | undefined {
  if (typeof type === "function") {
    const fn = type as { displayName?: string; name?: string };
    return fn.displayName || fn.name || undefined;
  }
  if (type && typeof type === "object") {
    // memo() and forwardRef() wrap a component; contexts and other objects
    // without one are not components and stay out of the chain.
    const wrapper = type as { displayName?: string; render?: unknown; type?: unknown };
    const inner = wrapper.render ?? wrapper.type;
    if (inner === undefined) return undefined;
    return wrapper.displayName || componentName(inner);
  }
  return undefined;
}

/// The first application file in a JSX creation stack, as a src-relative path.
export function sourceFileFromStack(stack: string | undefined): string | undefined {
  if (!stack) return undefined;
  for (const match of stack.matchAll(/\((?:https?:\/\/[^/)]+)?([^)]*?\/src\/[^)?:]+)/g)) {
    const path = match[1]!;
    if (path.includes("/node_modules/")) continue;
    return path.slice(path.indexOf("/src/") + 1);
  }
  for (const match of stack.matchAll(/(?:https?:\/\/[^/\s]+)?(\S*?\/src\/[^\s?:)]+)/g)) {
    const path = match[1]!;
    if (path.includes("/node_modules/")) continue;
    return path.slice(path.indexOf("/src/") + 1);
  }
  return undefined;
}

const MAX_COMPONENTS = 14;

/// Router and React plumbing that wraps every page and says nothing about
/// which UI was clicked.
const FRAMEWORK_COMPONENTS = new Set(["Suspense", "Routes", "Route", "RenderedRoute", "Outlet", "Router", "BrowserRouter", "MemoryRouter", "FloatingDelayGroup"]);

function reactInfo(element: Element): { components: string[]; sourceFiles: string[] } {
  const components: string[] = [];
  const sourceFiles: string[] = [];
  let fiber: Fiber | null | undefined = fiberOf(element);
  // The host element's own creation stack names the file that wrote its tag.
  const own = sourceFileFromStack(fiber?._debugStack?.stack);
  if (own) sourceFiles.push(own);
  for (; fiber; fiber = fiber.return) {
    const name = componentName(fiber.type);
    if (!name || name.endsWith("Provider") || FRAMEWORK_COMPONENTS.has(name)) continue;
    if (components[0] !== name) components.unshift(name);
    const file = sourceFileFromStack(fiber._debugStack?.stack);
    if (file && !sourceFiles.includes(file) && sourceFiles.length < 4) sourceFiles.push(file);
  }
  return { components: components.slice(-MAX_COMPONENTS), sourceFiles };
}
