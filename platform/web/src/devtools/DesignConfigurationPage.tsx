import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { AddIcon, DeleteIcon, ExportCodeIcon, ResetIcon, SearchIcon, SettingsIcon } from "@/icons";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { useTheme } from "@/lib/theme";
import { cn } from "@/lib/utils";
import { CodeCopy } from "./CodeCopy";
import {
  COLOR_GROUPS,
  DEFAULT_SHADOW,
  EMPTY_OVERRIDES,
  FONT_CHOICES,
  FONT_TOKENS,
  FONT_WEIGHT_TOKENS,
  RADIUS_STEPS,
  SHADOW_STEPS,
  SHADOW_TOKENS,
  BACKDROP_KEY,
  SHEEN_ROLES,
  SIDEBAR_LOGO_TOKENS,
  SURFACE_COLOR_SOURCES,
  SURFACE_ROLES,
  TEXT_SIZE_TOKENS,
  exportCss,
  isEmpty,
  loadOverrides,
  openChannel,
  parseSurfaceColor,
  readDefaults,
  sameCss,
  saveOverrides,
  shadowValues,
  surfaceColorValue,
  sheenToken,
  surfaceToken,
  type DesignMessage,
  type DesignOverrides,
  type ShadowParams,
  type SheenRole,
  type SurfaceRole,
  type ThemeName,
} from "./design-tokens";
import { LightspeedMark } from "@/components/brand/lightspeed-logo";

type Scope = "shared" | ThemeName;

/// The design-token editor, opened in its own window from the debug
/// toolbar's Design button. Every change is saved and broadcast at once, and
/// each open app window applies it live. This window keeps the stock tokens
/// so its own controls stay put while the app changes.
export function DesignConfigurationPage() {
  const defaults = useMemo(() => readDefaults(), []);
  const [overrides, setOverrides] = useState<DesignOverrides>(loadOverrides);
  const channel = useRef<BroadcastChannel | null>(null);
  const windows = useLiveWindows(channel, setOverrides);
  const { theme } = useTheme();
  const [colorTheme, setColorTheme] = useState<ThemeName>(() =>
    theme === "dark" || (theme === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches) ? "dark" : "light");
  const [exportOpen, setExportOpen] = useState(false);

  useEffect(() => {
    document.title = "Design tokens · Lightspeed";
  }, []);

  const publish = (next: DesignOverrides) => {
    setOverrides(next);
    saveOverrides(next);
    channel.current?.postMessage({ type: "overrides", overrides: next } satisfies DesignMessage);
  };

  const defaultOf = (scope: Scope, token: string) => (scope === "dark" ? defaults.dark : defaults.light)[token] ?? "";
  const valueOf = (scope: Scope, token: string) => overrides[scope][token] ?? defaultOf(scope, token);
  const setToken = (scope: Scope, token: string, value: string | undefined) => {
    const values = { ...overrides[scope] };
    if (value === undefined || sameCss(value, defaultOf(scope, token))) delete values[token];
    else values[token] = value.trim();
    publish({ ...overrides, [scope]: values });
  };
  const token = (scope: Scope, name: string) => ({
    value: valueOf(scope, name),
    changed: name in overrides[scope],
    onChange: (value: string) => setToken(scope, name, value),
    onReset: () => setToken(scope, name, undefined),
  });

  const changes = Object.keys(overrides.shared).length + Object.keys(overrides.light).length + Object.keys(overrides.dark).length;

  return (
    <div className="flex min-h-svh flex-col bg-background text-foreground">
      <header className="sticky top-0 z-10 grid gap-2 border-b bg-background/95 px-4 py-3 backdrop-blur">
        <div className="flex items-center justify-between gap-2">
          <div className="grid min-w-0">
            <h1 className="text-base font-semibold">Design tokens</h1>
            <p className="truncate text-xs text-muted-foreground" title={windows.join("\n")}>
              {windows.length
                ? `Live in ${windows.length} app window${windows.length === 1 ? "" : "s"}`
                : "No app window connected. Open one with debug mode on."}
            </p>
          </div>
          <div className="flex shrink-0 items-center gap-1.5">
            {changes > 0 && <Badge variant="secondary">{changes} changed</Badge>}
            <Button size="sm" variant="outline" onClick={() => setExportOpen(true)}>
              <ExportCodeIcon data-icon="inline-start" />
              Export CSS
            </Button>
            <Button size="sm" variant="ghost" disabled={isEmpty(overrides)} onClick={() => publish(EMPTY_OVERRIDES)}>
              Reset all
            </Button>
          </div>
        </div>
      </header>

      <Tabs defaultValue="colors" className="min-h-0 flex-1 px-4 py-3">
        <TabsList className="w-full">
          <TabsTrigger value="colors">Colors</TabsTrigger>
          <TabsTrigger value="type">Type</TabsTrigger>
          <TabsTrigger value="surfaces">Surfaces</TabsTrigger>
          <TabsTrigger value="shape">Shape</TabsTrigger>
          <TabsTrigger value="shadows">Shadows</TabsTrigger>
        </TabsList>

        <TabsContent value="colors" className="grid content-start gap-6 pt-3">
          <ThemeToggle value={colorTheme} onChange={setColorTheme}>
            Each theme has its own colors. Any CSS color works, including oklch().
          </ThemeToggle>
          {COLOR_GROUPS.map((group) => (
            <Group key={group.title} title={group.title}>
              {group.tokens.map((name) => (
                <ColorRow key={`${colorTheme}${name}`} name={name} {...token(colorTheme, name)} />
              ))}
            </Group>
          ))}
        </TabsContent>

        <TabsContent value="type" className="grid content-start gap-6 pt-3">
          <Group title="Font families">
            {FONT_TOKENS.map((font) => (
              <FontRow
                key={font.token}
                name={font.token}
                label={font.label}
                choices={font.token === "--font-mono" ? FONT_CHOICES.mono! : FONT_CHOICES.sans!}
                {...token("shared", font.token)}
              />
            ))}
          </Group>
          <Group title="Font sizes" note="Line heights scale with each size.">
            {TEXT_SIZE_TOKENS.map((name) => (
              <RangeRow
                key={name}
                name={name}
                min={0.5}
                max={2.5}
                step={0.0125}
                unit="rem"
                describe={(rem) => `${round(rem * 16)}px`}
                sample={(value) => <span style={{ fontSize: value }} className="leading-none">Aa</span>}
                {...token("shared", name)}
              />
            ))}
          </Group>
          <Group title="Font weights" note="IBM Plex Sans is variable, so any weight from 100 to 700 renders.">
            {FONT_WEIGHT_TOKENS.map((name) => (
              <RangeRow
                key={name}
                name={name}
                min={100}
                max={900}
                step={10}
                unit=""
                sample={(value) => <span style={{ fontWeight: value }}>Aa</span>}
                {...token("shared", name)}
              />
            ))}
          </Group>
        </TabsContent>

        <TabsContent value="surfaces" className="grid content-start gap-6 pt-3">
          <ThemeToggle value={colorTheme} onChange={setColorTheme}>
            Which colors each family of components is drawn with, and its border, corners and shadow. Colors are per theme;
            the rest applies to both.
          </ThemeToggle>
          {SURFACE_ROLES.map((role) => (
            <SurfaceEditor
              key={`${colorTheme}${role.key}`}
              role={role}
              resolve={(name) => valueOf(colorTheme, name)}
              background={token(colorTheme, surfaceToken(role.key, "background"))}
              border={token(colorTheme, surfaceToken(role.key, "border"))}
              borderWidth={token("shared", surfaceToken(role.key, "border-width"))}
              radius={token("shared", surfaceToken(role.key, "radius"))}
              shadow={token("shared", surfaceToken(role.key, "shadow"))}
              blur={role.effects ? token("shared", surfaceToken(role.key, "blur")) : undefined}
              saturate={role.effects ? token("shared", surfaceToken(role.key, "saturate")) : undefined}
            />
          ))}
          {SHEEN_ROLES.map((role) => (
            <SheenEditor
              key={`${colorTheme}${role.key}`}
              role={role}
              resolve={(name) => valueOf(colorTheme, name)}
              color={token(colorTheme, sheenToken(role.key))}
              start={token(colorTheme, sheenToken(role.key, "start"))}
            />
          ))}
          <SidebarLogoEditor
            key={`${colorTheme}sidebar-logo`}
            resolve={(name) => valueOf(colorTheme, name)}
            color={token(colorTheme, SIDEBAR_LOGO_TOKENS.color)}
            opacity={token(colorTheme, SIDEBAR_LOGO_TOKENS.opacity)}
            rotate={token("shared", SIDEBAR_LOGO_TOKENS.rotate)}
            scale={token("shared", SIDEBAR_LOGO_TOKENS.scale)}
            offset={token("shared", SIDEBAR_LOGO_TOKENS.offset)}
          />
          <BackdropEditor
            key={`${colorTheme}backdrop`}
            resolve={(name) => valueOf(colorTheme, name)}
            color={token(colorTheme, surfaceToken(BACKDROP_KEY, "background"))}
            blur={token("shared", surfaceToken(BACKDROP_KEY, "blur"))}
            saturate={token("shared", surfaceToken(BACKDROP_KEY, "saturate"))}
          />
        </TabsContent>

        <TabsContent value="shape" className="grid content-start gap-6 pt-3">
          <Group title="Corner radius" note="One base value; rounded-sm to rounded-4xl are multiples of it.">
            <RangeRow
              name="--radius"
              min={0}
              max={1.5}
              step={0.025}
              unit="rem"
              describe={(rem) => `${round(rem * 16)}px`}
              {...token("shared", "--radius")}
            />
            <div className="flex flex-wrap gap-3 pt-1">
              {[0.6, 0.8, 1, 1.4, 1.8].map((factor) => (
                <div
                  key={factor}
                  className="size-12 border-2 border-primary bg-muted"
                  style={{ borderRadius: `calc(${valueOf("shared", "--radius")} * ${factor})` }}
                  title={`× ${factor}`}
                />
              ))}
            </div>
          </Group>
          <Group title="Spacing" note="Every padding, margin, gap, width and height utility is a multiple of this unit.">
            <RangeRow
              name="--spacing"
              min={0.15}
              max={0.4}
              step={0.005}
              unit="rem"
              describe={(rem) => `${round(rem * 16)}px`}
              {...token("shared", "--spacing")}
            />
          </Group>
          <Group title="Icons" note="Scales every icon from the size its classes give it. Text stays the same size.">
            <RangeRow
              name="--icon-scale"
              min={0.5}
              max={2}
              step={0.05}
              unit=""
              describe={(scale) => `${round(scale * 16)}px at size-4`}
              {...token("shared", "--icon-scale")}
            />
            <div className="flex flex-wrap items-center gap-4 pt-1 text-sm">
              {[
                { Icon: SearchIcon, label: "Search" },
                { Icon: SettingsIcon, label: "Settings" },
                { Icon: AddIcon, label: "New" },
                { Icon: DeleteIcon, label: "Delete" },
              ].map(({ Icon, label }) => (
                <span key={label} className="flex items-center gap-1.5">
                  <Icon className="size-4" style={{ zoom: valueOf("shared", "--icon-scale") }} />
                  {label}
                </span>
              ))}
            </div>
          </Group>
          <Group title="Borders">
            <p className="text-xs text-muted-foreground">
              Cards, dialogs, popovers, menus and inputs have their own border width and color under Surfaces. Other borders
              (dividers, table rows, sidebar edges) are 1px in the component classes and take their color from Colors › Borders
              and focus.
            </p>
          </Group>
        </TabsContent>

        <TabsContent value="shadows" className="grid content-start gap-6 pt-3">
          <ShadowEditor
            params={overrides.shadow ?? DEFAULT_SHADOW}
            onChange={(params) => {
              const shared = { ...overrides.shared };
              for (const name of SHADOW_TOKENS) delete shared[name];
              const isDefault = params.color.toLowerCase() === DEFAULT_SHADOW.color && params.opacity === 1 && params.size === 1;
              if (isDefault) {
                publish({ ...overrides, shared, shadow: undefined });
              } else {
                publish({ ...overrides, shared: { ...shared, ...shadowValues(params) }, shadow: params });
              }
            }}
            values={Object.fromEntries(SHADOW_TOKENS.map((name) => [name, valueOf("shared", name)]))}
          />
        </TabsContent>
      </Tabs>

      <Dialog open={exportOpen} onOpenChange={setExportOpen}>
        <DialogContent className="sm:max-w-xl">
          <DialogHeader>
            <DialogTitle>Export CSS</DialogTitle>
            <DialogDescription>The changed tokens as rules for platform/web/src/index.css.</DialogDescription>
          </DialogHeader>
          <CodeCopy title="index.css" code={exportCss(overrides)} />
        </DialogContent>
      </Dialog>
    </div>
  );
}

/// Pings app windows over the channel and lists the ones that answered in
/// the last few seconds. Overrides published by another editor window are
/// taken over, so two editors stay in step.
function useLiveWindows(
  channel: React.RefObject<BroadcastChannel | null>,
  onOverrides: (overrides: DesignOverrides) => void,
): string[] {
  const [seen, setSeen] = useState<Record<string, { href: string; at: number }>>({});
  useEffect(() => {
    const opened = openChannel();
    channel.current = opened;
    if (!opened) return;
    opened.onmessage = (event: MessageEvent<DesignMessage>) => {
      const message = event.data;
      if (message.type === "pong") setSeen((current) => ({ ...current, [message.id]: { href: message.href, at: Date.now() } }));
      if (message.type === "overrides") onOverrides(message.overrides);
    };
    const ping = () => {
      opened.postMessage({ type: "ping" } satisfies DesignMessage);
      setSeen((current) => Object.fromEntries(Object.entries(current).filter(([, entry]) => Date.now() - entry.at < 5_000)));
    };
    ping();
    const timer = setInterval(ping, 2_000);
    return () => {
      clearInterval(timer);
      opened.close();
      channel.current = null;
    };
  }, [channel, onOverrides]);
  return Object.values(seen).map((entry) => entry.href);
}

function Group({ title, note, children }: { title: string; note?: string; children: ReactNode }) {
  return (
    <section className="grid gap-2.5">
      <div className="grid gap-0.5">
        <h2 className="text-xs font-medium tracking-wide text-muted-foreground uppercase">{title}</h2>
        {note && <p className="text-xs text-muted-foreground">{note}</p>}
      </div>
      {children}
    </section>
  );
}

interface TokenProps {
  name: string;
  value: string;
  changed: boolean;
  onChange: (value: string) => void;
  onReset: () => void;
}

function TokenName({ name, changed }: { name: string; changed: boolean }) {
  return (
    <span className={cn("truncate font-mono text-xs", changed ? "font-semibold text-foreground" : "text-muted-foreground")} title={name}>
      {name}
    </span>
  );
}

function ResetButton({ changed, onReset, name }: { changed: boolean; onReset: () => void; name: string }) {
  return (
    <Button
      size="icon-xs"
      variant="ghost"
      aria-label={`Reset ${name}`}
      title="Reset to the stylesheet value"
      className={cn(!changed && "invisible")}
      onClick={onReset}
    >
      <ResetIcon />
    </Button>
  );
}

function ColorRow({ name, value, changed, onChange, onReset }: TokenProps) {
  const [draft, setDraft] = useState(value);
  useEffect(() => setDraft(value), [value]);
  const valid = CSS.supports("color", draft);
  const rgba = toRgba(value);
  return (
    <div className="grid grid-cols-[2rem_minmax(0,1fr)_minmax(0,1.3fr)_1.5rem] items-center gap-2">
      <span className="relative size-8 overflow-hidden rounded-md border" style={{ background: value }}>
        <input
          type="color"
          aria-label={`Pick ${name}`}
          value={rgba ? toHex(rgba) : "#000000"}
          onChange={(event) => onChange(withAlpha(event.target.value, rgba?.[3] ?? 255))}
          className="absolute inset-0 size-full cursor-pointer opacity-0"
        />
      </span>
      <TokenName name={name} changed={changed} />
      <Input
        value={draft}
        aria-label={name}
        aria-invalid={!valid}
        spellCheck={false}
        className="h-8 font-mono text-xs"
        onChange={(event) => {
          setDraft(event.target.value);
          if (CSS.supports("color", event.target.value)) onChange(event.target.value);
        }}
        onBlur={() => setDraft(value)}
      />
      <ResetButton changed={changed} onReset={onReset} name={name} />
    </div>
  );
}

function FontRow({ name, label, choices, value, changed, onChange, onReset }: TokenProps & {
  label: string;
  choices: Array<{ label: string; value: string }>;
}) {
  const [draft, setDraft] = useState(value);
  useEffect(() => setDraft(value), [value]);
  const normalize = (stack: string) => stack.replace(/["']/g, "").replace(/\s*,\s*/g, ",").toLowerCase();
  const selected = choices.find((choice) => normalize(choice.value) === normalize(value))?.value ?? "custom";
  return (
    <div className="grid gap-1.5 rounded-lg border p-2.5">
      <div className="flex items-center justify-between gap-2">
        <div className="flex min-w-0 items-baseline gap-2">
          <span className="text-sm font-medium">{label}</span>
          <TokenName name={name} changed={changed} />
        </div>
        <ResetButton changed={changed} onReset={onReset} name={name} />
      </div>
      <div className="grid grid-cols-[10rem_minmax(0,1fr)] gap-2">
        <Select value={selected} onValueChange={(next) => next && next !== "custom" && onChange(next)}>
          <SelectTrigger size="sm" aria-label={`${label} font`}><SelectValue /></SelectTrigger>
          <SelectContent>
            {choices.map((choice) => (
              <SelectItem key={choice.label} value={choice.value}>{choice.label}</SelectItem>
            ))}
            <SelectItem value="custom" disabled>Custom</SelectItem>
          </SelectContent>
        </Select>
        <Input
          value={draft}
          aria-label={`${label} font stack`}
          spellCheck={false}
          className="h-8 font-mono text-xs"
          onChange={(event) => setDraft(event.target.value)}
          onBlur={() => draft.trim() && onChange(draft)}
          onKeyDown={(event) => event.key === "Enter" && draft.trim() && onChange(draft)}
        />
      </div>
      <p className="truncate text-sm" style={{ fontFamily: value }}>The quick brown fox jumps over the lazy dog 0123456789</p>
    </div>
  );
}

function RangeRow({ name, value, changed, onChange, onReset, min, max, step, unit, describe, sample }: TokenProps & {
  min: number;
  max: number;
  step: number;
  unit: "rem" | "px" | "%" | "deg" | "";
  describe?: (value: number) => string;
  sample?: (value: string) => ReactNode;
}) {
  const number = Number.parseFloat(value);
  const current = Number.isFinite(number) ? number : min;
  return (
    <div className="grid grid-cols-[minmax(0,10rem)_minmax(0,1fr)_4.5rem_2.5rem_1.5rem] items-center gap-2">
      <TokenName name={name} changed={changed} />
      <input
        type="range"
        aria-label={name}
        min={min}
        max={max}
        step={step}
        value={current}
        className="w-full accent-primary"
        onChange={(event) => onChange(`${round(Number(event.target.value))}${unit}`)}
      />
      <span className="text-right font-mono text-xs tabular-nums" title={value}>
        {round(current)}{unit}
        {describe && <span className="block text-[10px] text-muted-foreground">{describe(current)}</span>}
      </span>
      <span className="flex justify-center">{sample?.(value)}</span>
      <ResetButton changed={changed} onReset={onReset} name={name} />
    </div>
  );
}

function ThemeToggle({ value, onChange, children }: { value: ThemeName; onChange: (theme: ThemeName) => void; children: ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-3">
      <p className="text-xs text-muted-foreground">{children}</p>
      <div className="flex shrink-0 gap-1 rounded-lg bg-muted p-1" role="radiogroup" aria-label="Theme to edit">
        {(["light", "dark"] as const).map((name) => (
          <Button
            key={name}
            size="xs"
            role="radio"
            aria-checked={value === name}
            variant={value === name ? "outline" : "ghost"}
            onClick={() => onChange(name)}
          >
            {name === "light" ? "Light" : "Dark"}
          </Button>
        ))}
      </div>
    </div>
  );
}

type TokenControl = Omit<TokenProps, "name">;

/// One family of components: its background and border color (each a theme
/// color at an opacity, or any CSS color), border width, radius and shadow.
function SurfaceEditor({ role, resolve, background, border, borderWidth, radius, shadow, blur, saturate }: {
  role: SurfaceRole;
  resolve: (token: string) => string;
  background: TokenControl;
  border: TokenControl;
  borderWidth: TokenControl;
  radius: TokenControl;
  shadow: TokenControl;
  blur?: TokenControl;
  saturate?: TokenControl;
}) {
  return (
    <section className="grid gap-3 rounded-lg border p-3">
      <div className="flex items-start justify-between gap-3">
        <div className="grid min-w-0 gap-0.5">
          <h2 className="text-sm font-medium">{role.title}</h2>
          <p className="text-xs text-muted-foreground">{role.components}</p>
          {role.note && <p className="text-xs text-muted-foreground">{role.note}</p>}
        </div>
        <div
          aria-hidden
          className="grid h-12 w-20 shrink-0 place-items-center text-xs"
          style={{
            background: literalColor(background.value, resolve) ?? background.value,
            border: `${borderWidth.value} solid ${literalColor(border.value, resolve) ?? border.value}`,
            borderRadius: radius.value,
            boxShadow: shadow.value,
          }}
        >
          Aa
        </div>
      </div>
      <SurfaceColorRow label="Background" name={surfaceToken(role.key, "background")} resolve={resolve} {...background} />
      <SurfaceColorRow label="Border" name={surfaceToken(role.key, "border")} resolve={resolve} {...border} />
      <RangeRow name={surfaceToken(role.key, "border-width")} min={0} max={4} step={0.5} unit="px" {...borderWidth} />
      <StepRow label="Radius" name={surfaceToken(role.key, "radius")} steps={RADIUS_STEPS} {...radius} />
      <StepRow label="Shadow" name={surfaceToken(role.key, "shadow")} steps={SHADOW_STEPS} {...shadow} />
      {blur && saturate && <EffectRows role={role.key} blur={blur} saturate={saturate} />}
    </section>
  );
}

/// The backdrop filter behind a translucent surface: how much the content
/// behind it is blurred and how saturated it stays.
function EffectRows({ role, blur, saturate }: { role: string; blur: TokenControl; saturate: TokenControl }) {
  return (
    <>
      <RangeRow name={surfaceToken(role, "blur")} min={0} max={64} step={1} unit="px" {...blur} />
      <RangeRow name={surfaceToken(role, "saturate")} min={0} max={300} step={5} unit="%" {...saturate} />
    </>
  );
}

/// A sheen over a surface: the gradient's end color and its opacity.
function SheenEditor({ role, resolve, color, start }: {
  role: SheenRole;
  resolve: (token: string) => string;
  color: TokenControl;
  start: TokenControl;
}) {
  const end = literalColor(color.value, resolve) ?? color.value;
  return (
    <section className="grid gap-3 rounded-lg border p-3">
      <div className="flex items-start justify-between gap-3">
        <div className="grid min-w-0 gap-0.5">
          <h2 className="text-sm font-medium">{role.title}</h2>
          <p className="text-xs text-muted-foreground">{role.components}</p>
          <p className="text-xs text-muted-foreground">
            A left-to-right gradient that stays transparent up to the start, then rises to this color at the right edge. Each
            theme has its own.
          </p>
        </div>
        <div
          aria-hidden
          className="h-12 w-20 shrink-0 rounded-md border"
          style={{
            backgroundColor: resolve(role.over),
            backgroundImage: `linear-gradient(90deg, transparent ${start.value}, ${end} 100%)`,
          }}
        />
      </div>
      <SurfaceColorRow label="Color" name={sheenToken(role.key)} resolve={resolve} {...color} />
      <RangeRow name={sheenToken(role.key, "start")} min={0} max={100} step={1} unit="%" {...start} />
    </section>
  );
}

/// The large LS mark behind the bottom of the sidebar, with a preview the
/// size of a narrow sidebar.
function SidebarLogoEditor({ resolve, color, opacity, rotate, scale, offset }: {
  resolve: (token: string) => string;
  color: TokenControl;
  opacity: TokenControl;
  rotate: TokenControl;
  scale: TokenControl;
  offset: TokenControl;
}) {
  return (
    <section className="grid gap-3 rounded-lg border p-3">
      <div className="flex items-start justify-between gap-3">
        <div className="grid min-w-0 gap-0.5">
          <h2 className="text-sm font-medium">Sidebar logo</h2>
          <p className="text-xs text-muted-foreground">
            The LS mark behind the lower part of the sidebar. Color and opacity are per theme; rotation, scale (times the
            sidebar width) and the bottom offset (percent of its own height, pushing it below the edge) apply to both.
          </p>
        </div>
        <div
          aria-hidden
          className="relative h-28 w-16 shrink-0 overflow-hidden rounded-md border"
          style={{ background: resolve("--sidebar") }}
        >
          <LightspeedMark
            className="absolute"
            style={{
              left: "50%",
              bottom: 0,
              width: `calc(${scale.value} * 100%)`,
              height: "auto",
              color: literalColor(color.value, resolve) ?? color.value,
              opacity: opacity.value,
              transform: `translateX(-50%) translateY(${offset.value}) rotate(${rotate.value})`,
            }}
          />
        </div>
      </div>
      <SurfaceColorRow label="Color" name={SIDEBAR_LOGO_TOKENS.color} resolve={resolve} {...color} />
      <RangeRow name={SIDEBAR_LOGO_TOKENS.opacity} min={0} max={1} step={0.01} unit="" {...opacity} />
      <RangeRow name={SIDEBAR_LOGO_TOKENS.rotate} min={-180} max={180} step={1} unit="deg" {...rotate} />
      <RangeRow name={SIDEBAR_LOGO_TOKENS.scale} min={0.5} max={3} step={0.05} unit="" describe={(value) => `${round(value * 100)}% wide`} {...scale} />
      <RangeRow name={SIDEBAR_LOGO_TOKENS.offset} min={-50} max={100} step={1} unit="%" {...offset} />
    </section>
  );
}

/// The overlay drawn behind dialogs, alert dialogs and sheets.
function BackdropEditor({ resolve, color, blur, saturate }: {
  resolve: (token: string) => string;
  color: TokenControl;
  blur: TokenControl;
  saturate: TokenControl;
}) {
  return (
    <section className="grid gap-3 rounded-lg border p-3">
      <div className="flex items-start justify-between gap-3">
        <div className="grid min-w-0 gap-0.5">
          <h2 className="text-sm font-medium">Backdrop</h2>
          <p className="text-xs text-muted-foreground">Behind Dialog, Alert dialog, Sheet</p>
        </div>
        <div
          aria-hidden
          className="relative h-12 w-20 shrink-0 overflow-hidden rounded-md border text-[10px] leading-tight font-semibold"
        >
          <div className="absolute inset-0 grid grid-cols-2 p-1">
            <span className="text-primary">Aa</span>
            <span className="text-destructive">Bb</span>
            <span className="text-info">Cc</span>
            <span className="text-success">Dd</span>
          </div>
          <div
            className="absolute inset-0"
            style={{
              background: literalColor(color.value, resolve) ?? color.value,
              backdropFilter: `blur(${blur.value}) saturate(${saturate.value})`,
            }}
          />
        </div>
      </div>
      <SurfaceColorRow label="Color" name={surfaceToken(BACKDROP_KEY, "background")} resolve={resolve} {...color} />
      <EffectRows role={BACKDROP_KEY} blur={blur} saturate={saturate} />
    </section>
  );
}

/// A theme color at an opacity, Transparent, or a custom color. Picking a
/// color on the swatch, or choosing Custom, turns the current color into a
/// literal one that can then be edited freely.
function SurfaceColorRow({ label, name, value, changed, onChange, onReset, resolve }: TokenProps & {
  label: string;
  resolve: (token: string) => string;
}) {
  const [draft, setDraft] = useState(value);
  useEffect(() => setDraft(value), [value]);
  const parsed = parseSurfaceColor(value);
  const rgba = surfaceRgba(value, resolve);
  const valid = CSS.supports("color", draft);
  const percent = parsed ? parsed.percent : rgba ? Math.round(rgba[3] / 2.55) : 100;
  return (
    <div className="grid gap-1.5">
      <div className="flex items-center justify-between gap-2">
        <span className="text-xs font-medium">{label}</span>
        <ResetButton changed={changed} onReset={onReset} name={name} />
      </div>
      <div className="grid grid-cols-[2rem_9rem_minmax(0,1fr)_2.5rem] items-center gap-2">
        <span
          className="relative size-8 overflow-hidden rounded-md border"
          style={{ background: rgba ? rgbaCss(rgba) : value }}
          title="Pick a custom color"
        >
          <input
            type="color"
            aria-label={`Pick ${label.toLowerCase()} color`}
            value={rgba ? toHex(rgba) : "#000000"}
            onChange={(event) => onChange(withAlpha(event.target.value, rgba && rgba[3] > 0 ? rgba[3] : 255))}
            className="absolute inset-0 size-full cursor-pointer opacity-0"
          />
        </span>
        <Select
          value={parsed?.source ?? "custom"}
          onValueChange={(source) => {
            if (!source) return;
            if (source === "custom") {
              if (parsed && rgba) onChange(rgbaCss(rgba));
              return;
            }
            const next = source === "transparent" ? 0 : parsed && parsed.source !== "transparent" ? parsed.percent : parsed ? 100 : percent;
            onChange(surfaceColorValue({ source, percent: next }));
          }}
        >
          <SelectTrigger size="sm" aria-label={`${label} color`}><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="transparent">Transparent</SelectItem>
            {SURFACE_COLOR_SOURCES.map((source) => (
              <SelectItem key={source} value={source}>{source.replace(/^--/, "")}</SelectItem>
            ))}
            <SelectItem value="custom">Custom</SelectItem>
          </SelectContent>
        </Select>
        <input
          type="range"
          aria-label={`${label} opacity`}
          min={0}
          max={100}
          step={1}
          disabled={parsed?.source === "transparent" || (!parsed && !rgba)}
          value={percent}
          className="w-full accent-primary disabled:opacity-40"
          onChange={(event) => {
            const next = Number(event.target.value);
            if (parsed) onChange(surfaceColorValue({ source: parsed.source, percent: next }));
            else if (rgba) onChange(next >= 100 ? toHex(rgba) : `rgb(${rgba[0]} ${rgba[1]} ${rgba[2]} / ${next / 100})`);
          }}
        />
        <span className="text-right font-mono text-xs tabular-nums">{parsed?.source === "transparent" ? "" : `${percent}%`}</span>
      </div>
      <Input
        value={draft}
        aria-label={name}
        aria-invalid={!valid}
        spellCheck={false}
        className="h-8 font-mono text-xs"
        onChange={(event) => {
          setDraft(event.target.value);
          if (CSS.supports("color", event.target.value)) onChange(event.target.value);
        }}
        onBlur={() => setDraft(value)}
      />
    </div>
  );
}

function StepRow({ label, name, steps, value, changed, onChange, onReset }: TokenProps & {
  label: string;
  steps: Array<{ label: string; value: string }>;
}) {
  const selected = steps.find((step) => sameCss(step.value, value))?.value ?? "custom";
  return (
    <div className="grid grid-cols-[minmax(0,10rem)_minmax(0,1fr)_1.5rem] items-center gap-2">
      <TokenName name={name} changed={changed} />
      <Select value={selected} onValueChange={(next) => next && next !== "custom" && onChange(next)}>
        <SelectTrigger size="sm" aria-label={label}><SelectValue /></SelectTrigger>
        <SelectContent>
          {steps.map((step) => (
            <SelectItem key={step.label} value={step.value}>{`${label}: ${step.label}`}</SelectItem>
          ))}
          <SelectItem value="custom" disabled>{`${label}: custom`}</SelectItem>
        </SelectContent>
      </Select>
      <ResetButton changed={changed} onReset={onReset} name={name} />
    </div>
  );
}

function ShadowEditor({ params, values, onChange }: {
  params: ShadowParams;
  values: Record<string, string>;
  onChange: (params: ShadowParams) => void;
}) {
  const changed = params.color.toLowerCase() !== DEFAULT_SHADOW.color || params.opacity !== 1 || params.size !== 1;
  return (
    <>
      <Group title="Shadow scale" note="Shapes all seven levels, shadow-2xs to shadow-2xl, from Tailwind's defaults.">
        <div className="grid grid-cols-[minmax(0,10rem)_minmax(0,1fr)_4.5rem] items-center gap-2">
          <span className="text-xs text-muted-foreground">Color</span>
          <input
            type="color"
            aria-label="Shadow color"
            value={params.color}
            onChange={(event) => onChange({ ...params, color: event.target.value })}
            className="h-8 w-16 cursor-pointer rounded-md border bg-transparent p-0.5"
          />
          <span className="text-right font-mono text-xs">{params.color}</span>
          <span className="text-xs text-muted-foreground">Opacity ×</span>
          <input
            type="range"
            aria-label="Shadow opacity"
            min={0}
            max={4}
            step={0.05}
            value={params.opacity}
            className="w-full accent-primary"
            onChange={(event) => onChange({ ...params, opacity: Number(event.target.value) })}
          />
          <span className="text-right font-mono text-xs tabular-nums">{params.opacity.toFixed(2)}</span>
          <span className="text-xs text-muted-foreground">Size ×</span>
          <input
            type="range"
            aria-label="Shadow size"
            min={0}
            max={3}
            step={0.05}
            value={params.size}
            className="w-full accent-primary"
            onChange={(event) => onChange({ ...params, size: Number(event.target.value) })}
          />
          <span className="text-right font-mono text-xs tabular-nums">{params.size.toFixed(2)}</span>
        </div>
        <div>
          <Button size="sm" variant="ghost" disabled={!changed} onClick={() => onChange(DEFAULT_SHADOW)}>
            <ResetIcon data-icon="inline-start" />
            Reset shadows
          </Button>
        </div>
      </Group>
      <Group title="Preview">
        <div className="grid grid-cols-4 gap-4 rounded-lg bg-muted/40 p-4">
          {SHADOW_TOKENS.map((name) => (
            <div key={name} className="grid justify-items-center gap-2">
              <div className="size-14 rounded-lg bg-card ring-1 ring-foreground/5" style={{ boxShadow: values[name] }} />
              <span className="font-mono text-[10px] text-muted-foreground">{name.replace("--", "")}</span>
            </div>
          ))}
        </div>
      </Group>
    </>
  );
}

function round(value: number): number {
  return Math.round(value * 10_000) / 10_000;
}

let probe: CanvasRenderingContext2D | null | undefined;

/// Any CSS color as sRGB bytes, by painting one pixel. Colors outside sRGB
/// (wide oklch values) are clamped, which is fine for a picker swatch.
function toRgba(color: string): [number, number, number, number] | null {
  if (!CSS.supports("color", color)) return null;
  probe ??= document.createElement("canvas").getContext("2d", { willReadFrequently: true });
  if (!probe) return null;
  probe.clearRect(0, 0, 1, 1);
  probe.fillStyle = "#000";
  probe.fillStyle = color;
  probe.fillRect(0, 0, 1, 1);
  const [r = 0, g = 0, b = 0, a = 255] = probe.getImageData(0, 0, 1, 1).data;
  return [r, g, b, a];
}

function toHex([r, g, b]: [number, number, number, number]): string {
  return `#${[r, g, b].map((channel) => channel.toString(16).padStart(2, "0")).join("")}`;
}

type Rgba = [number, number, number, number];

/// A surface color as sRGB bytes in the edited theme: theme colors are looked
/// up through `resolve` (this window keeps the stock theme of its own), and
/// the opacity becomes the alpha.
function surfaceRgba(value: string, resolve: (token: string) => string): Rgba | null {
  const parsed = parseSurfaceColor(value);
  if (parsed?.source === "transparent") return [0, 0, 0, 0];
  const source = parsed && (parsed.source.startsWith("--") ? `var(${parsed.source})` : parsed.source);
  const base = toRgba(resolveVars(source || value, resolve));
  if (!base || !parsed) return base;
  return [base[0], base[1], base[2], Math.round((base[3] * parsed.percent) / 100)];
}

function literalColor(value: string, resolve: (token: string) => string): string | null {
  const rgba = surfaceRgba(value, resolve);
  return rgba && rgbaCss(rgba);
}

function rgbaCss(rgba: Rgba): string {
  return withAlpha(toHex(rgba), rgba[3]);
}

/// Replaces `var(--x)` with the token's value until none are left.
function resolveVars(text: string, resolve: (token: string) => string): string {
  let current = text;
  for (let depth = 0; depth < 8 && current.includes("var("); depth += 1) {
    current = current.replace(/var\((--[\w-]+)(?:\s*,\s*([^)]*))?\)/g, (match, token: string, fallback?: string) =>
      resolve(token) || fallback || match);
  }
  return current;
}

/// The picker's hex, keeping the token's own transparency.
function withAlpha(hex: string, alpha: number): string {
  if (alpha >= 255) return hex;
  const [r, g, b] = [1, 3, 5].map((index) => parseInt(hex.slice(index, index + 2), 16));
  return `rgb(${r} ${g} ${b} / ${round(alpha / 255)})`;
}
