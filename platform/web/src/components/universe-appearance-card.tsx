import { useState, type FormEvent } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { UNIVERSE_ICONS, UNIVERSE_ICON_COLORS, type UniverseIconName, type UniverseIconColor } from "@lightspeed-ai/platform-shared";
import { api, type Universe } from "@/api";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { UniverseIcon } from "@/components/universe-icon";

export function UniverseAppearanceCard({ universe }: { universe: Universe }) {
  const queryClient = useQueryClient();
  const [icon, setIcon] = useState<UniverseIconName>(universe.icon ?? "orbit");
  const [iconColor, setIconColor] = useState<UniverseIconColor>(universe.iconColor ?? "default");
  const changed = icon !== (universe.icon ?? "orbit") || iconColor !== (universe.iconColor ?? "default");
  const save = useMutation({
    mutationFn: () => api<Universe>("PATCH", `/api/v1/universes/${universe.id}`, { icon, iconColor }),
    onSuccess: (updated) => {
      queryClient.setQueryData<Universe[]>(["universes"], rows => rows?.map(row => row.id === updated.id ? updated : row));
      void queryClient.invalidateQueries({ queryKey: ["universes"] });
    },
  });
  function submit(event: FormEvent) {
    event.preventDefault();
    if (changed && !save.isPending) save.mutate();
  }
  return (
    <Card>
      <CardHeader>
        <CardTitle>Appearance</CardTitle>
        <CardDescription>The icon and color shown in the universe switcher for everyone.</CardDescription>
      </CardHeader>
      <CardContent>
        <form onSubmit={submit} className="grid max-w-md gap-5">
          <div className="flex items-center gap-3" aria-label="Universe appearance preview">
            <UniverseIcon icon={icon} iconColor={iconColor} />
            <span className="truncate text-sm font-medium">{universe.name}</span>
          </div>
          <fieldset disabled={save.isPending} className="grid gap-2">
            <legend className="mb-2 text-sm font-medium">Icon</legend>
            <div className="flex flex-wrap gap-2">
              {UNIVERSE_ICONS.map(choice => (
                <Button key={choice} type="button" variant="outline" size="icon" aria-label={`${label(choice)} icon`}
                  title={label(choice)} aria-pressed={icon === choice}
                  className={icon === choice ? "ring-2 ring-ring" : undefined}
                  onClick={() => setIcon(choice)}>
                  <UniverseIcon icon={choice} iconColor={iconColor} className="size-7 rounded-md" />
                </Button>
              ))}
            </div>
          </fieldset>
          <fieldset disabled={save.isPending} className="grid gap-2">
            <legend className="mb-2 text-sm font-medium">Color</legend>
            <div className="flex flex-wrap gap-2">
              {UNIVERSE_ICON_COLORS.map(choice => (
                <Button key={choice} type="button" variant="outline" size="icon" aria-label={`${label(choice)} color`}
                  title={label(choice)} aria-pressed={iconColor === choice}
                  className={iconColor === choice ? "ring-2 ring-ring" : undefined}
                  onClick={() => setIconColor(choice)}>
                  <UniverseIcon icon={icon} iconColor={choice} className="size-7 rounded-md" />
                </Button>
              ))}
            </div>
          </fieldset>
          <div className="flex flex-wrap gap-2">
            <Button type="submit" disabled={!changed || save.isPending}>{save.isPending ? "Saving…" : "Save appearance"}</Button>
            <Button type="button" variant="outline" disabled={save.isPending || (icon === "orbit" && iconColor === "default")}
              onClick={() => { setIcon("orbit"); setIconColor("default"); }}>Reset to default</Button>
          </div>
          {save.error && <p role="alert" className="text-sm text-destructive">{save.error.message}</p>}
        </form>
      </CardContent>
    </Card>
  );
}

function label(value: string) { return value.charAt(0).toUpperCase() + value.slice(1); }
