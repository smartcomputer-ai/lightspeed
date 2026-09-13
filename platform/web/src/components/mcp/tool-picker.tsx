import { useEffect, useId, useState } from "react";
import { RotateCcw, Search } from "lucide-react";
import type { McpAdvertisedTool } from "@/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  mcpDiscoveryFailureAction,
  useMcpToolDiscovery,
  type McpToolDiscoverySource,
} from "@/lib/mcp/tool-discovery";

type Props = {
  scope: "server" | "session";
  serverId: string;
  revision?: number;
  source?: McpToolDiscoverySource;
  allowedTools?: string[] | null;
  value?: string[];
  onChange: (tools: string[] | undefined) => void;
  discoveryDisabledReason?: string;
};

export function McpToolPicker(props: Props) {
  return (
    <ToolPicker
      key={JSON.stringify([
        props.source?.universeId,
        props.serverId,
        props.revision,
      ])}
      {...props}
    />
  );
}

function ToolPicker({
  scope,
  serverId,
  revision,
  source,
  allowedTools,
  value,
  onChange,
  discoveryDisabledReason,
}: Props) {
  const id = useId();
  const limited = value !== undefined;
  const [expanded, setExpanded] = useState(limited);
  const open = scope === "server" || expanded;
  const [selectionDraft, setSelectionDraft] = useState(value ?? []);
  const [search, setSearch] = useState("");
  useEffect(() => {
    if (value !== undefined) setSelectionDraft(value);
  }, [value]);
  const discovery = useMcpToolDiscovery({
    source,
    serverId,
    revision,
    enabled: open,
    disabled: Boolean(discoveryDisabledReason),
  });
  const result = discovery.result;
  const advertised = new Map(
    (result?.status === "success" ? result.tools : []).map((tool) => [
      tool.name,
      tool,
    ]),
  );
  const allowed = (name: string) =>
    scope === "server" || allowedTools == null || allowedTools.includes(name);
  const names = [
    ...new Set([
      ...[...advertised.keys()].filter(allowed),
      ...(scope === "session" ? (allowedTools ?? []) : []),
      ...(value ?? []),
    ]),
  ].sort((left, right) => left.localeCompare(right));
  const query = search.trim().toLocaleLowerCase();
  const visible = names.filter((name) => {
    const tool = advertised.get(name);
    return `${name} ${tool?.title ?? ""} ${tool?.description ?? ""}`
      .toLocaleLowerCase()
      .includes(query);
  });
  const allLabel =
    scope === "server" ? "All advertised tools" : "All server-allowed tools";
  const title = scope === "server" ? "Allowed tools" : "Tools";
  return (
    <div className="grid min-w-0 gap-3">
      <div className="grid min-w-0 gap-1">
        <Label htmlFor={open ? `${id}-mode` : undefined}>{title}</Label>
        <p className="text-xs text-muted-foreground">
          {scope === "server"
            ? "Sets the tools available to every profile and session using this server."
            : "Use the server’s allowance, or narrow it for this profile or session."}
        </p>
        {scope === "session" && (
          <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
            <button
              type="button"
              aria-expanded={open}
              className="w-fit cursor-pointer rounded-sm text-left text-xs text-muted-foreground underline-offset-4 outline-none hover:text-foreground hover:underline focus-visible:ring-2 focus-visible:ring-ring"
              aria-controls={`${id}-settings`}
              onClick={() => setExpanded((value) => !value)}
            >
              {open ? "Hide tool settings" : "Customize tools"}
            </button>
            {!open && (
              <span className="text-xs text-muted-foreground">
                · {limited ? `${value.length} tools selected` : allLabel}
              </span>
            )}
          </div>
        )}
      </div>
      {open && (
        <div id={`${id}-settings`} className="grid min-w-0 gap-3">
          <Select
            value={limited ? "selected" : "all"}
            onValueChange={(mode) => {
              if (mode === "all") {
                if (value !== undefined) setSelectionDraft(value);
                onChange(undefined);
              } else onChange(selectionDraft);
            }}
          >
            <SelectTrigger
              id={`${id}-mode`}
              aria-label={
                scope === "server" ? "Allowed tools" : "Session tools"
              }
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">{allLabel}</SelectItem>
              <SelectItem value="selected">Selected tools</SelectItem>
            </SelectContent>
          </Select>
          <p className="text-xs text-muted-foreground">
            {limited
              ? `${value.length} selected. Only these names are included, even if new tools appear.`
              : scope === "server"
                ? "Includes tools this server advertises in the future."
                : "Includes tools this server allows in the future."}
          </p>
          {limited && value.length === 0 && (
            <p role="alert" className="text-xs text-destructive">
              Select at least one tool, or choose {allLabel.toLowerCase()}.
            </p>
          )}
          <div className="flex min-w-0 items-center gap-2">
            <div className="relative min-w-0 flex-1">
              <Search className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
              <Input
                value={search}
                onChange={(event) => setSearch(event.target.value)}
                placeholder="Search tools"
                aria-label="Search MCP tools"
                className="pl-8"
              />
            </div>
            {source && (
              <Button
                type="button"
                variant="outline"
                size="sm"
                disabled={
                  discovery.loading ||
                  Boolean(discoveryDisabledReason) ||
                  !serverId
                }
                onClick={discovery.refresh}
              >
                <RotateCcw
                  className={discovery.loading ? "animate-spin" : ""}
                />
                {discovery.loading ? "Loading…" : "Refresh tools"}
              </Button>
            )}
          </div>
          {discoveryDisabledReason && (
            <p className="text-xs text-muted-foreground">
              {discoveryDisabledReason}
            </p>
          )}
          {discovery.error && (
            <p role="alert" className="text-xs text-destructive">
              {discovery.error}
            </p>
          )}
          {result?.status === "failure" && (
            <div role="alert" className="grid gap-1 text-xs">
              <p className="text-destructive">{result.message}</p>
              <p className="text-muted-foreground">
                {mcpDiscoveryFailureAction(result.code)}
                {!!result.requiredScopes?.length &&
                  ` Required scopes: ${result.requiredScopes.join(", ")}.`}
              </p>
            </div>
          )}
          <div
            className="max-h-64 overflow-y-auto rounded-md border"
            aria-busy={discovery.loading}
          >
            {visible.map((name) => (
              <ToolRow
                key={name}
                name={name}
                tool={advertised.get(name)}
                selected={limited ? value.includes(name) : undefined}
                status={
                  !allowed(name)
                    ? "No longer allowed"
                    : !advertised.has(name)
                      ? result?.status === "success"
                        ? "Not currently advertised"
                        : "Not verified"
                      : undefined
                }
                disabled={!allowed(name) && !value?.includes(name)}
                onChange={(checked) =>
                  onChange(
                    checked
                      ? [...new Set([...(value ?? []), name])]
                      : (value ?? []).filter((tool) => tool !== name),
                  )
                }
              />
            ))}
            {!visible.length && (
              <p className="p-3 text-xs text-muted-foreground">
                {query
                  ? "No tools match your search."
                  : discovery.loading
                    ? "Loading available tools…"
                    : result?.status === "success"
                      ? "No tools available with this server’s allowance."
                      : "Load tools to view the available inventory."}
              </p>
            )}
          </div>
          <p className="text-xs text-muted-foreground">
            Descriptions and badges are supplied by the server; they do not
            grant access.
          </p>
        </div>
      )}
    </div>
  );
}

function ToolRow({
  name,
  tool,
  selected,
  status,
  disabled,
  onChange,
}: {
  name: string;
  tool?: McpAdvertisedTool;
  selected?: boolean;
  status?: string;
  disabled: boolean;
  onChange: (checked: boolean) => void;
}) {
  const id = useId();
  return (
    <div className="flex items-start gap-3 border-b p-3 last:border-b-0">
      {selected !== undefined && (
        <Checkbox
          id={id}
          aria-label={name}
          className="mt-0.5"
          checked={selected}
          disabled={disabled}
          onCheckedChange={(checked) => onChange(checked === true)}
        />
      )}
      <div className="grid min-w-0 flex-1 gap-1">
        <Label
          htmlFor={selected !== undefined ? id : undefined}
          className="break-all text-sm"
        >
          {tool?.title ?? name}
        </Label>
        {tool?.title && tool.title !== name && (
          <span className="break-all font-mono text-xs text-muted-foreground">
            {name}
          </span>
        )}
        <div className="flex flex-wrap gap-1">
          {status && (
            <span
              className={
                status === "No longer allowed"
                  ? "text-xs text-destructive"
                  : "text-xs text-muted-foreground"
              }
            >
              {status}
            </span>
          )}
          {tool?.annotations?.readOnlyHint === true && (
            <Badge variant="outline">read only</Badge>
          )}
          {tool?.annotations?.readOnlyHint === false && (
            <Badge variant="outline">may write</Badge>
          )}
          {tool?.annotations?.destructiveHint === true && (
            <Badge variant="outline">destructive</Badge>
          )}
          {tool?.annotations?.idempotentHint === true && (
            <Badge variant="outline">idempotent</Badge>
          )}
          {tool?.annotations?.openWorldHint === true && (
            <Badge variant="outline">external access</Badge>
          )}
        </div>
        {tool?.description && (
          <p className="whitespace-pre-wrap break-words text-xs text-muted-foreground">
            {tool.description}
          </p>
        )}
      </div>
    </div>
  );
}
