/// Live design-token overrides for debug mode. The design-configuration
/// window edits them; every app window applies them as one injected style
/// sheet that redefines the CSS variables from index.css. Overrides persist
/// in localStorage (so a reload keeps them) and travel over a
/// BroadcastChannel (so open windows update while a control moves).

export type ThemeName = "light" | "dark";

export interface ShadowParams {
  /// Hex color of every shadow layer.
  color: string;
  /// Multiplies each layer's alpha.
  opacity: number;
  /// Multiplies each layer's offsets, blur and spread.
  size: number;
}

export interface DesignOverrides {
  /// Variables shared by both themes: radius, spacing, type, shadows.
  shared: Record<string, string>;
  light: Record<string, string>;
  dark: Record<string, string>;
  /// The generator inputs behind the shadow variables, kept to restore the controls.
  shadow?: ShadowParams;
}

export const EMPTY_OVERRIDES: DesignOverrides = { shared: {}, light: {}, dark: {} };

export interface ColorGroup {
  title: string;
  tokens: string[];
}

/// Theme colors from index.css, in the order the editor lists them.
export const COLOR_GROUPS: ColorGroup[] = [
  { title: "Surfaces", tokens: ["--background", "--foreground", "--card", "--card-foreground", "--popover", "--popover-foreground"] },
  {
    title: "Actions and emphasis",
    tokens: [
      "--primary", "--primary-foreground", "--secondary", "--secondary-foreground", "--accent", "--accent-foreground",
      "--muted", "--muted-foreground", "--destructive",
    ],
  },
  { title: "Borders and focus", tokens: ["--border", "--input", "--ring"] },
  {
    title: "Sidebar",
    tokens: [
      "--sidebar", "--sidebar-foreground", "--sidebar-primary", "--sidebar-primary-foreground", "--sidebar-accent",
      "--sidebar-accent-foreground", "--sidebar-border", "--sidebar-ring",
    ],
  },
  { title: "Charts", tokens: ["--chart-1", "--chart-2", "--chart-3", "--chart-4", "--chart-5"] },
];

export const FONT_TOKENS = [
  { token: "--font-sans", label: "Body" },
  { token: "--font-heading", label: "Headings" },
  { token: "--font-mono", label: "Monospace" },
] as const;

export const FONT_CHOICES: Record<string, Array<{ label: string; value: string }>> = {
  sans: [
    { label: "IBM Plex Sans", value: "'IBM Plex Sans Variable', system-ui, sans-serif" },
    { label: "System UI", value: "system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif" },
    { label: "Helvetica / Arial", value: "'Helvetica Neue', Helvetica, Arial, sans-serif" },
    { label: "Georgia (serif)", value: "Georgia, 'Times New Roman', serif" },
    { label: "IBM Plex Mono", value: "'IBM Plex Mono', ui-monospace, monospace" },
  ],
  mono: [
    { label: "IBM Plex Mono", value: "'IBM Plex Mono', ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace" },
    { label: "System monospace", value: "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace" },
    { label: "Consolas", value: "Consolas, 'Courier New', monospace" },
    { label: "Courier", value: "'Courier New', Courier, monospace" },
  ],
};

/// Tailwind's type scale. Line heights are ratios of the size, so they follow.
export const TEXT_SIZE_TOKENS = ["--text-xs", "--text-sm", "--text-base", "--text-lg", "--text-xl", "--text-2xl"] as const;
export const FONT_WEIGHT_TOKENS = ["--font-weight-normal", "--font-weight-medium", "--font-weight-semibold", "--font-weight-bold"] as const;
export const SHADOW_TOKENS = ["--shadow-2xs", "--shadow-xs", "--shadow-sm", "--shadow-md", "--shadow-lg", "--shadow-xl", "--shadow-2xl"] as const;

/// Families of components that share a surface. Each has its own tokens in
/// index.css: `--surface-<key>` (background), `-border` (color),
/// `-border-width`, `-radius` and `-shadow`.
export interface SurfaceRole {
  key: string;
  title: string;
  components: string;
  note?: string;
}

export const SURFACE_ROLES: SurfaceRole[] = [
  { key: "card", title: "Cards", components: "Card" },
  { key: "dialog", title: "Dialogs", components: "Dialog, Alert dialog, Sheet (background only)" },
  { key: "popover", title: "Popovers", components: "Popover" },
  {
    key: "menu",
    title: "Menus",
    components: "Dropdown menu, Select list, Combobox list",
    note: "Menus always render in the dark theme, so their colors take effect under Dark.",
  },
  { key: "field", title: "Inputs", components: "Input, Textarea, Select trigger, Input group, Combobox chips" },
];

export type SurfacePart = "background" | "border" | "border-width" | "radius" | "shadow";

export function surfaceToken(role: string, part: SurfacePart): string {
  return part === "background" ? `--surface-${role}` : `--surface-${role}-${part}`;
}

/// Theme colors a surface background or border can be drawn from.
export const SURFACE_COLOR_SOURCES = [
  "--background", "--card", "--popover", "--muted", "--secondary", "--accent", "--primary", "--sidebar", "--input",
  "--border", "--foreground", "--ring",
] as const;

/// A surface color as a theme color at an opacity, or transparent.
export interface SurfaceColor {
  source: string;
  percent: number;
}

/// Reads the forms surfaceColorValue writes (and index.css uses); any other
/// CSS color is custom and gives null.
export function parseSurfaceColor(value: string): SurfaceColor | null {
  const text = value.trim().replace(/\s+/g, " ");
  if (text === "transparent") return { source: "transparent", percent: 0 };
  const plain = /^var\((--[\w-]+)\)$/.exec(text);
  if (plain) return { source: plain[1]!, percent: 100 };
  const mixed = /^color-mix\(in oklab, ?var\((--[\w-]+)\) ([\d.]+)%, ?transparent\)$/.exec(text);
  return mixed ? { source: mixed[1]!, percent: Number(mixed[2]) } : null;
}

export function surfaceColorValue({ source, percent }: SurfaceColor): string {
  if (source === "transparent" || percent <= 0) return "transparent";
  return percent >= 100 ? `var(${source})` : `color-mix(in oklab, var(${source}) ${percent}%, transparent)`;
}

/// The radius scale from index.css's theme, plus square and pill.
export const RADIUS_STEPS = [
  { label: "None", value: "0" },
  { label: "sm", value: "calc(var(--radius) * 0.6)" },
  { label: "md", value: "calc(var(--radius) * 0.8)" },
  { label: "lg", value: "var(--radius)" },
  { label: "xl", value: "calc(var(--radius) * 1.4)" },
  { label: "2xl", value: "calc(var(--radius) * 1.8)" },
  { label: "3xl", value: "calc(var(--radius) * 2.2)" },
  { label: "4xl", value: "calc(var(--radius) * 2.6)" },
  { label: "Full", value: "9999px" },
];

export const SHADOW_STEPS = [
  { label: "None", value: "0 0 #0000" },
  ...SHADOW_TOKENS.map((token) => ({ label: token.slice("--shadow-".length), value: `var(${token})` })),
];

/// Compares CSS values the way the stylesheet may have spaced them.
export function sameCss(a: string, b: string): boolean {
  const normalize = (value: string) => value.trim().replace(/\s+/g, " ").replace(/(\d)\.0+\b/g, "$1").replace(/\b0\.(\d)/g, ".$1");
  return normalize(a) === normalize(b);
}

/// Fallbacks for variables Tailwind only emits when a utility uses them.
export const FALLBACK_VALUES: Record<string, string> = {
  "--radius": "0.625rem",
  "--spacing": "0.25rem",
  "--text-xs": "0.75rem",
  "--text-sm": "0.875rem",
  "--text-base": "1rem",
  "--text-lg": "1.125rem",
  "--text-xl": "1.25rem",
  "--text-2xl": "1.5rem",
  "--font-weight-normal": "400",
  "--font-weight-medium": "500",
  "--font-weight-semibold": "600",
  "--font-weight-bold": "700",
};

type ShadowLayer = [x: number, y: number, blur: number, spread: number, alpha: number];

/// Tailwind's default shadow scale as layers, the base for the generator.
const SHADOW_LAYERS: Record<(typeof SHADOW_TOKENS)[number], ShadowLayer[]> = {
  "--shadow-2xs": [[0, 1, 0, 0, 0.05]],
  "--shadow-xs": [[0, 1, 2, 0, 0.05]],
  "--shadow-sm": [[0, 1, 3, 0, 0.1], [0, 1, 2, -1, 0.1]],
  "--shadow-md": [[0, 4, 6, -1, 0.1], [0, 2, 4, -2, 0.1]],
  "--shadow-lg": [[0, 10, 15, -3, 0.1], [0, 4, 6, -4, 0.1]],
  "--shadow-xl": [[0, 20, 25, -5, 0.1], [0, 8, 10, -6, 0.1]],
  "--shadow-2xl": [[0, 25, 50, -12, 0.25]],
};

export const DEFAULT_SHADOW: ShadowParams = { color: "#000000", opacity: 1, size: 1 };

export function shadowValues(params: ShadowParams): Record<string, string> {
  const hex = /^#?([0-9a-f]{6})$/i.exec(params.color)?.[1] ?? "000000";
  const [r, g, b] = [0, 2, 4].map((index) => parseInt(hex.slice(index, index + 2), 16));
  const px = (value: number) => (value === 0 ? "0" : `${round(value * params.size)}px`);
  const values: Record<string, string> = {};
  for (const token of SHADOW_TOKENS) {
    values[token] = SHADOW_LAYERS[token]
      .map(([x, y, blur, spread, alpha]) =>
        `${px(x)} ${px(y)} ${px(blur)} ${px(spread)} rgb(${r} ${g} ${b} / ${round(Math.min(1, alpha * params.opacity))})`)
      .join(", ");
  }
  return values;
}

function round(value: number): number {
  return Math.round(value * 1000) / 1000;
}

/// The CSS variables index.css and Tailwind's theme layer define, read from
/// the loaded style sheets: `:root` rules give the light and shared values,
/// `.dark` rules the dark ones. Any injected override sheet is skipped.
export function readDefaults(doc: Document = document): Record<ThemeName, Record<string, string>> {
  const light: Record<string, string> = {};
  const dark: Record<string, string> = {};
  const collect = (style: CSSStyleDeclaration, target: Record<string, string>) => {
    for (const name of Array.from(style)) {
      if (name.startsWith("--")) target[name] = style.getPropertyValue(name).trim();
    }
  };
  // The CSS pipeline splits a rule around each color-mix() into a plain
  // fallback plus an @supports block, as nested rules; later ones win.
  const visitNested = (rules: CSSRuleList, target: Record<string, string>) => {
    for (const rule of Array.from(rules)) {
      if (rule instanceof CSSStyleRule) continue;
      if ("style" in rule) collect((rule as CSSRule & { style: CSSStyleDeclaration }).style, target);
      else if (rule instanceof CSSSupportsRule && CSS.supports(rule.conditionText)) visitNested(rule.cssRules, target);
    }
  };
  const visit = (rules: CSSRuleList) => {
    for (const rule of Array.from(rules)) {
      if (rule instanceof CSSStyleRule) {
        const selectors = rule.selectorText.split(",").map((selector) => selector.trim());
        const target = selectors.includes(":root") ? light : selectors.includes(".dark") ? dark : undefined;
        if (!target) continue;
        collect(rule.style, target);
        if (rule.cssRules) visitNested(rule.cssRules, target);
      } else if ("cssRules" in rule && !(rule instanceof CSSMediaRule)) {
        visit((rule as CSSGroupingRule).cssRules);
      }
    }
  };
  for (const sheet of Array.from(doc.styleSheets)) {
    if (sheet.ownerNode instanceof Element && sheet.ownerNode.id === STYLE_ID) continue;
    try {
      visit(sheet.cssRules);
    } catch {
      // A cross-origin sheet (a web font) cannot be read and holds no tokens.
    }
  }
  for (const [token, value] of Object.entries(FALLBACK_VALUES)) light[token] ??= value;
  return { light, dark: { ...light, ...dark } };
}

function declarations(values: Record<string, string>, indent: string): string {
  return Object.entries(values)
    .map(([token, value]) => `${indent}${token}: ${value};`)
    .join("\n");
}

/// The style sheet app windows inject. The doubled selectors outrank
/// index.css's `:root` and `.dark` rules wherever the sheet lands in <head>;
/// `.dark.dark` also reaches popups that carry their own `dark` class.
export function overridesStyleSheet(overrides: DesignOverrides): string {
  const blocks: string[] = [];
  if (Object.keys(overrides.shared).length) blocks.push(`:root:root {\n${declarations(overrides.shared, "  ")}\n}`);
  if (Object.keys(overrides.light).length) blocks.push(`:root:not(.dark) {\n${declarations(overrides.light, "  ")}\n}`);
  if (Object.keys(overrides.dark).length) blocks.push(`.dark.dark {\n${declarations(overrides.dark, "  ")}\n}`);
  return blocks.join("\n");
}

/// The overrides as rules to paste into index.css.
export function exportCss(overrides: DesignOverrides): string {
  const root = { ...overrides.shared, ...overrides.light };
  const blocks: string[] = [];
  if (Object.keys(root).length) blocks.push(`:root {\n${declarations(root, "    ")}\n}`);
  if (Object.keys(overrides.dark).length) blocks.push(`.dark {\n${declarations(overrides.dark, "    ")}\n}`);
  return blocks.length ? blocks.join("\n\n") : "/* No changes yet. */";
}

export function isEmpty(overrides: DesignOverrides): boolean {
  return !Object.keys(overrides.shared).length && !Object.keys(overrides.light).length && !Object.keys(overrides.dark).length;
}

export const DESIGN_STORAGE_KEY = "lightspeed:design-overrides";
const CHANNEL_NAME = "lightspeed:design-tokens";
const STYLE_ID = "lightspeed-design-overrides";

export type DesignMessage =
  | { type: "overrides"; overrides: DesignOverrides }
  | { type: "ping" }
  | { type: "pong"; id: string; href: string };

export function loadOverrides(): DesignOverrides {
  try {
    const parsed = JSON.parse(localStorage.getItem(DESIGN_STORAGE_KEY) ?? "null") as DesignOverrides | null;
    if (parsed && typeof parsed === "object" && parsed.shared && parsed.light && parsed.dark) return parsed;
  } catch {
    // Missing, unreadable or malformed: start from the stylesheet defaults.
  }
  return EMPTY_OVERRIDES;
}

export function saveOverrides(overrides: DesignOverrides): void {
  try {
    if (isEmpty(overrides)) localStorage.removeItem(DESIGN_STORAGE_KEY);
    else localStorage.setItem(DESIGN_STORAGE_KEY, JSON.stringify(overrides));
  } catch {
    // Without storage the overrides still apply live; they just do not survive a reload.
  }
}

export function openChannel(): BroadcastChannel | null {
  return typeof BroadcastChannel === "undefined" ? null : new BroadcastChannel(CHANNEL_NAME);
}

/// Replaces (or removes) the injected override sheet in this document.
export function applyOverrides(overrides: DesignOverrides, doc: Document = document): void {
  let style = doc.getElementById(STYLE_ID);
  if (isEmpty(overrides)) {
    style?.remove();
    return;
  }
  if (!style) {
    style = doc.createElement("style");
    style.id = STYLE_ID;
  }
  // Re-append so the sheet stays last, after any CSS a lazy chunk added.
  doc.head.append(style);
  style.textContent = overridesStyleSheet(overrides);
}

export const DESIGN_WINDOW_NAME = "lightspeed-design-configuration";

export function designConfigurationUrl(): string {
  return `${import.meta.env.BASE_URL}debug/design-configuration`;
}

export function isDesignConfigurationWindow(): boolean {
  return window.location.pathname.endsWith("/debug/design-configuration");
}
