import { cloneElement, type ReactNode } from "react";
import { Link } from "react-router-dom";
import type { IconComponent } from "@/icons";
import { StatusDot, type StatusTone } from "@/components/status-dot";
import { PopoverClose } from "@/components/ui/popover";
import { cn } from "@/lib/utils";

/// The content of a meta item's popover (`MetaItem.details`): what the item
/// is, the things behind it, and where to change them. The popover around
/// it (surface, notch, width, padding) comes from the meta toolbar; these
/// parts give every item's content the same look:
///
///   MetaDetails          the content, top to bottom
///   MetaDetailsHeading   a title and a muted line saying what the item is
///   MetaDetailsList      the rows, scrolling past a maximum height
///   MetaDetailsRow       one thing: a status dot or an icon, its name, its
///                        facts and badges; a link or an action when it opens
///                        something, which closes the popover
///   MetaDetailsEmpty     the line shown when there is nothing to list
///   MetaDetailsNote      a separated muted line under the rows
///   MetaDetailsFooter    the actions under a separator (MetaDetailsAction)
///
/// Rows and actions that close the popover must sit inside one.

/// A resource's tone as a status dot's: running is a steady green, busy
/// waits in amber, a problem asks for attention.
export type ResourceTone = "ok" | "idle" | "busy" | "problem";
export const RESOURCE_STATUS_TONE: Record<ResourceTone, StatusTone> = {
  ok: "ready",
  idle: "idle",
  busy: "waiting",
  problem: "attention",
};

export function MetaDetails({ children, className }: { children: ReactNode; className?: string }) {
  return <div data-slot="meta-details" className={cn("grid grid-cols-[minmax(0,1fr)]", className)}>{children}</div>;
}

export function MetaDetailsHeading({ title, description }: { title: ReactNode; description?: ReactNode }) {
  return (
    <div data-slot="meta-details-heading" className="px-2 pt-1 pb-1.5">
      <p className="text-sm font-medium">{title}</p>
      {description && <p className="text-xs text-muted-foreground">{description}</p>}
    </div>
  );
}

/// The rows. Past its height the list scrolls on its own, so the heading
/// and footer stay put and the popover never needs a second scroll bar.
/// Its one column never grows past the popover, so long names truncate.
export function MetaDetailsList({ label, children, className }: {
  /** The accessible name, e.g. "Sub-agents". */
  label?: string;
  children: ReactNode;
  className?: string;
}) {
  return (
    <ul data-slot="meta-details-list" aria-label={label} className={cn("grid max-h-72 grid-cols-[minmax(0,1fr)] gap-px overflow-y-auto", className)}>
      {children}
    </ul>
  );
}

const ROW_CLASS = "flex w-full items-start gap-2 rounded-md px-2 py-1.5 text-left";
/// A row that opens something highlights like a menu item.
const ROW_ACTS_CLASS = "cursor-action outline-none hover:bg-foreground/10 focus-visible:bg-foreground/10";

/// One thing behind the item, in a MetaDetailsList. Its lead is a status
/// dot (`tone`) or an icon; with neither it keeps the dot's room, so the
/// rows of a list line up (`inset={false}` gives the room up). With `href`
/// it is a link and with `onSelect` an action; either closes the popover.
export function MetaDetailsRow({
  primary,
  secondary,
  tone,
  icon: Icon,
  inset = true,
  badges = [],
  muted = false,
  mono = false,
  current = false,
  href,
  external = false,
  onSelect,
  end,
}: {
  /** Its name. */
  primary: ReactNode;
  /** Facts under the name, joined by " · "; empty parts are left out. */
  secondary?: ReactNode | ReactNode[];
  tone?: StatusTone;
  /** Leads instead of a status dot, e.g. for an action row. */
  icon?: IconComponent;
  inset?: boolean;
  /** Short words beside the name, e.g. "Active", "Default". */
  badges?: Array<string | false | null | undefined>;
  /** Drawn muted: something closed or gone. */
  muted?: boolean;
  /** The name is code (a path, a hash): monospace, broken anywhere instead of cut. */
  mono?: boolean;
  /** What the page shows now, e.g. the open conversation: highlighted, and announced as the current page. */
  current?: boolean;
  /** Opens this app path; with `external`, this address in a new tab. */
  href?: string;
  external?: boolean;
  onSelect?: () => void;
  /** Trailing controls, e.g. a copy button. Only on a row that is not itself a link or action. */
  end?: ReactNode;
}) {
  const facts = (Array.isArray(secondary) ? secondary : [secondary])
    .filter((part) => part !== undefined && part !== null && part !== false && part !== "");
  const shownBadges = badges.filter((badge): badge is string => Boolean(badge));
  // An icon's width, so names line up with each other and with the
  // footer's actions; a dot sits in its middle.
  const lead = Icon
    ? <Icon className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />
    : (tone || inset) && (
      <span className="mt-0.5 flex size-4 shrink-0 items-center justify-center" aria-hidden>
        {tone && <StatusDot tone={tone} />}
      </span>
    );
  const body = (
    <>
      {lead}
      <span className="min-w-0 flex-1">
        <span className="flex min-w-0 items-center gap-1.5">
          <span className={cn(
            "min-w-0",
            mono ? "font-mono text-xs leading-5 break-all" : "truncate text-sm",
            muted && "text-muted-foreground",
          )}>
            {primary}
          </span>
          {shownBadges.map((badge) => (
            <span key={badge} className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">{badge}</span>
          ))}
        </span>
        {facts.length > 0 && (
          <span className="block text-xs text-muted-foreground">
            {facts.map((part, index) => <span key={index}>{index > 0 && " · "}{part}</span>)}
          </span>
        )}
      </span>
    </>
  );
  const acts = cn(ROW_CLASS, ROW_ACTS_CLASS, current && "bg-foreground/5");
  return (
    <li data-slot="meta-details-row" data-current={current || undefined}>
      {href !== undefined && external ? (
        <PopoverClose nativeButton={false} render={<a href={href} target="_blank" rel="noopener noreferrer" />} className={acts}>{body}</PopoverClose>
      ) : href !== undefined ? (
        <PopoverClose nativeButton={false} render={<Link to={href} aria-current={current ? "page" : undefined} />} className={acts}>{body}</PopoverClose>
      ) : onSelect ? (
        <PopoverClose onClick={onSelect} className={acts}>{body}</PopoverClose>
      ) : (
        <div className={ROW_CLASS}>
          {body}
          {end && <span className="flex shrink-0 items-center gap-1">{end}</span>}
        </div>
      )}
    </li>
  );
}

export function MetaDetailsEmpty({ children }: { children: ReactNode }) {
  return <p data-slot="meta-details-empty" className="px-2 py-1.5 text-xs text-muted-foreground">{children}</p>;
}

export function MetaDetailsNote({ children }: { children: ReactNode }) {
  return <p data-slot="meta-details-note" className="mt-1 border-t px-2 pt-1.5 text-xs text-muted-foreground">{children}</p>;
}

/// The actions under a separator, one per line. The gap under the separator
/// keeps a highlighted action off the line.
export function MetaDetailsFooter({ children }: { children: ReactNode }) {
  return (
    <div data-slot="meta-details-footer" className="mt-1 grid grid-cols-[minmax(0,1fr)] gap-px border-t pt-1">
      {children}
    </div>
  );
}

/// One footer action, drawn like a row: an icon and a single line of text,
/// highlighted on hover like a menu item. A link (`href`, `external` for a
/// new tab) or a button (`onSelect`). It closes the popover unless
/// `keepOpen` (load more); `busy` disables it and shows progress. An
/// external link's accessible name says it opens a new tab.
export function MetaDetailsAction({
  children,
  icon: Icon,
  href,
  external = false,
  onSelect,
  keepOpen = false,
  busy = false,
  label,
  title,
}: {
  children: ReactNode;
  icon?: IconComponent;
  /** The accessible name, where the text alone says too little. */
  label?: string;
  /** Explains it on hover. */
  title?: string;
  href?: string;
  external?: boolean;
  onSelect?: () => void;
  keepOpen?: boolean;
  busy?: boolean;
}) {
  const className = cn(
    "flex w-full min-w-0 items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm",
    ROW_ACTS_CLASS,
    "disabled:cursor-progress disabled:opacity-60 disabled:hover:bg-transparent",
  );
  const content = (
    <>
      {Icon && <Icon className="size-4 shrink-0 text-muted-foreground" aria-hidden />}
      <span className="min-w-0 truncate">{children}</span>
    </>
  );
  const named = { className, onClick: onSelect, "aria-label": label, title };
  const element = href !== undefined
    ? external
      ? <a href={href} target="_blank" rel="noopener noreferrer" {...named} />
      : <Link to={href} {...named} />
    : <button type="button" {...named} disabled={busy} />;
  if (keepOpen) return cloneElement(element, { "aria-busy": busy || undefined }, content);
  return (
    <PopoverClose
      nativeButton={href === undefined}
      render={element}
      disabled={busy}
      aria-busy={busy || undefined}
    >
      {content}
    </PopoverClose>
  );
}
