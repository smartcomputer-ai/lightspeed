import { Fragment, type ReactNode } from "react";
import { NavLink, useNavigate } from "react-router-dom";
import { BackIcon, MoreIcon } from "@/icons";
import { PaneBar } from "@/components/list-pane";
import { Button, useInButtonGroup } from "@/components/ui/button";
import { ButtonGroup } from "@/components/ui/button-group";
import { SaveButton } from "@/components/ui/save-button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { WithTooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

/// A tool of a detail header's top right. Tools are data, not elements, so
/// the header can lay them out the same way on every page: a button where
/// there is room, an item of a ⋯ menu where there is not.
export type HeaderTool =
  | {
      /** An icon button. */
      kind?: "button";
      key: string;
      /** The button's accessible name and tooltip, and the menu item's text. */
      label: string;
      icon: ReactNode;
      onSelect?: () => void;
      /** Opens this app path, or, with `external`, this URL in a new tab. */
      href?: string;
      external?: boolean;
      disabled?: boolean;
      destructive?: boolean;
      hint?: string;
      keyShortcuts?: string;
    }
  | {
      /** One of a few views (Form/JSON, Source/Preview): tabs or a toggle group, or radio items. A subtool only. */
      kind: "choice";
      key: string;
      label: string;
      value: string;
      options: Array<{ value: string; label: string; hint?: string; icon?: ReactNode }>;
      onChange: (value: string) => void;
      /** Shown inline as tabs (the default) or as a toggle group. */
      appearance?: "tabs" | "toggle";
    }
  | {
      /** The save button (see ui/save-button): Save, Saving…, Saved or Save failed. */
      kind: "save";
      key: string;
      label: string;
      dirty: boolean;
      saving: boolean;
      /** The last save failed. */
      error?: boolean;
      /** Cannot save even with changes, e.g. the file changed elsewhere. */
      disabled?: boolean;
      onSave: () => void;
      hint?: string;
      keyShortcuts?: string;
    }
  | {
      /** Shown as is, e.g. a component's own ⋯ menu. Among the main tools it
          is one button of their group (a menu's trigger takes the group's look). */
      kind: "custom";
      key: string;
      node: ReactNode;
    };

/// A main tool: anything but a choice, which is a group of its own.
export type MainHeaderTool = Exclude<HeaderTool, { kind: "choice" }>;

/// The header of the detail beside a list. Left: what is shown (title,
/// leading icon, tags); right: details, then the tools. It is a PaneBar, so
/// it has the list header's height.
///
/// The tools are laid out the same way on every page. The main tools (save,
/// download, the page's ⋯ menu) sit at the far right, joined in one button
/// group, and show at every width. Subtools (a Source/Preview toggle, a
/// view's own buttons) sit to their left; where the header is narrow, the
/// details hide and the subtools move into a ⋯ menu of their own, except
/// custom ones.
export function DetailHeader({
  back,
  leading,
  title,
  titleHint,
  titleClassName,
  tags,
  details,
  tools = [],
  subtools = [],
}: {
  /** Where the back arrow leads on small screens, where the list is hidden. */
  back?: { to: string; label: string };
  leading?: ReactNode;
  title: ReactNode;
  titleHint?: string;
  titleClassName?: string;
  /** Beside the title: status, sharing, who manages it. */
  tags?: ReactNode;
  /** Secondary facts on the right (an id, a revision, a size); hidden where narrow. */
  details?: ReactNode;
  /** The main tools, in order, joined at the far right. */
  tools?: MainHeaderTool[];
  /** Tools left of the main ones, in order; folded into a menu where narrow. */
  subtools?: HeaderTool[];
}) {
  const collapsible = subtools.filter((tool): tool is CollapsibleTool => tool.kind !== "custom");
  return (
    <PaneBar as="header" className="@container/detail-header px-2 md:px-4">
      {back && (
        <NavLink to={back.to} className="flex size-8 shrink-0 items-center justify-center md:hidden" aria-label={back.label}>
          <BackIcon className="size-4" />
        </NavLink>
      )}
      {leading && <span className="flex shrink-0 items-center">{leading}</span>}
      <div className="flex min-w-0 flex-1 items-center gap-2">
        <h1 className={cn("min-w-0 truncate text-sm font-semibold", titleClassName)} title={titleHint}>{title}</h1>
        {tags}
      </div>
      {details && (
        <div className="hidden shrink-0 items-center gap-2 text-xs text-muted-foreground @2xl/detail-header:flex">
          {details}
        </div>
      )}
      {subtools.length > 0 && (
        <div data-header-subtools className="flex shrink-0 items-center gap-1">
          {subtools.map((tool) =>
            tool.kind === "custom" ? (
              <Fragment key={tool.key}>{tool.node}</Fragment>
            ) : (
              <span key={tool.key} className="hidden @xl/detail-header:contents">
                <InlineTool tool={tool} />
              </span>
            ),
          )}
          {collapsible.length > 0 && (
            <span className="contents @xl/detail-header:hidden">
              <CollapsedTools tools={collapsible} />
            </span>
          )}
        </div>
      )}
      {tools.length > 0 && (
        <ButtonGroup aria-label="Tools" data-header-tools className="shrink-0">
          {tools.map((tool) =>
            tool.kind === "custom" ? <Fragment key={tool.key}>{tool.node}</Fragment> : <InlineTool key={tool.key} tool={tool} />,
          )}
        </ButtonGroup>
      )}
    </PaneBar>
  );
}

type CollapsibleTool = Exclude<HeaderTool, { kind: "custom" }>;

/// What a save action says now, and whether it can save.
function saveStatus(action: Extract<HeaderTool, { kind: "save" }>) {
  const text = action.saving ? "Saving…" : action.error ? "Save failed" : action.dirty ? action.label : "Saved";
  const ready = !action.saving && (action.dirty || !!action.error) && !action.disabled;
  return { text, ready };
}

/// A tool as a button, or a group of buttons for a choice. Among the main
/// tools, its buttons take their group's secondary look.
function InlineTool({ tool: action }: { tool: CollapsibleTool }) {
  const grouped = useInButtonGroup();
  if (action.kind === "save") {
    return (
      <WithTooltip content={action.hint}>
        <SaveButton
          size="sm"
          dirty={action.dirty}
          saving={action.saving}
          error={action.error}
          disabled={action.disabled && action.dirty && !action.saving}
          onClick={action.onSave}
          aria-keyshortcuts={action.keyShortcuts}
        >
          {action.label}
        </SaveButton>
      </WithTooltip>
    );
  }
  if (action.kind === "choice" && action.appearance === "toggle") {
    return (
      <ButtonGroup aria-label={action.label}>
        {action.options.map((option) => (
          <WithTooltip key={option.value} content={option.hint}>
            <Button
              size="sm"
              aria-pressed={action.value === option.value}
              onClick={() => action.onChange(option.value)}
            >
              {option.icon}
              {option.label}
            </Button>
          </WithTooltip>
        ))}
      </ButtonGroup>
    );
  }
  if (action.kind === "choice") {
    return (
      <Tabs value={action.value} onValueChange={(value) => action.onChange(value as string)}>
        <TabsList aria-label={action.label} activateOnFocus>
          {action.options.map((option) => (
            <TabsTrigger key={option.value} value={option.value} title={option.hint}>
              {option.label}
            </TabsTrigger>
          ))}
        </TabsList>
      </Tabs>
    );
  }
  const link = action.href
    ? action.external
      ? <a href={action.href} target="_blank" rel="noopener noreferrer" />
      : <NavLink to={action.href} />
    : undefined;
  return (
    <WithTooltip content={action.hint ?? action.label}>
      <Button
        // Ghost on its own; in the main tools, the group's look, or destructive.
        variant={grouped ? (action.destructive ? "destructive" : undefined) : "ghost"}
        size="icon-sm"
        className={cn(action.destructive && !grouped && "text-destructive")}
        disabled={action.disabled}
        onClick={action.onSelect}
        nativeButton={link ? false : undefined}
        render={link}
        aria-label={action.label}
        aria-keyshortcuts={action.keyShortcuts}
      >
        {action.icon}
      </Button>
    </WithTooltip>
  );
}

function CollapsedTools({ tools: actions }: { tools: CollapsibleTool[] }) {
  const navigate = useNavigate();
  return (
    <DropdownMenu>
      <WithTooltip content="More tools">
        <DropdownMenuTrigger
          render={<Button variant="ghost" size="icon-sm" className="text-muted-foreground" aria-label="More tools" />}
        >
          <MoreIcon />
        </DropdownMenuTrigger>
      </WithTooltip>
      <DropdownMenuContent notch align="end" className="min-w-56">
        {actions.map((action, index) =>
          action.kind === "choice" ? (
            <Fragment key={action.key}>
              {index > 0 && <DropdownMenuSeparator />}
              <DropdownMenuGroup>
                <DropdownMenuLabel>{action.label}</DropdownMenuLabel>
                <DropdownMenuRadioGroup value={action.value} onValueChange={(value) => action.onChange(value as string)}>
                  {action.options.map((option) => (
                    <DropdownMenuRadioItem key={option.value} value={option.value}>
                      {option.label}
                    </DropdownMenuRadioItem>
                  ))}
                </DropdownMenuRadioGroup>
              </DropdownMenuGroup>
              {index < actions.length - 1 && actions[index + 1]?.kind !== "choice" && <DropdownMenuSeparator />}
            </Fragment>
          ) : action.kind === "save" ? (
            <DropdownMenuItem
              key={action.key}
              variant={action.error && !action.saving ? "destructive" : "default"}
              disabled={!saveStatus(action).ready}
              onClick={action.onSave}
            >
              {saveStatus(action).text}
            </DropdownMenuItem>
          ) : (
            <DropdownMenuItem
              key={action.key}
              variant={action.destructive ? "destructive" : "default"}
              disabled={action.disabled}
              onClick={() => {
                if (action.onSelect) action.onSelect();
                else if (action.href && action.external) window.open(action.href, "_blank", "noopener,noreferrer");
                else if (action.href) navigate(action.href);
              }}
            >
              {action.icon}
              {action.label}
            </DropdownMenuItem>
          ),
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
