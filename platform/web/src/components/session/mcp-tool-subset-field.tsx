import { useEffect, useId, useState } from "react";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import type { DiscoverMcpTools } from "./session-config-editor";

export function McpToolSubsetField({
  serverId,
  allowedTools,
  value,
  discoverTools,
  onChange,
}: {
  serverId: string;
  allowedTools?: string[] | null;
  value?: string[];
  discoverTools?: DiscoverMcpTools;
  onChange: (tools: string[] | undefined) => void;
}) {
  const id = useId();
  const limited = value !== undefined;
  const [tools, setTools] = useState<string[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string>();
  const [refresh, setRefresh] = useState(0);
  useEffect(() => {
    let cancelled = false;
    setTools([]);
    setError(undefined);
    setLoading(false);
    if (!limited || !serverId || !discoverTools) return;
    setLoading(true);
    void discoverTools(serverId)
      .then((names) => {
        if (!cancelled) setTools(names);
      })
      .catch((error: unknown) => {
        if (!cancelled) setError(error instanceof Error ? error.message : "Unable to load tools.");
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [serverId, limited, discoverTools, refresh]);
  const available = tools.filter((name) => allowedTools == null || allowedTools.includes(name));
  const choices = [...new Set([...available, ...(allowedTools ?? []), ...(value ?? [])])].sort();
  return (
    <div className="mt-3 grid gap-3">
      <div className="flex items-center justify-between gap-3">
        <Label htmlFor={id}>Limit tools for this session</Label>
        <Switch
          id={id}
          checked={limited}
          onCheckedChange={(checked) => onChange(checked ? [] : undefined)}
        />
      </div>
      {!limited && (
        <p className="text-xs text-muted-foreground">Use all tools allowed by the server.</p>
      )}
      {limited && (
        <>
          <p className="text-xs text-muted-foreground">
            {value.length} selected. Select at least one tool to save a subset.
          </p>
          {loading && <p className="text-xs text-muted-foreground">Loading available tools…</p>}
          {error && (
            <p role="alert" className="text-xs text-destructive">
              {error}
            </p>
          )}
          <div className="grid max-h-56 gap-2 overflow-y-auto">
            {choices.map((name) => (
              <Label key={name} className="flex items-center gap-2 font-normal">
                <Checkbox
                  checked={value.includes(name)}
                  onCheckedChange={(checked) =>
                    onChange(
                      checked === true ? [...value, name] : value.filter((tool) => tool !== name),
                    )
                  }
                />
                <span className="break-all font-mono text-xs">{name}</span>
                {allowedTools != null && !allowedTools.includes(name) && (
                  <span className="text-xs text-destructive">No longer allowed</span>
                )}
              </Label>
            ))}
          </div>
          {!loading && !choices.length && (
            <p className="text-xs text-muted-foreground">No tools available.</p>
          )}
          {discoverTools && (
            <Button
              variant="outline"
              size="xs"
              className="w-fit"
              disabled={loading || !serverId}
              onClick={() => setRefresh((value) => value + 1)}
            >
              Refresh tools
            </Button>
          )}
        </>
      )}
    </div>
  );
}
