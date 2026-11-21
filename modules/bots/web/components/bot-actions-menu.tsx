import { ConfigureIcon, LoadingIcon, MoreIcon, PauseIcon, ResumeIcon } from "@lightspeed-ai/platform-web/modules/ui/icons";
import { Button, useInButtonGroup } from "@lightspeed-ai/platform-web/modules/ui/button";
import { WithTooltip } from "@lightspeed-ai/platform-web/modules/ui/tooltip";
import { cn } from "@lightspeed-ai/platform-web/modules/ui";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@lightspeed-ai/platform-web/modules/ui/dropdown-menu";
import { MenuIdentity } from "@lightspeed-ai/session-components";
import { useRowMenu } from "@lightspeed-ai/platform-web/modules/ui/lists";

/**
 * A bot's ⋯ menu, shaped like a session's: what can be done to the bot
 * (settings, pausing), then its id. Closing and deleting stay in settings,
 * beside what they end. It is also its list row's right-click menu, opened
 * at the pointer. With `trigger={false}` it has no ⋯ button and opens only
 * from the row, for a header that shows its actions as buttons instead.
 */
export function BotActionsMenu({
  botId,
  onSettings,
  pause,
  trigger = true,
}: {
  botId: string;
  onSettings: () => void;
  /** Whether it has its own ⋯ button; without one, only the list row opens it. */
  trigger?: boolean;
  /** Offered to managers of an open bot; `enabled` is whether it runs now. */
  pause?: { enabled: boolean; pending: boolean; onToggle: () => void };
}) {
  const rowMenu = useRowMenu("bot", botId);
  // Ghost and muted on its own; the group's look in a header's tool group.
  const grouped = useInButtonGroup();
  return (
    <DropdownMenu open={rowMenu.open} onOpenChange={rowMenu.onOpenChange}>
      {trigger && <WithTooltip content="Bot actions"><DropdownMenuTrigger
        render={
          <Button
            variant={grouped ? undefined : "ghost"}
            size="icon-sm"
            className={cn("shrink-0", !grouped && "text-muted-foreground")}
            aria-label="Bot actions"
          />
        }
      >
        {pause?.pending ? <LoadingIcon className="size-3.5 animate-spin" /> : <MoreIcon />}
      </DropdownMenuTrigger></WithTooltip>}
      <DropdownMenuContent notch anchor={rowMenu.anchor} align={rowMenu.anchor ? "start" : "end"} className="w-80 max-w-[calc(100vw-1rem)]">
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
