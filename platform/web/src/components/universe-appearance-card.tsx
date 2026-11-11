import { useRef, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { UNIVERSE_ICONS, UNIVERSE_ICON_COLORS, type UniverseIconName, type UniverseIconColor } from "@lightspeed-ai/platform-shared";
import { api, type Universe } from "@/api";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { UniverseIcon } from "@/components/universe-icon";
import { offeredUniverseIcon } from "@/components/universe-illustrations";
import { cn } from "@/lib/utils";

type Appearance = { icon: UniverseIconName; iconColor: UniverseIconColor };

/// A universe's icon and color. A choice saves at once; a failed save puts
/// back the last saved appearance and says why.
export function UniverseAppearanceCard({ universe }: { universe: Universe }) {
  const queryClient = useQueryClient();
  const [saved, setSaved] = useState<Appearance>({ icon: offeredUniverseIcon(universe.icon), iconColor: universe.iconColor ?? "default" });
  const [shown, setShown] = useState<Appearance>(saved);
  // The latest choice: a slower response to an earlier one must not win.
  const latest = useRef(shown);
  const save = useMutation({
    mutationFn: (next: Appearance) => api<Universe>("PATCH", `/api/v1/universes/${universe.id}`, next),
    onSuccess: (updated, next) => {
      if (latest.current !== next) return;
      setSaved(next);
      queryClient.setQueryData<Universe[]>(["universes"], rows => rows?.map(row => row.id === updated.id ? updated : row));
      void queryClient.invalidateQueries({ queryKey: ["universes"] });
    },
    onError: (_error, next) => {
      if (latest.current !== next) return;
      latest.current = saved;
      setShown(saved);
    },
  });
  function choose(change: Partial<Appearance>) {
    const next = { ...shown, ...change };
    if (next.icon === shown.icon && next.iconColor === shown.iconColor) return;
    latest.current = next;
    setShown(next);
    save.mutate(next);
  }
  return (
    <Card>
      <CardHeader>
        <CardTitle>Appearance</CardTitle>
        <CardDescription>The icon and color shown in the universe switcher for everyone.</CardDescription>
      </CardHeader>
      <CardContent>
        <div className="grid gap-5">
          <div className="flex items-center gap-3" aria-label="Universe appearance preview">
            <UniverseIcon icon={shown.icon} iconColor={shown.iconColor} className="size-11" />
            <span className="truncate text-sm font-medium">{universe.name}</span>
            {save.isPending && <span className="text-xs text-muted-foreground">Saving…</span>}
          </div>
          <fieldset className="grid gap-2">
            <legend className="mb-2 text-sm font-medium">Icon</legend>
            <div className="grid grid-cols-[repeat(auto-fill,minmax(3.125rem,1fr))] justify-items-center gap-2">
              {UNIVERSE_ICONS.map(choice => (
                <Button key={choice} type="button" variant="outline" size="icon" aria-label={`${label(choice)} icon`}
                  title={label(choice)} aria-pressed={shown.icon === choice}
                  className={cn("size-12.5", shown.icon === choice && "ring-2 ring-ring")}
                  onClick={() => choose({ icon: choice })}>
                  {/* Uncolored, so the shapes are easy to compare. */}
                  <UniverseIcon icon={choice} className="size-10 rounded-md" />
                </Button>
              ))}
            </div>
          </fieldset>
          <fieldset className="grid gap-2">
            <legend className="mb-2 text-sm font-medium">Color</legend>
            <div className="grid grid-cols-[repeat(auto-fill,minmax(3.125rem,1fr))] justify-items-center gap-2">
              {UNIVERSE_ICON_COLORS.map(choice => (
                <Button key={choice} type="button" variant="outline" size="icon" aria-label={`${label(choice)} color`}
                  title={label(choice)} aria-pressed={shown.iconColor === choice}
                  className={cn("size-12.5", shown.iconColor === choice && "ring-2 ring-ring")}
                  onClick={() => choose({ iconColor: choice })}>
                  <UniverseIcon icon={shown.icon} iconColor={choice} className="size-10 rounded-md" />
                </Button>
              ))}
            </div>
          </fieldset>
          {save.error && <p role="alert" className="text-sm text-destructive">{save.error.message}</p>}
        </div>
      </CardContent>
    </Card>
  );
}

export function label(value: string) {
  const words = value.replaceAll("-", " ");
  return words.charAt(0).toUpperCase() + words.slice(1);
}
