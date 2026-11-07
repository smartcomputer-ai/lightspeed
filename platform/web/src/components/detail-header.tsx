import { Fragment, type ReactNode } from "react";
import { NavLink, useNavigate } from "react-router-dom";
import { BackIcon, MoreIcon } from "@/icons";
import { PaneBar } from "@/components/list-pane";
import { Button } from "@/components/ui/button";
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
import { cn } from "@/lib/utils";

/// An action of a detail header. Actions are data, not elements, so the
/// header can show each one as a button where there is room and as an item
/// of one ⋯ menu where there is not.
export type DetailHeaderAction =
  | {
      kind?: "button";
      key: string;
      /** The button's accessible name and the menu item's text. */
      label: string;
      icon?: ReactNode;
      onSelect?: () => void;
      /** Opens this app path, or, with `external`, this URL in a new tab. */
      href?: string;
      external?: boolean;
      disabled?: boolean;
      destructive?: boolean;
      /** A filled button with text (Save); others are icon buttons. */
      primary?: boolean;
      /** Text of a primary button and its menu item when not the label, e.g. "Saving…". */
      text?: string;
      hint?: string;
      keyShortcuts?: string;
    }
  | {
      /** One of a few views (Form/JSON, Source/Preview): tabs, or radio items. */
      kind: "choice";
      key: string;
      label: string;
      value: string;
      options: Array<{ value: string; label: string; hint?: string }>;
      onChange: (value: string) => void;
    }
  | {
      /** Shown as is at every width, e.g. a component's own ⋯ menu. */
      kind: "custom";
      key: string;
      node: ReactNode;
    };

/// The header of the detail beside a list. Left: what is shown (title,
/// leading icon, tags); right: details and actions. It is a PaneBar, so it
/// has the list header's height. Where the header is narrow, details hide
/// and every action but custom ones moves into a ⋯ menu.
export function DetailHeader({
  back,
  leading,
  title,
  titleHint,
  titleClassName,
  tags,
  details,
  actions = [],
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
  actions?: DetailHeaderAction[];
}) {
  const collapsible = actions.filter((action) => action.kind !== "custom");
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
      {actions.length > 0 && (
        <div className="flex shrink-0 items-center gap-1">
          {actions.map((action) =>
            action.kind === "custom" ? (
              <Fragment key={action.key}>{action.node}</Fragment>
            ) : (
              <span key={action.key} className="hidden @xl/detail-header:contents">
                <InlineAction action={action} />
              </span>
            ),
          )}
          {collapsible.length > 0 && (
            <span className="contents @xl/detail-header:hidden">
              <CollapsedActions actions={collapsible} />
            </span>
          )}
        </div>
      )}
    </PaneBar>
  );
}

type CollapsibleAction = Exclude<DetailHeaderAction, { kind: "custom" }>;

function InlineAction({ action }: { action: CollapsibleAction }) {
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
    <Button
      variant={action.primary ? "default" : "ghost"}
      size={action.primary ? "sm" : "icon-sm"}
      className={cn(action.destructive && !action.primary && "text-destructive")}
      disabled={action.disabled}
      onClick={action.onSelect}
      nativeButton={link ? false : undefined}
      render={link}
      aria-label={action.primary ? undefined : action.label}
      aria-keyshortcuts={action.keyShortcuts}
      title={action.hint ?? (action.primary ? undefined : action.label)}
    >
      {action.primary ? (action.text ?? action.label) : action.icon}
    </Button>
  );
}

function CollapsedActions({ actions }: { actions: CollapsibleAction[] }) {
  const navigate = useNavigate();
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button variant="ghost" size="icon-sm" className="text-muted-foreground" aria-label="More actions" title="More actions" />
        }
      >
        <MoreIcon />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-56">
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
              {action.text ?? action.label}
            </DropdownMenuItem>
          ),
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
