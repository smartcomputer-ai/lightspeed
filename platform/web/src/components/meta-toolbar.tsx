import { cloneElement, useEffect, useLayoutEffect, useRef, useState, type FocusEvent, type PointerEvent, type ReactNode } from "react";
import { Link } from "react-router-dom";
import { CollapseToolsIcon, ExpandToolsIcon, type IconComponent } from "@/icons";
import { useUserPreferences } from "@/lib/user-preferences";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

/// A meta toolbar: the small facts about a session or a bot (sub-agents,
/// sharing, who manages it, its environment, what waits on it), given once
/// as items and drawn wherever the entity appears. Where decides which items
/// show and how:
///
///   list     a list row: the essentials as icon and value, colored only
///            while the row is hovered or selected, not clickable
///   header   beside the title of its page: more of them, icon and value,
///            clickable
///   strip    under the page header: every item as a tag with its text
///
/// Each item names the places it shows in (`places`), the side it prefers
/// (`align`) and how much it matters (`priority`). Where the toolbar runs
/// out of width it gives way by priority, least important first: in the
/// strip tags first shrink to icon and value, then items move into a "+N"
/// overflow that lists them. A header can offer a toggle (`expandable`)
/// that draws its items as tags instead of icons, and back, and can draw
/// them as tags while the pointer or keyboard focus is on it (`expandOnHover`).
/// While it is so expanded, a lock keeps every hover-expanding toolbar in the
/// app expanded (a user preference) until unlocked.

/// The status color of an item. Untoned items stay muted.
export type MetaTone = "success" | "warning" | "danger" | "info" | "muted";

export interface MetaItem {
  /** Stable within the toolbar. */
  key: string;
  /** The accessible name, e.g. "3 sub-agents"; also the tooltip when there is no other. */
  label: string;
  icon?: IconComponent;
  /** The short value beside the icon, e.g. a count. Compact mode and shrunk tags show it. */
  value?: ReactNode;
  /** The tag's text in full mode, e.g. "3 sub-agents"; falls back to the value, then the label. */
  text?: ReactNode;
  /** Explains the item on hover and focus. Defaults to the text where only the icon shows. */
  tooltip?: ReactNode;
  tone?: MetaTone;
  /** The tone's dot pulses: something is under way. */
  pulse?: boolean;
  /** Stands for an absence ("No environment"): drawn dashed and muted. */
  placeholder?: boolean;
  /** Higher stays longer when room runs out. Default 0. */
  priority?: number;
  /** The side of the toolbar it prefers. Default start. */
  align?: "start" | "end";
  /** Where it shows. Default everywhere. */
  places?: readonly MetaPlace[];
  /** Runs on click. */
  onSelect?: () => void;
  /** Opens this app path on click. */
  href?: string;
  /** Opens in a popover on click, e.g. a resource's details and where to change them. */
  details?: ReactNode;
}

/// Where a toolbar is drawn.
export type MetaPlace = "list" | "header" | "strip";

/// How a place draws its items: compact marks (icon and value), or full tags.
export type MetaToolbarMode = "compact" | "full";
const MODE: Record<MetaPlace, MetaToolbarMode> = { list: "compact", header: "compact", strip: "full" };

/// When items show their status colors: always, or only while the row
/// around them (a ListRow, `group/row`) is hovered, focused or selected,
/// so a list rests calm and colors what the pointer or selection is on.
export type MetaTint = "always" | "highlight";

/// How an item is drawn: full mode's tag, the tag shrunk to icon and
/// value, or compact mode's mark.
export type MetaItemForm = "tag" | "condensed" | "mark";

/// What fitting decides: each shown item's form, and what overflows.
export interface MetaFit {
  forms: Map<string, MetaItemForm>;
  /** Keys of the items in the overflow, in item order. */
  hidden: string[];
}

/// The widths fitting needs, in pixels.
export interface MetaMeasures {
  /** Each item's width per form it can take. */
  widths: Map<string, Partial<Record<MetaItemForm, number>>>;
  overflow: number;
  gap: number;
}

/// The items a place shows.
export function metaItemsFor(items: MetaItem[], place: MetaPlace): MetaItem[] {
  return items.filter((item) => !item.places || item.places.includes(place));
}

/// Whether a full-mode tag can shrink to icon and value.
const condensable = (item: MetaItem) => item.icon !== undefined;

/// Fits a place's items into `budget` pixels. Least important first (by
/// priority, then the later of equals), the strip shrinks tags to icon and
/// value; if that is not enough, items overflow, but the most important one
/// always stays. A budget of zero or less (not laid out, or hidden) shows
/// everything as is.
export function fitMetaItems(
  items: MetaItem[],
  place: MetaPlace,
  budget: number,
  measures: MetaMeasures,
  /** How the place draws them, when not its own way (an expanded header draws tags). */
  mode: MetaToolbarMode = MODE[place],
): MetaFit {
  const shown = metaItemsFor(items, place);
  const forms = new Map<string, MetaItemForm>(shown.map((item) => [item.key, mode === "full" ? "tag" : "mark"]));
  const hidden = new Set<string>();
  if (budget <= 0) return { forms, hidden: [] };

  const width = (key: string) => measures.widths.get(key)?.[forms.get(key)!] ?? 0;
  const total = () => {
    const visible = shown.filter((item) => forms.has(item.key));
    const sides = new Set(visible.map((item) => item.align ?? "start")).size;
    const parts = visible.length + (hidden.size > 0 ? 1 : 0);
    const content = visible.reduce((sum, item) => sum + width(item.key), 0) + (hidden.size > 0 ? measures.overflow : 0);
    // One gap between neighbours, and at least one between the two sides.
    return content + Math.max(0, parts - 1) * measures.gap + (sides > 1 ? measures.gap : 0);
  };
  // Half a pixel of slack absorbs rounding in the measured widths.
  const fits = () => total() <= budget + 0.5;
  const leastFirst = shown
    .map((item, index) => ({ item, index }))
    .sort((a, b) => (a.item.priority ?? 0) - (b.item.priority ?? 0) || b.index - a.index)
    .map(({ item }) => item);

  if (mode === "full") {
    for (const item of leastFirst) {
      if (fits()) break;
      if (condensable(item)) forms.set(item.key, "condensed");
    }
  }
  for (const item of leastFirst.slice(0, -1)) {
    if (fits()) break;
    forms.delete(item.key);
    hidden.add(item.key);
  }
  // Giving way in strict order can leave room: give it back, most important
  // first, to items that fit once more (back from the overflow, shrunk if
  // need be, or whole again).
  const widest: MetaItemForm = mode === "full" ? "tag" : "mark";
  for (const item of [...leastFirst].reverse()) {
    const was = forms.get(item.key);
    if (was === widest) continue;
    if (was === undefined) hidden.delete(item.key);
    const tries: MetaItemForm[] = was === undefined && mode === "full" && condensable(item) ? ["tag", "condensed"] : [widest];
    const form = tries.find((candidate) => {
      forms.set(item.key, candidate);
      return fits();
    });
    if (form) continue;
    if (was === undefined) {
      forms.delete(item.key);
      hidden.add(item.key);
    } else forms.set(item.key, was);
  }
  const ordered = new Map(shown.filter((item) => forms.has(item.key)).map((item) => [item.key, forms.get(item.key)!]));
  return { forms: ordered, hidden: shown.filter((item) => hidden.has(item.key)).map((item) => item.key) };
}

const TONE_TEXT: Record<MetaTone, string> = {
  success: "text-success",
  warning: "text-warning",
  danger: "text-danger",
  info: "text-info",
  muted: "text-muted-foreground/60",
};
const TONE_DOT: Record<MetaTone, string> = {
  success: "bg-success",
  warning: "bg-warning",
  danger: "bg-danger",
  info: "bg-info",
  muted: "bg-muted-foreground/50",
};

// The same colors, applied only while the row is hovered, focused or
// selected, easing with the row's own transition.
const ROW_EASE = "transition-colors duration-(--list-transition-duration)";
const TONE_TEXT_ROW: Record<MetaTone, string> = {
  success: "group-hover/row:text-success group-focus-visible/row:text-success group-data-highlighted/row:text-success",
  warning: "group-hover/row:text-warning group-focus-visible/row:text-warning group-data-highlighted/row:text-warning",
  danger: "group-hover/row:text-danger group-focus-visible/row:text-danger group-data-highlighted/row:text-danger",
  info: "group-hover/row:text-info group-focus-visible/row:text-info group-data-highlighted/row:text-info",
  muted: "text-muted-foreground/60",
};
const TONE_DOT_ROW: Record<MetaTone, string> = {
  success: "bg-muted-foreground/50 group-hover/row:bg-success group-focus-visible/row:bg-success group-data-highlighted/row:bg-success",
  warning: "bg-muted-foreground/50 group-hover/row:bg-warning group-focus-visible/row:bg-warning group-data-highlighted/row:bg-warning",
  danger: "bg-muted-foreground/50 group-hover/row:bg-danger group-focus-visible/row:bg-danger group-data-highlighted/row:bg-danger",
  info: "bg-muted-foreground/50 group-hover/row:bg-info group-focus-visible/row:bg-info group-data-highlighted/row:bg-info",
  muted: "bg-muted-foreground/50",
};
const toneText = (tone: MetaTone, tint: MetaTint) => (tint === "always" ? TONE_TEXT[tone] : cn(ROW_EASE, TONE_TEXT_ROW[tone]));
const toneDot = (tone: MetaTone, tint: MetaTint) => (tint === "always" ? TONE_DOT[tone] : cn(ROW_EASE, TONE_DOT_ROW[tone]));

const GAP: Record<MetaToolbarMode, number> = { compact: 6, full: 4 };
const GAP_CLASS: Record<MetaToolbarMode, string> = { compact: "gap-1.5", full: "gap-1" };

const TAG_CLASS =
  "inline-flex h-6 max-w-56 min-w-0 shrink-0 items-center gap-1 rounded-full border bg-background/60 px-2 text-xs font-medium text-foreground transition-colors";
// Marks brighten under the pointer, clickable or not (in a list row they only
// explain themselves), as the session page's do.
const MARK_CLASS = "inline-flex shrink-0 items-center gap-0.5 rounded-sm text-xs tabular-nums text-muted-foreground transition-colors hover:text-foreground";
/// The expand toggle: a square icon button, kept out of the fitting.
const TOGGLE_WIDTH = 24;

const ACTS_CLASS = "cursor-pointer outline-none focus-visible:ring-2 focus-visible:ring-ring/50";

const present = (node: ReactNode) => node !== undefined && node !== null && node !== false;

/// One item's look in a form, without its tooltip or behavior.
function ItemFace({ item, form, tint = "always" }: { item: MetaItem; form: MetaItemForm; tint?: MetaTint }) {
  const Icon = item.icon;
  const tone = item.tone;
  const dot = (size: string) =>
    tone && <span className={cn(size, "shrink-0 rounded-full", toneDot(tone, tint), item.pulse && "animate-pulse motion-reduce:animate-none")} aria-hidden />;
  if (!Icon) {
    // A status in words: its tone is a dot before them ("● Working"), and a
    // danger also colors the words.
    const text = form === "mark" ? item.value ?? item.text ?? item.label : item.text ?? item.value ?? item.label;
    return (
      <span className="flex min-w-0 items-center gap-1.5">
        {dot(form === "mark" ? "size-2" : "size-1.5")}
        <span className={cn("truncate", tone === "danger" && toneText("danger", tint))}>{text}</span>
      </span>
    );
  }
  if (form === "mark") {
    return (
      <>
        <Icon className={cn("size-3.5 shrink-0", tone && toneText(tone, tint))} aria-hidden />
        {present(item.value) && <span className={cn(tone && toneText(tone, tint))}>{item.value}</span>}
      </>
    );
  }
  const text = form === "condensed" ? item.value : item.text ?? item.value ?? item.label;
  return (
    <>
      <Icon className={cn("size-3 shrink-0", tone ? toneText(tone, tint) : "text-muted-foreground")} aria-hidden />
      {present(text) && <span className={cn("truncate", form === "condensed" && "tabular-nums")}>{text}</span>}
      {dot("size-1.5")}
    </>
  );
}

const faceClass = (item: MetaItem, form: MetaItemForm) =>
  form === "mark" ? MARK_CLASS : cn(TAG_CLASS, item.placeholder && "border-dashed text-muted-foreground");

/// What the item's tooltip says, if anything: its own tooltip, or its text
/// where the form hides the text.
function tipOf(item: MetaItem, form: MetaItemForm): ReactNode {
  if (item.tooltip !== undefined) return item.tooltip;
  if (form === "tag") return null;
  return item.text ?? item.label;
}

/// One item with its tooltip and behavior. Without `interactive` (inside a
/// row link) every item is a plain mark that only explains itself.
function MetaItemView({ item, form, interactive, tint = "always", tooltips = true, onOpenChange }: {
  item: MetaItem;
  form: MetaItemForm;
  interactive: boolean;
  tint?: MetaTint;
  /** Off where hovering already expands the item into its words. */
  tooltips?: boolean;
  /** Whether its details are open. */
  onOpenChange?: (open: boolean) => void;
}) {
  const tip = tooltips ? tipOf(item, form) : null;
  const acts = interactive && (item.onSelect !== undefined || item.href !== undefined || item.details !== undefined);
  const opens = acts && item.details !== undefined;
  const className = cn(faceClass(item, form), acts && ACTS_CLASS, acts && form !== "mark" && "hover:bg-muted");
  const face = <ItemFace item={item} form={form} tint={tint} />;
  // The element the item is: a link, a button, or a plain mark; a popover's
  // trigger when it opens details.
  const shell = !acts ? (
    <span className={className} aria-label={item.label} data-meta-item={item.key} />
  ) : item.href !== undefined ? (
    <Link to={item.href} className={className} aria-label={item.label} onClick={item.onSelect} data-meta-item={item.key} />
  ) : (
    <button type="button" className={className} aria-label={item.label} onClick={item.onSelect} data-meta-item={item.key} />
  );
  const trigger = opens ? <PopoverTrigger render={shell} /> : shell;
  const view = tip ? (
    <Tooltip>
      <TooltipTrigger render={trigger}>{face}</TooltipTrigger>
      <TooltipContent>{tip}</TooltipContent>
    </Tooltip>
  ) : (
    cloneElement(trigger, undefined, face)
  );
  if (!opens) return view;
  return (
    <Popover onOpenChange={onOpenChange}>
      {view}
      <PopoverContent align="start" className="w-80 p-1.5">{item.details}</PopoverContent>
    </Popover>
  );
}

/// The "+N" that holds the items without room. Interactive, it opens a
/// popover with their tags; otherwise its tooltip lists them.
function Overflow({ items, mode, interactive, onOpenChange }: {
  items: MetaItem[];
  mode: MetaToolbarMode;
  interactive: boolean;
  onOpenChange?: (open: boolean) => void;
}) {
  const label = `${items.length} more`;
  const className = mode === "full" ? TAG_CLASS : MARK_CLASS;
  if (!interactive) {
    return (
      <Tooltip>
        <TooltipTrigger render={<span className={className} aria-label={label} data-meta-item={OVERFLOW_KEY} />}>+{items.length}</TooltipTrigger>
        <TooltipContent>
          <span className="grid gap-0.5">
            {items.map((item) => {
              const Icon = item.icon;
              return (
                <span key={item.key} className="flex items-center gap-1.5">
                  {Icon && <Icon className="size-3 shrink-0" aria-hidden />}
                  {item.text ?? item.label}
                </span>
              );
            })}
          </span>
        </TooltipContent>
      </Tooltip>
    );
  }
  return (
    <Popover onOpenChange={onOpenChange}>
      <PopoverTrigger
        render={<button type="button" className={cn(className, ACTS_CLASS, mode === "full" && "hover:bg-muted")} aria-label={`Show ${label}`} data-meta-item={OVERFLOW_KEY} />}
      >
        +{items.length}
      </PopoverTrigger>
      <PopoverContent align="end" className="flex w-auto max-w-80 flex-wrap gap-1 p-1.5">
        {items.map((item) => <MetaItemView key={item.key} item={item} form="tag" interactive />)}
      </PopoverContent>
    </Popover>
  );
}

/// What the measuring layer found: each item's forms, and the overflow in
/// each mode.
interface Measured {
  widths: MetaMeasures["widths"];
  overflow: Partial<Record<MetaToolbarMode, number>>;
}

const sameMeasures = (a: Measured, b: Measured) =>
  a.overflow.compact === b.overflow.compact &&
  a.overflow.full === b.overflow.full &&
  a.widths.size === b.widths.size &&
  [...b.widths].every(([key, forms]) => {
    const before = a.widths.get(key);
    return before !== undefined && (Object.keys(forms) as MetaItemForm[]).every((form) => before[form] === forms[form]);
  });

/// The overflow and the toggle, among the items a switch animates.
const OVERFLOW_KEY = "\u0000overflow";
const TOGGLE_KEY = "\u0000toggle";
const LOCK_KEY = "\u0000lock";
/// The easing of a switch: quick out of the old box, settling into the new.
const SWITCH_EASING = "cubic-bezier(0.2, 0, 0, 1)";

/// Glides a toolbar's items from where they were drawn to where they are
/// now (FLIP). Each starts at its old offset and eases into its new place;
/// one that grows is revealed from its old width by a clip, one that shrinks
/// starts a little faded; one that was not there fades in. Only transform,
/// clip and opacity animate, so no item's animation moves another. Runs over
/// the layout transition time; not at all for viewers who prefer reduced
/// motion, or where elements cannot animate.
function animateSwitch(node: HTMLElement, before: Map<string, DOMRect>, after: Map<string, DOMRect>) {
  if (window.matchMedia?.("(prefers-reduced-motion: reduce)").matches) return;
  const duration = Number.parseFloat(getComputedStyle(node).getPropertyValue("--layout-transition-duration")) || 200;
  const timing = { duration, easing: SWITCH_EASING };
  for (const element of drawnItems(node)) {
    if (typeof element.animate !== "function") return;
    const key = element.dataset.metaItem!;
    const from = before.get(key);
    const to = after.get(key)!;
    if (!from) {
      element.animate([{ opacity: 0 }, { opacity: 1 }], timing);
      continue;
    }
    const dx = from.left - to.left;
    const grows = to.width - from.width;
    if (Math.abs(dx) < 0.5 && Math.abs(grows) < 0.5) continue;
    element.animate(
      grows > 0
        ? [
          { transform: `translateX(${dx}px)`, clipPath: `inset(0 ${grows}px 0 0 round 12px)`, opacity: 0.6 },
          { transform: "none", clipPath: "inset(0 0 0 0 round 12px)", opacity: 1 },
        ]
        : [
          { transform: `translateX(${dx}px)`, opacity: 0.6 },
          { transform: "none", opacity: 1 },
        ],
      timing,
    );
  }
}

/// The drawn items of a toolbar (not its measuring layer), with the "+N",
/// the toggle and the lock.
function drawnItems(node: HTMLElement): HTMLElement[] {
  return [...node.querySelectorAll<HTMLElement>("[data-meta-item]")];
}

/// Where each drawn item of a toolbar is, as it shows now.
function itemRects(node: HTMLElement): Map<string, DOMRect> {
  return new Map(drawnItems(node).map((element) => [element.dataset.metaItem!, element.getBoundingClientRect()]));
}

/// Stops a toolbar's running glides, so its items stand where they belong.
function stopGlides(node: HTMLElement) {
  for (const element of drawnItems(node)) for (const animation of element.getAnimations?.() ?? []) animation.cancel();
}

/// How long the pointer rests before a hover-expanding toolbar opens, and
/// how long it stays open after the pointer leaves, so passing over it does
/// not flash and leaving briefly does not close it.
const PEEK_OPEN_MS = 120;
const PEEK_CLOSE_MS = 250;

/// The toolbar. It fits its items into its own width, which it measures. As
/// a flex child it asks for its natural width (every item at its widest)
/// and may shrink down to its most important item; elsewhere it takes the
/// width it is given. `wrap` instead wraps onto more lines and never gives
/// way.
export function MetaToolbar({
  items,
  place,
  label,
  interactive = place !== "list",
  wrap = false,
  tint = place === "list" ? "highlight" : "always",
  expandable = false,
  expandOnHover = false,
  expanded: expandedProp,
  defaultExpanded = false,
  onExpandedChange,
  className,
}: {
  items: MetaItem[];
  place: MetaPlace;
  /** The accessible name of the group, e.g. "Session details". */
  label?: string;
  /** Items act on click. Off in a list (inside the row link), where marks only explain themselves. */
  interactive?: boolean;
  /** Wrap onto more lines instead of giving way by priority. */
  wrap?: boolean;
  /** When status colors show: always, or (the list's default) only while the row around is hovered, focused or selected. */
  tint?: MetaTint;
  /** Offers a toggle that draws the items as tags (full) instead of icons, and back. */
  expandable?: boolean;
  /**
   * Draws the items as tags while the pointer rests on it or keyboard focus
   * is in it, and while one of its popovers is open. It keeps the room it has
   * collapsed, so expanding moves nothing around it: tags give way inside it.
   * Its icons have no tooltips: hovering already shows their words. While
   * expanded it offers a lock that keeps every such toolbar expanded.
   */
  expandOnHover?: boolean;
  /** Whether it draws tags, when the caller keeps that state. */
  expanded?: boolean;
  defaultExpanded?: boolean;
  onExpandedChange?: (expanded: boolean) => void;
  className?: string;
}) {
  const [ownExpanded, setOwnExpanded] = useState(defaultExpanded);
  // Expanded by its toggle, or for now by hover, focus or an open popover.
  const toggled = expandable && (expandedProp ?? ownExpanded);
  const [peeking, setPeeking] = useState(false);
  const [openPopovers, setOpenPopovers] = useState(0);
  const peekTimer = useRef<number | undefined>(undefined);
  // Where the items were drawn and in which mode, for the next switch to
  // glide from. Taken as a switch begins, where the items visibly are (a
  // glide may be under way), and after each switch where they settle.
  const outer = useRef<HTMLDivElement>(null);
  const drawnMode = useRef<MetaToolbarMode | null>(null);
  const drawn = useRef<{ mode: MetaToolbarMode; rects: Map<string, DOMRect> } | null>(null);
  const snapshot = () => {
    if (outer.current && drawnMode.current) drawn.current = { mode: drawnMode.current, rects: itemRects(outer.current) };
  };
  useEffect(() => () => window.clearTimeout(peekTimer.current), []);
  // Set when expansion is ended by hand (unlocking, the toggle) while the
  // pointer is on it: hover and focus do not reopen it until the pointer
  // leaves and comes back.
  const held = useRef(false);
  const peek = (next: boolean, delay: number) => {
    if (next && held.current) return;
    window.clearTimeout(peekTimer.current);
    peekTimer.current = window.setTimeout(() => {
      snapshot();
      setPeeking(next);
    }, delay);
  };
  // The lock, one setting for the whole app: hover-expanding toolbars start expanded.
  const { keepMetaExpanded, setKeepMetaExpanded } = useUserPreferences();
  const locked = expandOnHover && keepMetaExpanded;
  const expanded = toggled || locked || (expandOnHover && (peeking || openPopovers > 0));
  const showLock = expandOnHover && expanded && !toggled;
  /// Collapses at once, and keeps hover from reopening it until the pointer
  /// leaves and comes back.
  const collapseNow = () => {
    window.clearTimeout(peekTimer.current);
    setPeeking(false);
    held.current = true;
  };
  const setExpanded = (next: boolean) => {
    snapshot();
    setOwnExpanded(next);
    onExpandedChange?.(next);
    // Collapsing by hand wins over the hover until the pointer comes back.
    if (!next) collapseNow();
  };
  const popoverOpen = expandOnHover
    ? (open: boolean) => {
      snapshot();
      setOpenPopovers((count) => Math.max(0, count + (open ? 1 : -1)));
    }
    : undefined;
  const mode = expanded ? "full" : MODE[place];
  // The room it asks for: a hover keeps the collapsed room.
  const roomMode = toggled || locked ? "full" : MODE[place];
  const shown = metaItemsFor(items, place);
  const gap = GAP[mode];
  const layer = useRef<HTMLDivElement>(null);
  const [budget, setBudget] = useState(0);
  const [measured, setMeasured] = useState<Measured>({ widths: new Map(), overflow: {} });
  const measures: MetaMeasures = { widths: measured.widths, overflow: measured.overflow[mode] ?? 0, gap };

  // The available width follows the toolbar's own box.
  useLayoutEffect(() => {
    const node = outer.current;
    if (!node || wrap || typeof ResizeObserver === "undefined") return;
    setBudget(node.clientWidth);
    const observer = new ResizeObserver(() => setBudget(node.clientWidth));
    observer.observe(node);
    return () => observer.disconnect();
  }, [wrap, shown.length > 0]);

  // Every form of every item, measured in an invisible layer after each
  // render; state changes only when a width does.
  useLayoutEffect(() => {
    const node = layer.current;
    if (!node) return;
    const widths = new Map<string, Partial<Record<MetaItemForm, number>>>();
    const overflow: Measured["overflow"] = {};
    for (const child of node.children) {
      const element = child as HTMLElement;
      const width = element.getBoundingClientRect().width;
      if (element.dataset.overflow !== undefined) overflow[element.dataset.overflow as MetaToolbarMode] = width;
      else {
        const key = element.dataset.key!;
        widths.set(key, { ...widths.get(key), [element.dataset.form as MetaItemForm]: width });
      }
    }
    const next = { widths, overflow };
    setMeasured((current) => (sameMeasures(current, next) ? current : next));
  });

  // On a switch, glide from where the items were to where they settle.
  useLayoutEffect(() => {
    const node = outer.current;
    drawnMode.current = mode;
    if (!node) return;
    if (drawn.current && drawn.current.mode !== mode) {
      const before = drawn.current.rects;
      stopGlides(node);
      const after = itemRects(node);
      animateSwitch(node, before, after);
      drawn.current = { mode, rects: after };
    } else if (!drawnItems(node).some((element) => (element.getAnimations?.() ?? []).length > 0)) {
      // Settled (no glide under way): where the items are now.
      drawn.current = { mode, rects: itemRects(node) };
    }
  });

  if (shown.length === 0) return null;
  // The toggle and the lock keep their room; the items fit into the rest.
  const buttons = (expandable ? 1 : 0) + (showLock ? 1 : 0);
  const reserved = buttons * (TOGGLE_WIDTH + gap);
  const roomGap = GAP[roomMode];
  const roomReserved = ((expandable ? 1 : 0) + (locked ? 1 : 0)) * (TOGGLE_WIDTH + roomGap);
  const { forms, hidden } = fitMetaItems(items, place, wrap || budget <= 0 ? 0 : Math.max(1, budget - reserved), measures, mode);
  const visible = shown.filter((item) => forms.has(item.key));
  const start = visible.filter((item) => (item.align ?? "start") === "start");
  const end = visible.filter((item) => item.align === "end");
  const overflowing = shown.filter((item) => hidden.includes(item.key));

  // What the toolbar asks for (every item at its widest) and the least it
  // keeps (its most important item at its smallest, and the overflow).
  const widest = roomMode === "full" ? "tag" : "mark";
  const sides = new Set(shown.map((item) => item.align ?? "start")).size;
  const natural = Math.ceil(
    shown.reduce((sum, item) => sum + (measured.widths.get(item.key)?.[widest] ?? 0), 0) +
      (shown.length - 1) * roomGap +
      (sides > 1 ? roomGap : 0) +
      roomReserved,
  );
  const top = [...shown].sort((a, b) => (b.priority ?? 0) - (a.priority ?? 0))[0]!;
  const topForms = measured.widths.get(top.key) ?? {};
  const topWidths = (roomMode === "full" ? [topForms.tag, topForms.condensed] : [topForms.mark]).filter((width): width is number => width !== undefined);
  const keep = topWidths.length
    ? Math.ceil(Math.min(...topWidths) + (shown.length > 1 ? roomGap + (measured.overflow[roomMode] ?? 0) : 0) + roomReserved)
    : 0;
  // A toolbar that can switch measures both modes' forms, so switching is instant.
  const switches = expandable || expandOnHover;
  const layerForms: readonly MetaItemForm[] = switches ? ["mark", "tag", "condensed"] : mode === "full" ? ["tag", "condensed"] : ["mark"];
  const layerOverflows: readonly MetaToolbarMode[] = switches ? ["compact", "full"] : [mode];
  const view = (item: MetaItem) => {
    const form = forms.get(item.key)!;
    return (
      <MetaItemView
        key={item.key}
        item={item}
        form={form}
        interactive={interactive}
        tint={tint}
        tooltips={!(expandOnHover && form === "mark")}
        onOpenChange={popoverOpen}
      />
    );
  };

  return (
    <div
      ref={outer}
      role="group"
      aria-label={label}
      data-slot="meta-toolbar"
      data-place={place}
      data-mode={mode}
      data-expanded={expanded || undefined}
      style={wrap ? undefined : { flexBasis: natural || undefined, minWidth: keep || undefined }}
      className={cn("relative flex min-w-0 shrink items-center", GAP_CLASS[mode], wrap && "flex-wrap", className)}
      {...(expandOnHover ? {
        onPointerEnter: (event: PointerEvent) => { if (event.pointerType !== "touch") peek(true, PEEK_OPEN_MS); },
        onPointerLeave: (event: PointerEvent) => {
          held.current = false;
          if (event.pointerType !== "touch") peek(false, PEEK_CLOSE_MS);
        },
        // Keyboard focus only: a click focuses too, and should not hold it open.
        onFocus: (event: FocusEvent) => { if ((event.target as HTMLElement).matches(":focus-visible")) peek(true, 0); },
        onBlur: (event: FocusEvent) => {
          if (!event.currentTarget.contains(event.relatedTarget as Node | null)) peek(false, PEEK_CLOSE_MS);
        },
      } : {})}
    >
      {!wrap && (
        // Clipped to the toolbar, so the wide invisible layer never makes a
        // scroll container scroll.
        <div aria-hidden className="pointer-events-none invisible absolute inset-0 overflow-hidden">
          <div ref={layer} className="flex w-max flex-col items-start">
            {shown.flatMap((item) =>
              layerForms
                .filter((form) => form !== "condensed" || condensable(item))
                .map((form) => (
                  <span key={`${item.key}:${form}`} data-key={item.key} data-form={form} className={faceClass(item, form)}>
                    <ItemFace item={item} form={form} />
                  </span>
                )),
            )}
            {layerOverflows.map((overflowMode) => (
              <span key={overflowMode} data-overflow={overflowMode} className={overflowMode === "full" ? TAG_CLASS : MARK_CLASS}>
                +{Math.max(shown.length, 9)}
              </span>
            ))}
          </div>
        </div>
      )}
      {start.map(view)}
      {overflowing.length > 0 && <Overflow items={overflowing} mode={mode} interactive={interactive} onOpenChange={popoverOpen} />}
      {end.length > 0 && (
        <span className={cn("ml-auto flex shrink-0 items-center", GAP_CLASS[mode])}>
          {end.map(view)}
        </span>
      )}
      {showLock && (
        <Tooltip>
          <TooltipTrigger
            render={
              <button
                type="button"
                aria-label={locked ? "Let details collapse to icons" : "Keep details expanded everywhere"}
                aria-pressed={locked}
                data-meta-item={LOCK_KEY}
                onClick={() => {
                  snapshot();
                  setKeepMetaExpanded(!keepMetaExpanded);
                  // Unlocking collapses it now, under the pointer too.
                  if (keepMetaExpanded) collapseNow();
                }}
                className={cn(
                  "inline-flex size-6 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground",
                  ACTS_CLASS,
                )}
              />
            }
          >
            {/* Its state is its icon: arrows out to keep expanded, arrows in to let collapse. */}
            {locked ? <CollapseToolsIcon className="size-3.5" aria-hidden /> : <ExpandToolsIcon className="size-3.5" aria-hidden />}
          </TooltipTrigger>
          <TooltipContent>{locked ? "Let details collapse to icons" : "Keep details expanded everywhere"}</TooltipContent>
        </Tooltip>
      )}
      {expandable && (
        <Tooltip>
          <TooltipTrigger
            render={
              <button
                type="button"
                aria-label={toggled ? "Show details as icons" : "Show details as tags"}
                aria-pressed={toggled}
                data-meta-item={TOGGLE_KEY}
                onClick={() => setExpanded(!toggled)}
                className={cn(
                  "inline-flex size-6 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground",
                  ACTS_CLASS,
                )}
              />
            }
          >
            {toggled ? <CollapseToolsIcon className="size-3.5" aria-hidden /> : <ExpandToolsIcon className="size-3.5" aria-hidden />}
          </TooltipTrigger>
          <TooltipContent>{toggled ? "Show details as icons" : "Show details as tags"}</TooltipContent>
        </Tooltip>
      )}
    </div>
  );
}
