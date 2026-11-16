import type { ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/api";
import { openEnvironmentsQuery } from "@/lib/environments";
import type { McpServerOption, WorkspaceOption } from "@/components/session/session-config-editor";
import { CodeModeIcon, ConfigureIcon, EnvironmentIcon, GoToIcon, ProfileIcon, TimerIcon, ToolsIcon, WebIcon, WorkspaceIcon, type IconComponent } from "@/icons";
import { environmentTone, sessionResources, type ResourceTone, type SessionResources } from "@/lib/sessions/session-resources";
import type { Environment, ProfileSummary } from "@/api";
import type { MetaItem, MetaTone } from "@/components/meta-toolbar";
import {
  MetaDetails,
  MetaDetailsAction,
  MetaDetailsEmpty,
  MetaDetailsFooter,
  MetaDetailsHeading,
  MetaDetailsList,
  MetaDetailsNote,
  MetaDetailsRow,
  RESOURCE_STATUS_TONE,
} from "@/components/meta-details";

/// A resource's tone as a row's status dot; a resource without one has none.
const dot = (tone: ResourceTone | undefined) => (tone ? RESOURCE_STATUS_TONE[tone] : undefined);
const TONE_RANK: Record<ResourceTone, number> = { ok: 0, idle: 1, busy: 2, problem: 3 };
const worst = (tones: ResourceTone[]) => tones.reduce<ResourceTone>((a, b) => (TONE_RANK[b] > TONE_RANK[a] ? b : a), "ok");

const ENV_STATUS: Record<string, string> = {
  ready: "Running",
  paused: "Paused · wakes on use",
  suspended: "Suspended · wakes on use",
  stopped: "Stopped · wakes on use",
  provisioning: "Provisioning",
  unknown: "Status unknown",
  away: "Daemon disconnected",
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
    ...openEnvironmentsQuery(universeId),
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

/// What the session can reach, as meta items for the strip under its
/// header and beside its title (the active environment in list rows too): its environment, attached workspaces, MCP
/// servers, sub-agent profiles, and web, code-mode, and timer tools. Each
/// opens the details and where to change them. Their priorities sit below
/// the session's own state, environment first.
export function sessionResourceItems({ resources, handle, universeId, onConfigure }: {
  resources: SessionResources;
  handle: string;
  /// Names and describes the sub-agent profiles, from the universe's
  /// profiles once their popover opens; without it they show by id.
  universeId?: string;
  /// Opens the session's settings; absent when the viewer cannot change them.
  onConfigure?: () => void;
}): MetaItem[] {
  const { environments, files, mcp, agents, web, codeMode, timers } = resources;
  const footer = (page?: { to: string; label: string }) => (
    <Footer onConfigure={onConfigure} page={page} />
  );
  const environmentDetails = () => environments && (
    <MetaDetails>
      <MetaDetailsHeading title="Environment" description={environments.selection
        ? "The agent may switch between the attached environments."
        : "Commands, jobs, and machine files run here."} />
      {environments.items.length === 0
        ? <MetaDetailsEmpty>No environments are attached.</MetaDetailsEmpty>
        : (
          <MetaDetailsList label="Environments">
            {environments.items.map((item) => (
              <MetaDetailsRow key={item.key} tone={dot(item.tone)}
                primary={item.label}
                secondary={[item.status ? ENV_STATUS[item.status] ?? item.status : "Closed or not in this universe's list", item.access && `${item.access} access`]}
                badges={[item.active && "Active", item.isDefault && "Default"]} />
            ))}
          </MetaDetailsList>
        )}
      {(environments.instructions || environments.skills) && (
        <MetaDetailsNote>{sources(environments.instructions, environments.skills)} load from the active environment.</MetaDetailsNote>
      )}
      {footer({ to: `/u/${handle}/environments`, label: "Environments" })}
    </MetaDetails>
  );
  const items: Array<MetaItem | false | null | undefined> = [
    // The active environment shows in list rows too.
    environments && environments.active && { ...environmentItem(environments.active), details: environmentDetails() },
    environments && !environments.active && chip(
      { key: "environments", icon: EnvironmentIcon, label: environments.items.length ? "No active environment" : "No environment", muted: true, title: "Environment" },
      environmentDetails(),
    ),
    files && chip(
      { key: "files", icon: WorkspaceIcon, label: files.workspaces.length === 1 ? files.workspaces[0]!.label : files.workspaces.length ? plural(files.workspaces.length, "workspace") : "No workspaces", tone: files.workspaces.some((workspace) => workspace.missing) ? "problem" : undefined, muted: files.workspaces.length === 0, title: "Files" },
      <MetaDetails>
        <MetaDetailsHeading title="Files" description="Workspaces mounted into the agent's virtual file system." />
        {files.workspaces.length === 0
          ? <MetaDetailsEmpty>No workspaces are attached.</MetaDetailsEmpty>
          : (
            <MetaDetailsList label="Workspaces">
              {files.workspaces.map((workspace) => (
                <MetaDetailsRow key={workspace.path} tone={workspace.missing ? dot("problem") : undefined}
                  primary={workspace.label}
                  href={workspace.workspaceId && !workspace.missing
                    ? `/u/${handle}/workspaces/${encodeURIComponent(workspace.workspaceId)}`
                    : undefined}
                  secondary={[<span key="path" className="font-mono">{workspace.path}</span>, `${workspace.access === "edit" ? "read and edit" : "read only"}`, workspace.pinned && "pinned snapshot", workspace.missing && "not found"]}
                />
              ))}
            </MetaDetailsList>
          )}
        {(files.instructions || files.skills) && (
          <MetaDetailsNote>{sources(files.instructions, files.skills)} load from these workspaces.</MetaDetailsNote>
        )}
        {footer({ to: `/u/${handle}/workspaces`, label: "Workspaces" })}
      </MetaDetails>,
    ),
    mcp && chip(
      { key: "mcp", icon: ToolsIcon, label: mcp.servers.length === 1 ? mcp.servers[0]!.label : mcp.servers.length ? plural(mcp.servers.length, "MCP server") : "No MCP servers", tone: mcp.servers.length && worst(mcp.servers.map((server) => server.tone)) !== "ok" ? worst(mcp.servers.map((server) => server.tone)) : undefined, muted: mcp.servers.length === 0, title: "MCP servers" },
      <MetaDetails>
        <MetaDetailsHeading title="MCP servers" description="Remote tools from the universe catalog." />
        {mcp.servers.length === 0
          ? <MetaDetailsEmpty>No servers are attached.</MetaDetailsEmpty>
          : (
            <MetaDetailsList label="MCP servers">
              {mcp.servers.map((server) => (
                <MetaDetailsRow key={server.serverId} tone={dot(server.tone)}
                  primary={server.label}
                  secondary={[
                    server.tools ? `${server.tools.length}${server.toolTotal ? ` of ${server.toolTotal}` : ""} tools` : "All allowed tools",
                    server.status && MCP_STATUS[server.status],
                    server.tone === "problem" && !server.status && "Not in the catalog",
                  ]}
                  badges={[server.approval && "Asks approval"]} />
              ))}
            </MetaDetailsList>
          )}
        {footer({ to: `/u/${handle}/mcp-servers`, label: "MCP servers" })}
      </MetaDetails>,
    ),
    agents && chip(
      { key: "agents", icon: ProfileIcon, label: plural(agents.profiles.length, "profile"), muted: agents.profiles.length === 0, title: "Sub-agent profiles" },
      <MetaDetails>
        <MetaDetailsHeading title="Sub-agent profiles" description="Profiles the agent may run as sub-agents." />
        {agents.profiles.length === 0
          ? <MetaDetailsEmpty>No profiles are listed.</MetaDetailsEmpty>
          : (
            <SubAgentProfileRows universeId={universeId} handle={handle} profileIds={agents.profiles} />
          )}
        {(agents.maxDepth !== undefined || agents.maxConcurrent !== undefined) && (
          <MetaDetailsNote>{[agents.maxDepth !== undefined && `Up to ${agents.maxDepth} levels deep`, agents.maxConcurrent !== undefined && `${agents.maxConcurrent} open at once`].filter(Boolean).join(" · ")}.</MetaDetailsNote>
        )}
        {footer()}
      </MetaDetails>,
    ),
    codeMode && chip(
      { key: "codeMode", icon: CodeModeIcon, label: "Code mode", title: "Code mode" },
      <MetaDetails>
        <MetaDetailsHeading title="Code mode" description="The agent may compose its available tools with JavaScript." />
        <MetaDetailsNote>Ordinary tool calls remain available alongside code execution.</MetaDetailsNote>
        {footer()}
      </MetaDetails>,
    ),
    web && chip(
      { key: "web", icon: WebIcon, label: web.search && web.fetch ? "Web" : web.search ? "Web search" : web.fetch ? "Web fetch" : "Web", title: "Web" },
      <MetaDetails>
        <MetaDetailsHeading title="Web" description="Independent of MCP servers and the environment's network." />
        <MetaDetailsList label="Web tools">
          <MetaDetailsRow primary="Search" secondary={web.search ? "Enabled" : "Off"} tone={dot(web.search ? "ok" : "idle")} />
          <MetaDetailsRow primary="Fetch pages" secondary={web.fetch ? "Enabled" : "Off"} tone={dot(web.fetch ? "ok" : "idle")} />
        </MetaDetailsList>
        {footer()}
      </MetaDetails>,
    ),
    timers && chip(
      { key: "timers", icon: TimerIcon, label: "Timers", title: "Timers" },
      <MetaDetails>
        <MetaDetailsHeading title="Timers" description="The agent may wait, sleep, and await delayed work." />
        {footer()}
      </MetaDetails>,
    ),
  ];
  return items.filter((item): item is MetaItem => Boolean(item));
}

/// The sub-agent profiles a session may run, as the sub-agents popover lists
/// sub-agents: each by its name (its id without one), with its description
/// under it, opening its profile. The universe's profiles are read when the
/// popover opens (shared with every other list of them), not before.
function SubAgentProfileRows({ universeId, handle, profileIds }: {
  universeId: string | undefined;
  handle: string;
  profileIds: string[];
}) {
  const profiles = useQuery({
    queryKey: ["profiles", universeId],
    queryFn: () => api<ProfileSummary[]>("GET", `/api/v1/universes/${universeId}/profiles`),
    enabled: Boolean(universeId),
    staleTime: 60_000,
  });
  const byId = new Map((profiles.data ?? []).map((profile) => [profile.profileId, profile]));
  return (
    <MetaDetailsList label="Sub-agent profiles">
      {profileIds.map((profileId) => {
        const profile = byId.get(profileId);
        const name = profile?.displayName?.trim();
        const description = profile?.description?.trim();
        return (
          <MetaDetailsRow
            key={profileId}
            primary={name || profileId}
            mono={!name}
            {...(description ? { secondary: <span className="line-clamp-2">{description}</span> } : {})}
            href={`/u/${handle}/profiles/${encodeURIComponent(profileId)}`}
          />
        );
      })}
    </MetaDetailsList>
  );
}

/// A session's environment as a meta item: its name, colored by its status,
/// everywhere the session shows (a list row, beside a page title, the strip).
function environmentItem({ label, status, tone }: { label: string; status?: string; tone: ResourceTone }): MetaItem {
  return {
    key: "resource:environments",
    label: `Environment: ${label}`,
    icon: EnvironmentIcon,
    text: label,
    tooltip: `Environment ${label}: ${status ? ENV_STATUS[status] ?? status : ENV_STATUS.unknown}`,
    ...TONE[tone],
    priority: RESOURCE_PRIORITY.environments,
  };
}

/// The active environment of a session known only by id (a list row), named
/// and colored from the universe's open environments. None without one.
export function activeEnvironmentItems(environmentId: string | null | undefined, environments: readonly Environment[] | undefined): MetaItem[] {
  if (!environmentId) return [];
  const known = environments?.find((environment) => environment.environmentId === environmentId);
  return [environmentItem({
    label: known?.displayName ?? environmentId,
    ...(known ? { status: known.status } : {}),
    tone: known ? environmentTone(known.status) : "idle",
  })];
}

function sources(instructions: boolean, skills: boolean): string {
  return instructions && skills ? "Instructions and skills" : instructions ? "Instructions" : "Skills";
}

const TONE: Record<ResourceTone, { tone: MetaTone; pulse?: boolean }> = {
  ok: { tone: "success" },
  idle: { tone: "muted" },
  busy: { tone: "warning", pulse: true },
  problem: { tone: "danger" },
};

/// One resource as a full-mode item: its icon and label, a dot for its
/// state, dashed when it stands for an absence, and its details on click.
function chip(
  { key, icon, label, tone, muted, title }: { key: string; icon: IconComponent; label: string; tone?: ResourceTone; muted?: boolean; title: string },
  details: ReactNode,
): MetaItem {
  return {
    key: `resource:${key}`,
    label: `${title}: ${label}`,
    icon,
    text: label,
    ...(tone ? TONE[tone] : {}),
    placeholder: muted,
    priority: RESOURCE_PRIORITY[key] ?? 0,
    // Absences only in the strip; the rest beside the page title too.
    places: muted ? ["strip"] : ["header", "strip"],
    details,
  };
}

const RESOURCE_PRIORITY: Record<string, number> = { environments: 5, mcp: 4, files: 3, agents: 2, codeMode: 1, web: 1, timers: 0 };

/// A resource popover's actions: change it in the session's settings, and
/// open its page in the universe.
function Footer({ onConfigure, page }: { onConfigure?: () => void; page?: { to: string; label: string } }) {
  if (!onConfigure && !page) return null;
  return (
    <MetaDetailsFooter>
      {onConfigure && <MetaDetailsAction icon={ConfigureIcon} onSelect={onConfigure}>Change in session settings</MetaDetailsAction>}
      {page && <MetaDetailsAction icon={GoToIcon} href={page.to}>{page.label}</MetaDetailsAction>}
    </MetaDetailsFooter>
  );
}
