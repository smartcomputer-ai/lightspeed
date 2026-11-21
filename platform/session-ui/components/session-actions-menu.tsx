import type { ReactNode } from "react";
import { CompactIcon, ConfigureIcon, LoadingIcon, MoreIcon, ShareIcon } from "@/icons";
import { useNavigate } from "react-router-dom";
import { Button, useInButtonGroup } from "@/components/ui/button";
import { WithTooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { SessionMenuIdentity, SessionMenuMetadata } from "@session-ui/components/session-menu-details";
import { SessionMenuPreferences } from "@session-ui/components/session-menu-preferences";
import { useRowMenu } from "@/lib/row-menu";

/**
 * One menu for a session, on the Sessions page (behind ⋯) and on a bot's
 * conversation tab (behind its chevron): what can be done to the session
 * (settings, sharing, where else it opens), then view preferences and session
 * details. Within the action group, bot compaction follows reset and session
 * compaction precedes close/delete. Pages pass lifecycle items
 * (close and delete, or reset) and own every confirmation. The page's ⋯
 * menu is also its list row's right-click menu, opened at the pointer.
 */
export function SessionActionsMenu({
  sessionId,
  metadata,
  label = "Session actions",
  lifecyclePlacement = "last",
  open,
  onSettings,
  onShare,
  onCompact,
  compactionLabel,
  lifecycle,
}: {
  sessionId: string;
  metadata: Record<string, string> | undefined;
  /** The ⋯ button's name, e.g. "Conversation actions" on a bot's page. */
  label?: string;
  /**
   * Where the lifecycle items go in the action group: a bot conversation's
   * reset before compaction ("first"), a session's close or delete after it
   * ("last").
   */
  lifecyclePlacement?: "first" | "last";
  open?: { label: string; href: string; icon: ReactNode };
  onSettings?: () => void;
  /** Offered while the session is private and the viewer may share it. */
  onShare?: () => void;
  onCompact?: () => void;
  compactionLabel?: string | null;
  lifecycle?: ReactNode;
}) {
  const navigate = useNavigate();
  const rowMenu = useRowMenu("session", sessionId);
  // Ghost and muted on its own; the group's look in a header's tool group.
  const grouped = useInButtonGroup();
  const compactItem = onCompact && (
    <DropdownMenuItem onClick={onCompact} disabled={Boolean(compactionLabel)}>
      {compactionLabel ? <LoadingIcon className="animate-spin" /> : <CompactIcon />}
      {compactionLabel ?? "Compact context"}
    </DropdownMenuItem>
  );
  return (
    <DropdownMenu open={rowMenu.open} onOpenChange={rowMenu.onOpenChange}>
      <WithTooltip content={label}>
        <DropdownMenuTrigger
          render={
            <Button
              variant={grouped ? undefined : "ghost"}
              size="icon-sm"
              className={cn("shrink-0", !grouped && "text-muted-foreground")}
              aria-label={label}
            />
          }
        >
          <MoreIcon />
        </DropdownMenuTrigger>
      </WithTooltip>
      <DropdownMenuContent
        notch
        anchor={rowMenu.anchor}
        align={rowMenu.anchor ? "start" : "end"}
        className="max-h-[min(28rem,calc(100vh-1rem))] w-80 max-w-[calc(100vw-1rem)]"
      >
        {(onSettings || onShare || open || lifecycle || onCompact) && (
          <>
            <DropdownMenuGroup>
              {onSettings && (
                <DropdownMenuItem onClick={onSettings}>
                  <ConfigureIcon /> Session settings
                </DropdownMenuItem>
              )}
              {onShare && (
                <DropdownMenuItem onClick={onShare}>
                  <ShareIcon /> Share with universe…
                </DropdownMenuItem>
              )}
              {open && (
                <DropdownMenuItem onClick={() => navigate(open.href)}>
                  {open.icon} {open.label}
                </DropdownMenuItem>
              )}
              {lifecyclePlacement === "first" && lifecycle}
              {compactItem}
              {lifecyclePlacement === "last" && lifecycle}
            </DropdownMenuGroup>
            <DropdownMenuSeparator />
          </>
        )}
        <SessionMenuPreferences />
        <DropdownMenuSeparator />
        <SessionMenuIdentity sessionId={sessionId} />
        <SessionMenuMetadata metadata={metadata} />
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
