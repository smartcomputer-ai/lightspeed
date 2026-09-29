import type { ReactNode } from "react";
import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { api, type Environment } from "@/api";
import type { McpServerOption, WorkspaceOption } from "@/components/session/session-config-editor";
import { Clock3, FolderOpen, Globe2, Network, Server, Settings2, Wrench, type LucideIcon } from "lucide-react";
import { sessionResources, type ResourceTone, type SessionResources } from "@/lib/sessions/session-resources";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { cn } from "@/lib/utils";

const TONE_DOT: Record<ResourceTone, string> = {
  ok: "bg-emerald-500",
  idle: "bg-muted-foreground/50",
  busy: "bg-amber-500 animate-pulse",
  problem: "bg-destructive",
};
const TONE_RANK: Record<ResourceTone, number> = { ok: 0, idle: 1, busy: 2, problem: 3 };
const worst = (tones: ResourceTone[]) => tones.reduce<ResourceTone>((a, b) => (TONE_RANK[b] > TONE_RANK[a] ? b : a), "ok");

const ENV_STATUS: Record<string, string> = {
  ready: "Running",
  paused: "Paused · wakes on use",
  suspended: "Suspended · wakes on use",
  stopped: "Stopped · wakes on use",
  provisioning: "Provisioning",
  booting: "Booting",
  closing: "Closing",
  closed: "Closed",
  failed: "Failed",
};
const MCP_STATUS: Record<string, string> = {
  needsAuthConfig: "Needs authentication",
  unverified: "Not verified yet",
  disabled: "Disabled",
};

/// The session's resources, named from the universe catalogs. The catalogs
/// load only while the strip is shown; environment status refreshes so a
/// machine waking or failing shows without a reload.
export function useSessionResources(universeId: string, config: unknown, activeEnvironmentId: string | null | undefined, enabled: boolean) {
  const environments = useQuery({
    queryKey: ["environments", universeId],
    queryFn: () => api<Environment[]>("GET", `/api/v1/universes/${universeId}/environments`),
    enabled,
    refetchInterval: enabled ? 30_000 : false,
  });
  const mcpServers = useQuery({
    queryKey: ["mcp-servers", universeId],
    queryFn: () => api<McpServerOption[]>("GET", `/api/v1/universes/${universeId}/mcp-servers`),
    enabled,
  });
  const workspaces = useQuery({
    queryKey: ["workspaces", universeId],
    queryFn: () => api<WorkspaceOption[]>("GET", `/api/v1/universes/${universeId}/workspaces`),
    enabled,
  });
  return sessionResources(config, activeEnvironmentId, {
    environments: environments.data,
    mcpServers: mcpServers.data,
    workspaces: workspaces.data,
  });
}

const plural = (count: number, word: string) => `${count} ${word}${count === 1 ? "" : "s"}`;

/// What the session can reach, as chips in the strip under its header:
/// its environment, attached workspaces, MCP servers, sub-agent profiles, and
/// web and timer tools. Each chip opens the details and where to change them.
export function SessionResourceChips({ resources, slug, onConfigure }: {
  resources: SessionResources;
  slug: string;
  /// Opens the session's settings; absent when the viewer cannot change them.
  onConfigure?: () => void;
}) {
  const { environments, files, mcp, agents, web, timers } = resources;
  const footer = (page?: { to: string; label: string }) => (
    <Footer onConfigure={onConfigure} page={page} />
  );
  return (
    <span className="flex min-w-0 flex-wrap items-center gap-1" aria-label="Session resources">
      {environments && (
        <Chip
          icon={Server}
          label={environments.active?.label ?? (environments.items.length ? "No active environment" : "No environment")}
          tone={environments.active?.tone}
          muted={!environments.active}
          title="Environment"
        >
          <Heading title="Environment" detail={environments.selection
            ? "The agent may switch between the attached environments."
            : "Commands, jobs, and machine files run here."} />
          {environments.items.length === 0
            ? <Empty>No environments are attached.</Empty>
            : environments.items.map((item) => (
              <Row key={item.key} tone={item.tone}
                primary={item.label}
                secondary={[item.status ? ENV_STATUS[item.status] ?? item.status : "Not in this universe's list", item.access && `${item.access} access`].filter(Boolean).join(" · ")}
                badges={[item.active && "Active", item.isDefault && "Default"]} />
            ))}
          {(environments.instructions || environments.skills) && (
            <Note>{sources(environments.instructions, environments.skills)} load from the active environment.</Note>
          )}
          {footer({ to: `/u/${slug}/environments`, label: "Environments" })}
        </Chip>
      )}
      {files && (
        <Chip
          icon={FolderOpen}
          label={files.workspaces.length === 1 ? files.workspaces[0]!.label : files.workspaces.length ? plural(files.workspaces.length, "workspace") : "No workspaces"}
          tone={files.workspaces.some((workspace) => workspace.missing) ? "problem" : undefined}
          muted={files.workspaces.length === 0}
          title="Files"
        >
          <Heading title="Files" detail="Workspaces mounted into the agent's virtual file system." />
          {files.workspaces.length === 0
            ? <Empty>No workspaces are attached.</Empty>
            : files.workspaces.map((workspace) => (
              <Row key={workspace.path} tone={workspace.missing ? "problem" : undefined}
                primary={workspace.workspaceId && !workspace.missing
                  ? <Link to={`/u/${slug}/workspaces/${encodeURIComponent(workspace.workspaceId)}`} className="hover:underline">{workspace.label}</Link>
                  : workspace.label}
                secondary={[<span key="path" className="font-mono">{workspace.path}</span>, `${workspace.access === "edit" ? "read and edit" : "read only"}`, workspace.pinned && "pinned snapshot", workspace.missing && "not found"]}
              />
            ))}
          {(files.instructions || files.skills) && (
            <Note>{sources(files.instructions, files.skills)} load from these workspaces.</Note>
          )}
          {footer({ to: `/u/${slug}/workspaces`, label: "Workspaces" })}
        </Chip>
      )}
      {mcp && (
        <Chip
          icon={Wrench}
          label={mcp.servers.length === 1 ? mcp.servers[0]!.label : mcp.servers.length ? plural(mcp.servers.length, "MCP server") : "No MCP servers"}
          tone={mcp.servers.length && worst(mcp.servers.map((server) => server.tone)) !== "ok" ? worst(mcp.servers.map((server) => server.tone)) : undefined}
          muted={mcp.servers.length === 0}
          title="MCP servers"
        >
          <Heading title="MCP servers" detail="Remote tools from the universe catalog." />
          {mcp.servers.length === 0
            ? <Empty>No servers are attached.</Empty>
            : mcp.servers.map((server) => (
              <Row key={server.serverId} tone={server.tone}
                primary={server.label}
                secondary={[
                  server.tools ? `${server.tools.length}${server.toolTotal ? ` of ${server.toolTotal}` : ""} tools` : "All allowed tools",
                  server.status && MCP_STATUS[server.status],
                  server.tone === "problem" && !server.status && "Not in the catalog",
                ]}
                badges={[server.approval && "Asks approval"]} />
            ))}
          {footer({ to: `/u/${slug}/mcp-servers`, label: "MCP servers" })}
        </Chip>
      )}
      {agents && (
        <Chip icon={Network} label={plural(agents.profiles.length, "agent")} muted={agents.profiles.length === 0} title="Sub-agent profiles">
          <Heading title="Sub-agent profiles" detail="Profiles the agent may run as sub-agents." />
          {agents.profiles.length === 0
            ? <Empty>No profiles are listed.</Empty>
            : agents.profiles.map((profile) => (
              <Row key={profile}
                primary={<Link to={`/u/${slug}/profiles/${encodeURIComponent(profile)}`} className="font-mono hover:underline">{profile}</Link>} />
            ))}
          {(agents.maxDepth !== undefined || agents.maxConcurrent !== undefined) && (
            <Note>{[agents.maxDepth !== undefined && `Up to ${agents.maxDepth} levels deep`, agents.maxConcurrent !== undefined && `${agents.maxConcurrent} open at once`].filter(Boolean).join(" · ")}.</Note>
          )}
          {footer()}
        </Chip>
      )}
      {web && (
        <Chip icon={Globe2} label={web.search && web.fetch ? "Web" : web.search ? "Web search" : web.fetch ? "Web fetch" : "Web"} title="Web">
          <Heading title="Web" detail="Independent of MCP servers and the environment's network." />
          <Row primary="Search" secondary={web.search ? "Enabled" : "Off"} tone={web.search ? "ok" : "idle"} />
          <Row primary="Fetch pages" secondary={web.fetch ? "Enabled" : "Off"} tone={web.fetch ? "ok" : "idle"} />
          {footer()}
        </Chip>
      )}
      {timers && (
        <Chip icon={Clock3} label="Timers" title="Timers">
          <Heading title="Timers" detail="The agent may wait, sleep, and await delayed work." />
          {footer()}
        </Chip>
      )}
    </span>
  );
}

function sources(instructions: boolean, skills: boolean): string {
  return instructions && skills ? "Instructions and skills" : instructions ? "Instructions" : "Skills";
}

function Chip({ icon: Icon, label, tone, muted, title, children }: {
  icon: LucideIcon;
  label: string;
  tone?: ResourceTone;
  muted?: boolean;
  title: string;
  children: ReactNode;
}) {
  return (
    <Popover>
      <PopoverTrigger render={
        <button type="button" aria-label={`${title}: ${label}`}
          className={cn(
            "inline-flex h-6 max-w-56 min-w-0 items-center gap-1 rounded-full border bg-background/60 px-2 text-xs font-medium text-foreground transition-colors hover:bg-muted",
            muted && "border-dashed text-muted-foreground",
          )} />
      }>
        <Icon className="size-3 shrink-0 text-muted-foreground" aria-hidden />
        <span className="truncate">{label}</span>
        {tone && <span className={cn("size-1.5 shrink-0 rounded-full", TONE_DOT[tone])} aria-hidden />}
      </PopoverTrigger>
      <PopoverContent align="start" className="w-80 p-1.5">
        {children}
      </PopoverContent>
    </Popover>
  );
}

function Heading({ title, detail }: { title: string; detail: string }) {
  return (
    <div className="px-2 pt-1 pb-1.5">
      <p className="text-sm font-medium">{title}</p>
      <p className="text-xs text-muted-foreground">{detail}</p>
    </div>
  );
}

function Row({ primary, secondary, tone, badges = [] }: {
  primary: ReactNode;
  secondary?: ReactNode | ReactNode[];
  tone?: ResourceTone;
  badges?: Array<string | false | undefined>;
}) {
  const parts = (Array.isArray(secondary) ? secondary : [secondary]).filter((part) => part !== undefined && part !== false && part !== "");
  return (
    <div className="flex items-start gap-2 rounded-md px-2 py-1.5">
      <span className={cn("mt-1.5 size-1.5 shrink-0 rounded-full", tone ? TONE_DOT[tone] : "bg-transparent")} aria-hidden />
      <div className="min-w-0 flex-1">
        <div className="flex min-w-0 items-center gap-1.5 text-sm">
          <span className="min-w-0 truncate">{primary}</span>
          {badges.filter(Boolean).map((badge) => (
            <span key={badge as string} className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground">{badge}</span>
          ))}
        </div>
        {parts.length > 0 && (
          <p className="text-xs text-muted-foreground">
            {parts.map((part, index) => <span key={index}>{index > 0 && " · "}{part}</span>)}
          </p>
        )}
      </div>
    </div>
  );
}

function Empty({ children }: { children: ReactNode }) {
  return <p className="px-2 py-1.5 text-xs text-muted-foreground">{children}</p>;
}

function Note({ children }: { children: ReactNode }) {
  return <p className="mx-2 mt-1 border-t pt-1.5 text-xs text-muted-foreground">{children}</p>;
}

function Footer({ onConfigure, page }: { onConfigure?: () => void; page?: { to: string; label: string } }) {
  if (!onConfigure && !page) return null;
  return (
    <div className="mt-1 flex items-center gap-2 border-t px-2 pt-1.5 pb-0.5 text-xs">
      {onConfigure && (
        <button type="button" onClick={onConfigure} className="inline-flex items-center gap-1 font-medium hover:underline">
          <Settings2 className="size-3" aria-hidden />
          Change in session settings
        </button>
      )}
      {page && <Link to={page.to} className="ml-auto text-muted-foreground hover:text-foreground hover:underline">{page.label}</Link>}
    </div>
  );
}
