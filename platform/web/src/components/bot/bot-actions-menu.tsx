import { ConfigureIcon, LoadingIcon, MoreIcon, PauseIcon, ResumeIcon } from "@/icons";
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
import { useRowMenu } from "@/lib/row-menu";

/**
 * A bot's ⋯ menu, shaped like a session's: what can be done to the bot
 * (settings, pausing), then its id. Closing and deleting stay in settings,
 * beside what they end. It is also its list row's right-click menu, opened
 * at the pointer.
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
  const rowMenu = useRowMenu("bot", botId);
  return (
    <DropdownMenu open={rowMenu.open} onOpenChange={rowMenu.onOpenChange}>
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
        {pause?.pending ? <LoadingIcon className="size-3.5 animate-spin" /> : <MoreIcon />}
      </DropdownMenuTrigger>
      <DropdownMenuContent anchor={rowMenu.anchor} align={rowMenu.anchor ? "start" : "end"} className="w-80 max-w-[calc(100vw-1rem)]">
        <DropdownMenuGroup>
          <DropdownMenuItem onClick={onSettings}>
            <ConfigureIcon /> Bot settings
          </DropdownMenuItem>
          {pause && (
            <DropdownMenuItem
              disabled={pause.pending}
              onClick={pause.onToggle}
              title={pause.enabled
                ? "Schedules stop and events wait; nothing is lost."
                : "Schedules and delivery resume."}
            >
              {pause.enabled ? <PauseIcon /> : <ResumeIcon />}
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
