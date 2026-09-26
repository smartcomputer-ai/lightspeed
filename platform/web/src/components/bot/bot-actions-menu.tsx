import { Ellipsis, LoaderCircle, Pause, Play, SlidersHorizontal } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { MenuIdentity } from "@/components/session/session-menu-details";

/**
 * A bot's ⋯ menu, shaped like a session's: what can be done to the bot
 * (settings, pausing), then its id. Closing and deleting stay in settings,
 * beside what they end.
 */
export function BotActionsMenu({
  botId,
  onSettings,
  pause,
}: {
  botId: string;
  onSettings: () => void;
  /** Offered to managers of an open bot; `enabled` is whether it runs now. */
  pause?: { enabled: boolean; pending: boolean; onToggle: () => void };
}) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button
            variant="ghost"
            size="icon-sm"
            className="shrink-0 text-muted-foreground"
            aria-label="Bot actions"
            title="Bot actions"
          />
        }
      >
        {pause?.pending ? <LoaderCircle className="size-3.5 animate-spin" /> : <Ellipsis />}
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-80 max-w-[calc(100vw-1rem)]">
        <DropdownMenuGroup>
          <DropdownMenuItem onClick={onSettings}>
            <SlidersHorizontal /> Bot settings
          </DropdownMenuItem>
          {pause && (
            <DropdownMenuItem
              disabled={pause.pending}
              onClick={pause.onToggle}
              title={pause.enabled
                ? "Schedules stop and events wait; nothing is lost."
                : "Schedules and delivery resume."}
            >
              {pause.enabled ? <Pause /> : <Play />}
              {pause.enabled ? "Pause bot" : "Resume bot"}
            </DropdownMenuItem>
          )}
        </DropdownMenuGroup>
        <DropdownMenuSeparator />
        <MenuIdentity noun="bot" id={botId} />
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
