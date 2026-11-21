import { useState, type ComponentType, type ReactNode, type SVGProps } from "react";
import type { ToolCallDisplayGroup } from "@lightspeed-ai/sdk";
import { BotEventIcon, CodeToolIcon, DelegateToolIcon, EditToolIcon, ExecuteToolIcon, ExploreToolIcon, McpToolIcon, OtherToolIcon } from "@/icons";
import { cn } from "@/lib/utils";

export type ToolActivityIconComponent = ComponentType<SVGProps<SVGSVGElement> & { className?: string }>;

interface GroupStyle {
  icon: ToolActivityIconComponent;
  text: string;
  tile: string;
}

const GROUP_STYLES: Record<ToolCallDisplayGroup, GroupStyle> = {
  explore: { icon: ExploreToolIcon, text: "text-cyan-700 dark:text-cyan-300", tile: "bg-cyan-500/10" },
  edit: { icon: EditToolIcon, text: "text-amber-700 dark:text-amber-300", tile: "bg-amber-500/10" },
  execute: { icon: ExecuteToolIcon, text: "text-blue-700 dark:text-blue-300", tile: "bg-blue-500/10" },
  code: { icon: CodeToolIcon, text: "text-fuchsia-700 dark:text-fuchsia-300", tile: "bg-fuchsia-500/10" },
  mcp: { icon: McpToolIcon, text: "text-violet-700 dark:text-violet-300", tile: "bg-violet-500/10" },
  agent: { icon: DelegateToolIcon, text: "text-indigo-700 dark:text-indigo-300", tile: "bg-indigo-500/10" },
  bot: { icon: BotEventIcon, text: "text-teal-700 dark:text-teal-300", tile: "bg-teal-500/10" },
  other: { icon: OtherToolIcon, text: "text-foreground/75", tile: "bg-muted" },
};

/// Display order of activity families on a folded run strip.
export const GROUP_ORDER = Object.keys(GROUP_STYLES) as ToolCallDisplayGroup[];

export function groupStyle(group: string | null | undefined): GroupStyle {
  return group && Object.hasOwn(GROUP_STYLES, group)
    ? GROUP_STYLES[group as ToolCallDisplayGroup]
    : GROUP_STYLES.other;
}

export function ToolActivityIcon({ group, className }: { group?: string | null; className?: string }) {
  const Icon = groupStyle(group).icon;
  return <Icon className={className} aria-hidden="true" />;
}

/// One activity row. The row is the trigger; the detail opens beneath it.
export function StepRow({
  icon,
  tone,
  verb,
  target,
  targetTitle,
  detail,
  right,
  highlightFailure = false,
  cancelled = false,
  note,
  extra,
  ariaLabel,
  children,
}: {
  icon: ReactNode;
  tone: string;
  verb: string;
  target?: string | null;
  targetTitle?: string;
  detail?: string | null;
  right?: ReactNode;
  highlightFailure?: boolean;
  cancelled?: boolean;
  /// A line under the row: the first line of an error, or a link.
  note?: ReactNode;
  /// Trailing inline content before the right slot, such as a link.
  extra?: ReactNode;
  ariaLabel?: string;
  children?: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  return (
    <div className={cn("min-w-0", highlightFailure && "rounded-md bg-destructive/5", cancelled && "opacity-60")}>
      <div className="group/step flex min-w-0 items-center gap-2 rounded-md pr-2 transition-colors hover:bg-muted/50">
        <button
          type="button"
          aria-expanded={open}
          aria-label={ariaLabel}
          onClick={() => setOpen((value) => !value)}
          className="flex min-w-0 flex-1 items-center gap-2 rounded-md px-2 py-1 text-left text-[13px] leading-5 outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
        >
          <span className={cn("flex size-3.5 shrink-0 items-center justify-center [&_svg]:size-3.5", tone)}>
            {icon}
          </span>
          <span className={cn("shrink-0 font-medium", tone)}>{verb}</span>
          {target && (
            <span
              className={cn("min-w-0 truncate text-foreground/85", cancelled && "line-through")}
              title={targetTitle ?? target}
            >
              {target}
            </span>
          )}
          {detail && <span className="min-w-0 shrink-[2] truncate text-muted-foreground">{detail}</span>}
        </button>
        {extra}
        {right !== undefined && right !== null && (
          <span
            className={cn(
              "flex shrink-0 items-center gap-1 text-xs tabular-nums text-muted-foreground [&_svg]:size-3",
              highlightFailure && "text-destructive",
            )}
          >
            {right}
          </span>
        )}
      </div>
      {note}
      {open && children}
    </div>
  );
}
