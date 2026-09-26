import type { ReactNode } from "react";
import { ChevronDown, Ellipsis, LoaderCircle, Share2, SlidersHorizontal } from "lucide-react";
import { useNavigate } from "react-router-dom";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { SessionMenuIdentity, SessionMenuMetadata } from "@/components/session/session-menu-details";
import { SessionMenuPreferences } from "@/components/session/session-menu-preferences";

/**
 * One menu for a session, on the Sessions page (behind ⋯) and on a bot's
 * conversation tab (behind its chevron): what can be done to the session
 * (settings, sharing, where else it opens, its lifecycle), then view
 * preferences, then its id and metadata. Pages pass their lifecycle items
 * (close and delete, or reset) and own every confirmation.
 */
export function SessionActionsMenu({
  sessionId,
  metadata,
  variant,
  pending = false,
  open,
  onSettings,
  onShare,
  lifecycle,
}: {
  sessionId: string;
  metadata: Record<string, string> | undefined;
  variant: "more" | "tab";
  pending?: boolean;
  open?: { label: string; href: string; icon: ReactNode };
  onSettings?: () => void;
  /** Offered while the session is private and the viewer may share it. */
  onShare?: () => void;
  lifecycle?: ReactNode;
}) {
  const navigate = useNavigate();
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          variant === "more" ? (
            <Button
              variant="ghost"
              size="icon-sm"
              className="shrink-0 text-muted-foreground"
              aria-label="Session actions"
              title="Session actions"
            />
          ) : (
            <button
              type="button"
              aria-label="Conversation menu"
              title="Conversation menu"
              className="flex items-center rounded-sm pr-2 pl-0.5 text-muted-foreground hover:text-foreground"
            />
          )
        }
      >
        {pending ? (
          <LoaderCircle className="size-3.5 animate-spin" />
        ) : variant === "more" ? (
          <Ellipsis />
        ) : (
          <ChevronDown className="size-3.5" />
        )}
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align={variant === "more" ? "end" : "start"}
        className="max-h-[min(28rem,calc(100vh-1rem))] w-80 max-w-[calc(100vw-1rem)]"
      >
        {(onSettings || onShare || open || lifecycle) && (
          <>
            <DropdownMenuGroup>
              {onSettings && (
                <DropdownMenuItem onClick={onSettings}>
                  <SlidersHorizontal /> Session settings
                </DropdownMenuItem>
              )}
              {onShare && (
                <DropdownMenuItem onClick={onShare}>
                  <Share2 /> Share with universe…
                </DropdownMenuItem>
              )}
              {open && (
                <DropdownMenuItem onClick={() => navigate(open.href)}>
                  {open.icon} {open.label}
                </DropdownMenuItem>
              )}
              {lifecycle}
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
