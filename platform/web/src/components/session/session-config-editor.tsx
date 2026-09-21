import { useEffect, useId, useState, type ReactNode } from "react";
import type { WorkspaceAttachmentDraft } from "@/api";
import {
  ChevronDown,
  ChevronRight,
  CalendarClock,
  Clock3,
  FolderOpen,
  Globe2,
  Network,
  Plus,
  Server,
  SlidersHorizontal,
  Tags,
  Trash2,
  Wrench,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Combobox,
  ComboboxChip,
  ComboboxChips,
  ComboboxChipsInput,
  ComboboxContent,
  ComboboxEmpty,
  ComboboxInput,
  ComboboxItem,
  ComboboxList,
  ComboboxValue,
} from "@/components/ui/combobox";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { supportsOpenAiProcessingTier } from "@/lib/sessions/run-options";
import { cn } from "@/lib/utils";
import { resourceFeatureDisableReasons, selectableEnvironments } from "@/lib/sessions/resource-features";
import { McpToolPicker } from "@/components/mcp/tool-picker";
import type { McpToolDiscoverySource } from "@/lib/mcp/tool-discovery";

export type SessionConfig = Record<string, unknown>;
type FeatureName = "vfs" | "web" | "subagents" | "timers" | "environments" | "mcp";

export type McpServerOption = {
  serverId: string;
  displayName?: string | null;
  status?: "active" | "needsAuthConfig" | "unverified" | "disabled";
  allowedTools?: string[] | null;
  revision?: number;
};

export type WorkspaceOption = {
  workspaceId: string;
  displayName?: string | null;
};

export type ModelOption = {
  providerId: string;
  apiKind: string;
  model: string;
  displayName: string;
  createdAtMs?: number | null;
  capabilities: {
    maxInputTokens?: number | null;
    maxOutputTokens?: number | null;
    parallelToolUse?: boolean | null;
    reasoningEfforts?: string[] | null;
  };
};

export type ProfileOption = {
  profileId: string;
  displayName?: string | null;
};

export type EnvironmentOption = {
  environmentId: string;
  displayName?: string | null;
  status?: string;
};

type Props = {
  value?: unknown;
  onChange: (config: SessionConfig | undefined) => void;
  onValidityChange?: (message: string | null) => void;
  mcpServers?: McpServerOption[];
  workspaces?: WorkspaceOption[];
  workspacesLoading?: boolean;
  models?: ModelOption[];
  profiles?: ProfileOption[];
  environments?: EnvironmentOption[];
  allowInherit?: boolean;
  mcpToolDiscovery?: McpToolDiscoverySource;
  featureDisableReasons?: Partial<Record<FeatureName, string>>;
  environmentSetup?: ReactNode;
  metadataSetup?: ReactNode;
  metadataDescription?: string;
  retentionSetup?: ReactNode;
  retentionDescription?: string;
  pinnedApiKind?: string;
  className?: string;
};

type RecordValue = Record<string, unknown>;
type ModelChoice = {
  key: string;
  label: string;
  search: string;
  option?: ModelOption;
};

const featureInfo: Record<
  FeatureName,
  { title: string; description: string; icon: typeof FolderOpen }
> = {
  vfs: {
    title: "Virtual File System: Files, Instructions, Skills",
    description: "Grant access to workspace-attached files and source instructions or skills from them.",
    icon: FolderOpen,
  },
  web: {
    title: "Web",
    description: "Grant independent web search and fetch capabilities.",
    icon: Globe2,
  },
  subagents: {
    title: "Sub-agents",
    description: "Let the agent run listed profiles as sub-agents (agent_run / agent_spawn) within root-scoped limits.",
    icon: Network,
  },
  timers: {
    title: "Timers",
    description: "Allow delayed work and promise controls such as sleep and await.",
    icon: Clock3,
  },
  environments: {
    title: "Environments",
    description: "Grant access to universe environments and active-environment tools.",
    icon: Server,
  },
  mcp: {
    title: "MCP Servers",
    description: "Declare remote tools from the universe MCP catalog.",
    icon: Wrench,
  },
};

const featureDisplayOrder: FeatureName[] = [
  "environments",
  "vfs",
  "mcp",
  "subagents",
  "web",
  "timers",
];

const environmentAccessDescriptions: Record<string, string> = {
  read: "Includes reading files.",
  edit: "Includes reading and editing files.",
  exec: "Includes reading and editing files, and running commands.",
  jobs: "Includes reading and editing files, running commands, and running durable jobs.",
};

function record(value: unknown): RecordValue {
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as RecordValue)
    : {};
}

function string(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function stringList(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

function subagentProfileIds(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value
    .map((item) => (typeof item === "string" ? item : string(record(item).profileId)).trim())
    .filter(Boolean);
}

function commaList(value: unknown): string {
  return stringList(value).join(", ");
}

function listFromInput(value: string): string[] {
  return value
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
}

function numberString(value: unknown): string {
  return typeof value === "number" && Number.isFinite(value) ? String(value) : "";
}

function parseNumber(value: string): number | undefined {
  if (!value.trim()) return undefined;
  const number = Number(value);
  return Number.isInteger(number) && number >= 0 ? number : undefined;
}

function omitEmptyRecord(value: RecordValue): RecordValue | undefined {
  return Object.keys(value).length > 0 ? value : undefined;
}

/**
 * Keep the wire document sparse. Feature presence is meaningful, but fields
 * whose absence delegates to the engine default never need to be written.
 */
export function normalizeSessionConfig(value: unknown): SessionConfig | undefined {
  const source = record(value);
  const result: RecordValue = {};

  const model = record(source.model);
  const modelResult = Object.fromEntries(
    ["providerId", "apiKind", "model"]
      .map((key) => [key, string(model[key]).trim()] as const)
      .filter(([, item]) => item),
  );
  if (Object.keys(modelResult).length) result.model = modelResult;

  const generation = record(source.generation);
  const generationResult: RecordValue = {};
  for (const key of ["maxOutputTokens"] as const) {
    const number = parseNumber(numberString(generation[key]));
    if (number !== undefined) generationResult[key] = number;
  }
  for (const key of ["reasoningEffort"] as const) {
    const item = string(generation[key]).trim();
    if (item) generationResult[key] = item;
  }
  const processingTier = string(generation.processingTier);
  if (
    supportsOpenAiProcessingTier({ model: modelResult }) &&
    ["standard", "fast", "flex"].includes(processingTier)
  ) {
    generationResult.processingTier = processingTier;
  }
  if (typeof generation.parallelToolUse === "boolean") {
    generationResult.parallelToolUse = generation.parallelToolUse;
  }
  const toolChoice = record(generation.toolChoice);
  if (string(toolChoice.type)) {
    if (toolChoice.type === "specific") {
      generationResult.toolChoice = { type: "specific", toolId: string(toolChoice.toolId).trim() };
    } else if (["auto", "none", "requiredAny"].includes(string(toolChoice.type))) {
      generationResult.toolChoice = { type: toolChoice.type };
    }
  }
  if (omitEmptyRecord(generationResult)) result.generation = generationResult;

  const limits = record(source.limits);
  const limitsResult: RecordValue = {};
  for (const key of ["maxTurns", "maxToolRounds"] as const) {
    const number = parseNumber(numberString(limits[key]));
    if (number !== undefined) limitsResult[key] = number;
  }
  if (omitEmptyRecord(limitsResult)) result.limits = limitsResult;

  const context = record(source.context);
  const compaction = record(context.compaction);
  const mode = string(compaction.mode);
  if (mode && mode !== "default") {
    const compactResult: RecordValue = { mode };
    for (const key of ["compactThresholdTokens", "targetTokens"] as const) {
      const number = parseNumber(numberString(compaction[key]));
      if (number !== undefined) compactResult[key] = number;
    }
    result.context = { compaction: compactResult };
  }

  const sourceFeatures = record(source.features);
  const features: RecordValue = {};
  for (const name of Object.keys(featureInfo) as FeatureName[]) {
    if (!(name in sourceFeatures)) continue;
    const feature = record(sourceFeatures[name]);
    const next: RecordValue = {};

    if (name === "vfs") {
      if (string(feature.workingDirectory)) next.workingDirectory = feature.workingDirectory;
      next.workspaces = Array.isArray(feature.workspaces) ? feature.workspaces.map((item) => {
        const attachment = record(item);
        return {
          path: string(attachment.path).trim(),
          access: string(attachment.access) || ("snapshotRef" in attachment ? "read" : "edit"),
          ...("workspaceId" in attachment ? { workspaceId: string(attachment.workspaceId).trim() } : {}),
          ...("snapshotRef" in attachment ? { snapshotRef: string(attachment.snapshotRef).trim() } : {}),
        };
      }) : [];
      for (const key of ["prompts", "skills"] as const) {
        const roots = stringList(record(feature[key]).roots).filter(Boolean);
        if (feature[key] != null) {
          next[key] = record(feature[key]).roots == null ? {} : { roots };
        }
      }
    }
    if (name === "web") {
      for (const key of ["fetch", "search"] as const) {
        if (!(key in feature)) continue;
        if (key === "fetch") next.fetch = {};
        if (key === "search") {
          const search = record(feature.search);
          const searchResult: RecordValue = {};
          for (const listKey of ["allowedDomains", "blockedDomains"] as const) {
            const domains = stringList(search[listKey]).filter(Boolean);
            if (domains.length) searchResult[listKey] = domains;
          }
          next.search = searchResult;
        }
      }
      // A web capability with neither sub-grant is rejected by the engine.
      if (!("fetch" in next) && !("search" in next)) continue;
    }
    if (name === "subagents") {
      const agents = subagentProfileIds(feature.agents);
      next.agents = agents.map((profileId) => ({ profileId }));
      for (const key of ["maxDepth", "maxDescendants", "maxConcurrent", "deadlineMs"] as const) {
        const value = parseNumber(numberString(feature[key]));
        if (value !== undefined && value > 0) next[key] = value;
      }
    }
    if (name === "environments") {
      if (feature.selection === true) next.selection = true;
      next.environments = Array.isArray(feature.environments) ? feature.environments.map((item) => {
        const attachment = record(item);
        return {
          ...("environmentId" in attachment ? { environmentId: string(attachment.environmentId).trim() } : {}),
          ...(attachment.inherit === true ? { inherit: true } : {}),
          ...(attachment.default === true ? { default: true } : {}),
          access: string(attachment.access) || "read",
          ...(string(attachment.workingDirectory) ? { workingDirectory: string(attachment.workingDirectory) } : {}),
        };
      }) : [];
      for (const key of ["skills", "prompts"] as const) {
        if (feature[key] != null) {
          const source = record(feature[key]);
          next[key] = source.roots == null ? {} : { roots: stringList(source.roots) };
        }
      }
    }
    if (name === "mcp") {
      const servers = Array.isArray(feature.servers)
        ? feature.servers.map((item) => {
              const server = record(item);
              const serverId = string(server.serverId).trim();
              return { serverId, ...(Array.isArray(server.tools) ? { tools: stringList(server.tools) } : {}) };
            })
        : [];
      next.servers = servers;
    }

    // Versions are supplied by admission; leaving the current version out
    // keeps authored documents forward-compatible and minimal.
    features[name] = next;
  }
  if (omitEmptyRecord(features)) result.features = features;

  return omitEmptyRecord(result);
}

export function configError(config: SessionConfig | undefined, pinnedApiKind?: string, allowInherit = false): string | null {
  if (!config) return null;
  const model = record(config.model);
  if (Object.keys(model).length && !["providerId", "apiKind", "model"].every((key) => string(model[key]))) {
    return "A model override needs provider id, API kind, and model name.";
  }
  const toolChoice = record(record(config.generation).toolChoice);
  if (toolChoice.type === "specific" && !string(toolChoice.toolId)) {
    return "A specific tool choice needs a tool id.";
  }
  const search = record(record(record(config.features).web).search);
  if (
    (pinnedApiKind ?? string(model.apiKind)) === "anthropic:messages"
    && stringList(search.allowedDomains).length
    && stringList(search.blockedDomains).length
  ) {
    return "Anthropic web search accepts either allowed domains or blocked domains, not both.";
  }
  const mcp = record(record(config.features).mcp);
  if ("mcp" in record(config.features) && (!Array.isArray(mcp.servers) || mcp.servers.some((server) => !string(record(server).serverId)))) {
    return "Each enabled MCP server needs a server id.";
  }
  const serverIds = new Set<string>();
  for (const item of Array.isArray(mcp.servers) ? mcp.servers : []) {
    const server = record(item);
    const id = string(server.serverId);
    if (serverIds.has(id)) return "Each MCP server may be attached only once.";
    serverIds.add(id);
    if (Array.isArray(server.tools)) {
      if (!server.tools.length) return "Choose at least one tool for each MCP subset, or use all allowed tools.";
      const names = stringList(server.tools);
      if (names.length !== server.tools.length || names.some((name) => !name.trim()) || new Set(names).size !== names.length) return "MCP tool selections must contain distinct nonempty names.";
    }
  }
  const subagents = record(record(config.features).subagents);
  if ("subagents" in record(config.features) && !subagentProfileIds(subagents.agents).length) {
    return "Sub-agents require at least one agent profile.";
  }
  const attachmentError = workspaceAttachmentsError(workspaceAttachmentsFromConfig(config));
  if (attachmentError) return attachmentError;
  const features = record(config.features);
  const vfs = record(features.vfs);
  for (const key of ["skills", "prompts"] as const) {
    const source = record(vfs[key]);
    if (source.roots == null) continue;
    const label = key === "skills" ? "VFS skill" : "VFS prompt";
    const roots = stringList(source.roots);
    if (!roots.length) return `${label} root overrides must not be empty; clear the override to use defaults.`;
    const attachments = workspaceAttachmentsFromConfig(config);
    if (roots.some((root) => !isCanonicalAbsolutePath(root)
      || !attachments.some((attachment) => attachment.path === "/" || root === attachment.path || root.startsWith(`${attachment.path}/`)))) {
      return `${label} roots must be absolute paths inside workspace attachments.`;
    }
  }
  const vfsCwd = string(vfs.workingDirectory);
  if (vfsCwd && (!isCanonicalAbsolutePath(vfsCwd) || (vfsCwd !== "/" && !workspaceAttachmentsFromConfig(config).some((attachment) => attachment.path === "/" || vfsCwd === attachment.path || vfsCwd.startsWith(`${attachment.path}/`))))) {
    return "VFS working directory must be / or an absolute path inside a workspace attachment.";
  }
  const environment = record(features.environments);
  const attachments = Array.isArray(environment.environments) ? environment.environments.map(record) : [];
  const ids = new Set<string>();
  let inherited = 0;
  let defaults = 0;
  for (const attachment of attachments) {
    const id = string(attachment.environmentId);
    if (attachment.inherit === true) {
      if (!allowInherit) return "Inheriting an environment is only available in sub-agent profiles.";
      if (attachment.environmentId != null || ++inherited > 1) return "Use at most one inherited environment, without an environment id.";
    } else {
      if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(id)) return "Each environment attachment needs a valid environment id.";
      if (ids.has(id)) return "Each environment may be attached only once.";
      ids.add(id);
    }
    if (attachment.default === true && ++defaults > 1) return "Choose at most one default environment.";
    if (!["read", "edit", "exec", "jobs"].includes(string(attachment.access))) return "Choose an access level for each environment.";
    if (string(attachment.workingDirectory) && !string(attachment.workingDirectory).startsWith("/")) return "Environment working directory must be absolute.";
  }
  for (const key of ["skills", "prompts"] as const) {
    const roots = record(environment[key]).roots;
    if (roots != null && !stringList(roots).length) return "Environment source root overrides must not be empty; clear the override to use defaults.";
  }
  return null;
}

export function workspaceAttachmentsFromConfig(config: unknown): WorkspaceAttachmentDraft[] {
  const attachments = record(record(record(config).features).vfs).workspaces;
  return Array.isArray(attachments)
    ? structuredClone(attachments.filter((attachment) => attachment && typeof attachment === "object")) as WorkspaceAttachmentDraft[]
    : [];
}

export function workspaceAttachmentsError(attachments: WorkspaceAttachmentDraft[]): string | null {
  const paths: string[] = [];
  for (const attachment of attachments) {
    const path = attachment.path?.trim() ?? "";
    if (!path) return "Each workspace attachment needs a session path.";
    if (!isCanonicalAbsolutePath(path)) {
      return `Workspace attachment path must be canonical and absolute: ${path}`;
    }
    if (paths.some((existing) => pathsOverlap(existing, path))) {
      return `Workspace attachment paths cannot overlap: ${path}`;
    }
    paths.push(path);
    if (("workspaceId" in attachment) === ("snapshotRef" in attachment)) return `Workspace attachment ${path} needs exactly one workspace or snapshot.`;
    if ("workspaceId" in attachment && !attachment.workspaceId?.trim()) return `Workspace attachment ${path} needs a workspace.`;
    if ("snapshotRef" in attachment) {
      if (!/^sha256:[a-f0-9]{64}$/.test(attachment.snapshotRef ?? "")) return `Workspace attachment ${path} needs a valid snapshot ref.`;
      if (attachment.access !== "read") return `Snapshot attachment ${path} must be read only.`;
    }
    if (attachment.access !== "read" && attachment.access !== "edit") return `Workspace attachment ${path} needs read or edit access.`;
  }
  return null;
}

export function mcpAttachmentError(config: unknown, servers: McpServerOption[]): string | null {
  const attachments = record(record(record(config).features).mcp).servers;
  for (const item of Array.isArray(attachments) ? attachments : []) {
    const attachment = record(item);
    const server = servers.find((server) => server.serverId === attachment.serverId);
    if (server?.allowedTools != null && Array.isArray(attachment.tools)) {
      const denied = attachment.tools.find((name) => !server.allowedTools!.includes(String(name)));
      if (denied !== undefined)
        return `Tool ${String(denied)} is not allowed by server ${server.serverId}.`;
    }
  }
  return null;
}

function isCanonicalAbsolutePath(path: string): boolean {
  if (path === "/") return true;
  if (!path.startsWith("/") || (path.length > 1 && path.endsWith("/"))) return false;
  return path.split("/").slice(1).every((segment) => segment && segment !== "." && segment !== "..");
}

function pathsOverlap(left: string, right: string): boolean {
  return left === right || left === "/" || right === "/"
    || left.startsWith(`${right}/`) || right.startsWith(`${left}/`);
}

export function SessionConfigEditor({
  value,
  onChange,
  onValidityChange,
  mcpServers = [],
  workspaces = [],
  workspacesLoading = false,
  models = [],
  profiles = [],
  environments = [],
  allowInherit = false,
  mcpToolDiscovery,
  featureDisableReasons = {},
  environmentSetup,
  metadataSetup,
  metadataDescription = "Descriptive key/value pairs for finding sessions. Metadata never changes how a session runs.",
  retentionSetup,
  retentionDescription = "Delete the retained session tree automatically after its root session closes.",
  pinnedApiKind,
  className,
}: Props) {
  const config = normalizeSessionConfig(value) ?? {};
  const error = configError(config, pinnedApiKind, allowInherit) ?? mcpAttachmentError(config, mcpServers);
  const [manualModel, setManualModel] = useState(false);

  useEffect(() => onValidityChange?.(error), [error, onValidityChange]);

  const change = (mutate: (next: RecordValue) => void) => {
    const next = structuredClone(config) as RecordValue;
    mutate(next);
    onChange(normalizeSessionConfig(next));
  };

  const features = record(config.features);
  const disableReasons = { ...featureDisableReasons, ...resourceFeatureDisableReasons({ config }) };
  const setFeature = (name: FeatureName, enabled: boolean) =>
    change((next) => {
      const nextFeatures = record(next.features);
      if (enabled) {
        if (name === "vfs") nextFeatures.vfs = { workspaces: [], prompts: {}, skills: {} };
        else if (name === "web") nextFeatures.web = { search: {}, fetch: {} };
        else if (name === "mcp") {
          nextFeatures.mcp = { servers: [] };
        }
        else nextFeatures[name] = {};
      } else {
        delete nextFeatures[name];
      }
      if (Object.keys(nextFeatures).length) next.features = nextFeatures;
      else delete next.features;
    });

  const patchFeature = (name: FeatureName, mutate: (feature: RecordValue) => void) =>
    change((next) => {
      const nextFeatures = record(next.features);
      const feature = record(nextFeatures[name]);
      mutate(feature);
      nextFeatures[name] = feature;
      next.features = nextFeatures;
    });

  return (
    <div className={cn("grid min-w-0 max-w-full gap-8", className)}>
      <section className="grid min-w-0 gap-5">
        <ModelFields
          config={config}
          models={models}
          manualModel={manualModel}
          onManualModelChange={setManualModel}
          pinnedApiKind={pinnedApiKind}
          change={change}
        />
        <AdvancedFields config={config} change={change} />
      </section>

      <section className="grid min-w-0 gap-4">
        <div className="grid gap-0.5">
          <h2 className="text-sm font-semibold">Features</h2>
          <p className="text-xs text-muted-foreground">
            Features are capability grants. Disabled features are absent from the config.
          </p>
        </div>
        <div className="grid min-w-0 gap-2">
          {featureDisplayOrder
            .map((name) => name === "environments" ? (
              <EnvironmentFeatureEditor
                key={name}
                value={config}
                environments={environments}
                allowInherit={allowInherit}
                disableReason={disableReasons.environments}
                onChange={onChange}
              >
                {environmentSetup}
              </EnvironmentFeatureEditor>
            ) : (
              <FeaturePanel
                key={name}
                name={name}
                enabled={name in features}
                feature={record(features[name])}
                disableReason={disableReasons[name]}
                expandable={name !== "timers"}
                onEnabledChange={(enabled) => setFeature(name, enabled)}
              >
                {name === "vfs" && (
                  <VfsFields
                    feature={record(features.vfs)}
                    environmentsGranted={"environments" in features}
                    workspaces={workspaces}
                    workspacesLoading={workspacesLoading}
                    patch={(fn) => patchFeature("vfs", fn)}
                  />
                )}
                {name === "web" && (
                  <WebFields
                    feature={record(features.web)}
                    apiKind={pinnedApiKind ?? string(record(config.model).apiKind)}
                    patch={(fn) => patchFeature("web", fn)}
                  />
                )}
                {name === "subagents" && <SubagentFields feature={record(features.subagents)} profiles={profiles} patch={(fn) => patchFeature("subagents", fn)} />}
                {name === "mcp" && <McpFields feature={record(features.mcp)} servers={mcpServers} discoverySource={mcpToolDiscovery} patch={(fn) => patchFeature("mcp", fn)} />}
              </FeaturePanel>
            ))}
          {(metadataSetup || retentionSetup) && (
            <div className="grid gap-0.5 pb-1 pt-5">
              <h2 className="text-sm font-semibold">Session data</h2>
              <p className="text-xs text-muted-foreground">
                Organize sessions with metadata and control how long retained data is kept.
              </p>
            </div>
          )}
          {metadataSetup && (
            <ExpandableSetupPanel
              title="Session metadata"
              description={metadataDescription}
              icon={Tags}
            >
              {metadataSetup}
            </ExpandableSetupPanel>
          )}
          {retentionSetup && (
            <ExpandableSetupPanel
              title="Automatic deletion"
              description={retentionDescription}
              icon={CalendarClock}
            >
              {retentionSetup}
            </ExpandableSetupPanel>
          )}
        </div>
      </section>
    </div>
  );
}

function ExpandableSetupPanel({
  title,
  description,
  icon: Icon,
  children,
}: {
  title: string;
  description: string;
  icon: typeof Tags;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  return (
    <div className="min-w-0 max-w-full rounded-lg border">
      <button
        type="button"
        aria-expanded={open}
        className="flex min-h-16 w-full min-w-0 cursor-pointer items-center gap-3 rounded-md px-4 py-3 text-left outline-none focus-visible:ring-2 focus-visible:ring-ring"
        onClick={() => setOpen((value) => !value)}
      >
        <Icon className="size-4 shrink-0 text-muted-foreground" />
        <span className="min-w-0 flex-1">
          <span className="block text-sm font-medium">{title}</span>
          <span className="block text-xs text-muted-foreground">{description}</span>
        </span>
        <span className="flex size-8 shrink-0 items-center justify-center" aria-hidden="true">
          {open ? <ChevronDown /> : <ChevronRight />}
        </span>
      </button>
      {open && <div className="min-w-0 border-t px-4 py-4">{children}</div>}
    </div>
  );
}

/**
 * Environment attachments define access; the optional child controls manage
 * the active selection of an existing session.
 */
function EnvironmentFeatureEditor({
  value,
  environments = [],
  allowInherit = false,
  disableReason,
  children,
  onChange,
}: {
  value?: unknown;
  environments?: EnvironmentOption[];
  allowInherit?: boolean;
  disableReason?: string;
  children?: ReactNode;
  onChange: (config: SessionConfig | undefined) => void;
}) {
  const config = normalizeSessionConfig(value) ?? {};
  const features = record(config.features);
  const enabled = "environments" in features;
  const change = (mutate: (next: RecordValue) => void) => {
    const next = structuredClone(config) as RecordValue;
    mutate(next);
    onChange(normalizeSessionConfig(next));
  };
  const setEnabled = (nextEnabled: boolean) => change((next) => {
    const nextFeatures = record(next.features);
    if (nextEnabled) nextFeatures.environments = { environments: [], prompts: {}, skills: {} };
    else delete nextFeatures.environments;
    if (Object.keys(nextFeatures).length) next.features = nextFeatures;
    else delete next.features;
  });
  const patch = (mutate: (feature: RecordValue) => void) => change((next) => {
    const nextFeatures = record(next.features);
    const feature = record(nextFeatures.environments);
    mutate(feature);
    nextFeatures.environments = feature;
    next.features = nextFeatures;
  });

  return (
    <FeaturePanel
      name="environments"
      enabled={enabled}
      feature={record(features.environments)}
      disableReason={disableReason}
      onEnabledChange={setEnabled}
    >
      <div className="grid gap-4">
        <EnvironmentFields
          feature={record(features.environments)}
          environments={environments}
          allowInherit={allowInherit}
          patch={patch}
        />
        {children && <div className="grid gap-3 border-t pt-4">{children}</div>}
      </div>
    </FeaturePanel>
  );
}

function ModelFields({ config, models, manualModel, onManualModelChange, pinnedApiKind, change }: {
  config: RecordValue;
  models: ModelOption[];
  manualModel: boolean;
  onManualModelChange: (enabled: boolean) => void;
  pinnedApiKind?: string;
  change: (fn: (next: RecordValue) => void) => void;
}) {
  const model = record(config.model);
  const currentModel = models.find(
    (option) =>
      option.providerId === string(model.providerId) &&
      option.apiKind === string(model.apiKind) &&
      option.model === string(model.model),
  );
  const hasModel = Object.keys(model).length > 0;
  const selected = currentModel
    ? modelOptionKey(currentModel)
    : manualModel || hasModel
      ? "manual"
      : "default";
  const choices: ModelChoice[] = [
    ...(!pinnedApiKind ? [{
      key: "default",
      label: "Deployment default",
      search: "deployment default",
    }] : []),
    ...modelPickerOptions(models, currentModel, pinnedApiKind)
      .map((option) => ({
        key: modelOptionKey(option),
        label: `${option.displayName} (${option.providerId})`,
        search: `${option.displayName} ${option.providerId} ${option.apiKind} ${option.model}`,
        option,
      })),
    {
      key: "manual",
      label: "Enter model manually",
      search: "manual custom model",
    },
  ];
  const selectedChoice = choices.find((choice) => choice.key === selected) ?? {
    key: "manual",
    label: "Custom model",
    search: "custom model",
  };
  const choiceKeys = choices.map((choice) => choice.key);
  const [modelSearch, setModelSearch] = useState(selectedChoice.label);
  useEffect(() => {
    setModelSearch(selectedChoice.label);
  }, [selectedChoice.label]);
  const update = (key: string, value: string) =>
    change((next) => {
      const nextModel = record(next.model);
      nextModel[key] = value;
      if (pinnedApiKind) nextModel.apiKind = pinnedApiKind;
      next.model = nextModel;
    });
  const generation = record(config.generation);
  const reasoningEffort = string(generation.reasoningEffort);
  const reasoningOptions = [
    ...new Set([
      ...(currentModel?.capabilities.reasoningEfforts ?? []),
      ...(reasoningEffort ? [reasoningEffort] : []),
    ]),
  ];
  const reasoningOptionsId = useId();
  const updateReasoningEffort = (value: string | undefined) =>
    change((next) => {
      const nextGeneration = record(next.generation);
      if (value) nextGeneration.reasoningEffort = value;
      else delete nextGeneration.reasoningEffort;
      if (Object.keys(nextGeneration).length) next.generation = nextGeneration;
      else delete next.generation;
    });
  return (
    <div className="grid gap-3">
      <div className="grid gap-3 md:grid-cols-2">
        <Field>
        <FieldLabel>Model</FieldLabel>
        <Combobox
          items={choiceKeys}
          value={selected}
          inputValue={modelSearch}
          autoHighlight
          itemToStringLabel={(key) => choices.find((choice) => choice.key === key)?.label ?? key}
          filter={(key, query) => {
            const choice = choices.find((item) => item.key === key);
            return choice?.search.toLocaleLowerCase().includes(query.toLocaleLowerCase()) ?? false;
          }}
          onInputValueChange={setModelSearch}
          onValueChange={(key) => {
            if (!key) return;
            const choice = choices.find((item) => item.key === key);
            if (!choice) return;
            setModelSearch(choice.label);
            if (choice.key === "default") {
              onManualModelChange(false);
              change((next) => {
                delete next.model;
              });
              return;
            }
            if (choice.key === "manual") {
              onManualModelChange(true);
              return;
            }
            const option = choice.option;
            if (!option) return;
            onManualModelChange(false);
            change((next) => {
              next.model = {
                providerId: option.providerId,
                apiKind: option.apiKind,
                model: option.model,
              };
            });
          }}
        >
          <ComboboxInput
            id="session-config-model"
            className="w-full"
            placeholder="Search models..."
            aria-label="Search models"
          />
          <ComboboxContent>
            <ComboboxEmpty>No matching models.</ComboboxEmpty>
            <ComboboxList>
              {(key: string) => {
                const choice = choices.find((item) => item.key === key);
                if (!choice) return null;
                return (
                  <ComboboxItem
                    key={choice.key}
                    value={choice.key}
                    className="py-2"
                  >
                    <span className="min-w-0">
                      <span className="block truncate">{choice.label}</span>
                      {choice.option && (
                        <span className="block truncate text-xs text-muted-foreground">
                          {choice.option.displayName !== choice.option.model
                            ? choice.option.model + " · "
                            : ""}
                          {choice.option.apiKind}
                          {choice.option.createdAtMs
                            ? " · created " +
                              formatModelCreatedAt(choice.option.createdAtMs)
                            : ""}
                        </span>
                      )}
                    </span>
                  </ComboboxItem>
                );
              }}
            </ComboboxList>
          </ComboboxContent>
        </Combobox>
        <FieldDescription className="text-xs">
          {currentModel
            ? `${currentModel.providerId} · ${currentModel.apiKind}`
            : models.length
              ? "Choose a provider-discovered model, or enter a route manually."
              : "No models were discovered. You can still enter a route manually."}
        </FieldDescription>
        {currentModel && <ModelCapabilities option={currentModel} />}
        </Field>
        <Field>
          <FieldLabel>Reasoning effort</FieldLabel>
          <Input
            value={reasoningEffort}
            list={reasoningOptions.length ? reasoningOptionsId : undefined}
            onChange={(e) => updateReasoningEffort(e.target.value || undefined)}
            placeholder="Provider default"
          />
          {reasoningOptions.length > 0 && (
            <datalist id={reasoningOptionsId}>
              {reasoningOptions.map((effort) => (
                <option key={effort} value={effort} />
              ))}
            </datalist>
          )}
          <FieldDescription className="text-xs">
            {currentModel?.capabilities.reasoningEfforts?.length
              ? "Choose a known tier or enter any provider-supported value."
              : "No tiers are known. Enter a provider-supported value, or leave unset for its default."}
          </FieldDescription>
        </Field>
      </div>
      {selected === "manual" && (
        <div className="grid gap-3 sm:grid-cols-3">
          <Field><FieldLabel>Provider id</FieldLabel><Input value={string(model.providerId)} onChange={(e) => update("providerId", e.target.value)} placeholder="openai" /></Field>
          <Field><FieldLabel>API kind</FieldLabel><Input value={pinnedApiKind ?? string(model.apiKind)} disabled={Boolean(pinnedApiKind)} onChange={(e) => update("apiKind", e.target.value)} placeholder="openai:responses" /></Field>
          <Field><FieldLabel>Model</FieldLabel><Input value={string(model.model)} onChange={(e) => update("model", e.target.value)} placeholder="gpt-5.5" /></Field>
        </div>
      )}
    </div>
  );
}

function modelOptionKey(option: Pick<ModelOption, "providerId" | "apiKind" | "model">): string {
  return JSON.stringify([option.providerId, option.apiKind, option.model]);
}

/**
 * Discovery returns one route per API kind, but the primary picker is a model
 * picker. Collapse those route variants while preserving an existing or pinned
 * route; otherwise prefer Responses for a newly selected compatible model.
 */
export function modelPickerOptions(
  models: ModelOption[],
  currentModel?: ModelOption,
  pinnedApiKind?: string,
): ModelOption[] {
  const catalog = new Map<string, ModelOption>();
  const matchesCurrent = (option: ModelOption) =>
    currentModel !== undefined && modelOptionKey(option) === modelOptionKey(currentModel);
  const apiKindPriority = (apiKind: string) =>
    apiKind === "openai:responses" ? 0 : apiKind === "openai:completions" ? 1 : 2;

  for (const option of models) {
    if (pinnedApiKind && option.apiKind !== pinnedApiKind) continue;
    const key = JSON.stringify([option.providerId, option.model]);
    const existing = catalog.get(key);
    if (
      !existing ||
      matchesCurrent(option) ||
      (!matchesCurrent(existing) &&
        apiKindPriority(option.apiKind) < apiKindPriority(existing.apiKind))
    ) {
      catalog.set(key, option);
    }
  }

  return [...catalog.values()].sort(compareModelOptions);
}

/** Prefer the provider's newest models while keeping ordering deterministic. */
export function compareModelOptions(
  left: ModelOption,
  right: ModelOption,
): number {
  const leftCreated = left.createdAtMs ?? null;
  const rightCreated = right.createdAtMs ?? null;
  if (
    leftCreated !== null &&
    rightCreated !== null &&
    leftCreated !== rightCreated
  ) {
    return rightCreated - leftCreated;
  }
  if (leftCreated !== null) return -1;
  if (rightCreated !== null) return 1;
  return (
    left.displayName.localeCompare(right.displayName) ||
    left.providerId.localeCompare(right.providerId) ||
    left.apiKind.localeCompare(right.apiKind)
  );
}

function formatModelCreatedAt(createdAtMs: number): string {
  return new Intl.DateTimeFormat(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  }).format(new Date(createdAtMs));
}

function ModelCapabilities({ option }: { option: ModelOption }) {
  const details = [
    option.createdAtMs ? `created ${formatModelCreatedAt(option.createdAtMs)}` : null,
    option.capabilities.maxInputTokens ? `${option.capabilities.maxInputTokens.toLocaleString()} input tokens` : null,
    option.capabilities.maxOutputTokens ? `${option.capabilities.maxOutputTokens.toLocaleString()} output tokens` : null,
    option.capabilities.parallelToolUse === true ? "parallel tools" : null,
  ].filter(Boolean);
  return (
    <p className="text-xs text-muted-foreground">
      {details.join(" · ")}
    </p>
  );
}

function AdvancedFields({ config, change }: { config: RecordValue; change: (fn: (next: RecordValue) => void) => void }) {
  return (
    <ExpandableSetupPanel
      title="Model run controls"
      description="Optional generation controls, run limits, and context compaction."
      icon={SlidersHorizontal}
    >
      <div className="grid gap-5">
        <GenerationFields config={config} change={change} />
        <LimitsFields config={config} change={change} />
        <ContextFields config={config} change={change} />
      </div>
    </ExpandableSetupPanel>
  );
}

function GenerationFields({ config, change }: { config: RecordValue; change: (fn: (next: RecordValue) => void) => void }) {
  const generation = record(config.generation);
  const supportsProcessingTier = supportsOpenAiProcessingTier({ model: record(config.model) });
  const processingTier = string(generation.processingTier) || "providerDefault";
  const update = (key: string, value: unknown) => change((next) => {
    const item = record(next.generation);
    if (value === undefined || value === "") delete item[key]; else item[key] = value;
    if (Object.keys(item).length) next.generation = item; else delete next.generation;
  });
  const parallel = typeof generation.parallelToolUse === "boolean" ? String(generation.parallelToolUse) : "default";
  const toolChoice = record(generation.toolChoice);
  const toolChoiceType = string(toolChoice.type) || "default";
  return (
    <div className="grid gap-3 border-b pb-5">
      <div><p className="text-sm font-medium">Generation</p><p className="text-xs text-muted-foreground">Defaults applied to every run.</p></div>
      <div className="grid gap-3 sm:grid-cols-2">
          <Field>
            <FieldLabel>Max output tokens</FieldLabel>
            <Input
              type="number"
              min="0"
              value={numberString(generation.maxOutputTokens)}
              onChange={(e) => update("maxOutputTokens", parseNumber(e.target.value))}
              placeholder="Provider default"
            />
          </Field>
          <Field>
            <FieldLabel>Parallel tool use</FieldLabel>
            <Select value={parallel} onValueChange={(value) => update("parallelToolUse", value === "default" ? undefined : value === "true")}>
              <SelectTrigger className="w-full"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="default">Provider default</SelectItem>
                <SelectItem value="true">Allow</SelectItem>
                <SelectItem value="false">Disallow</SelectItem>
              </SelectContent>
            </Select>
          </Field>
          {supportsProcessingTier && (
            <Field>
              <FieldLabel>Processing tier</FieldLabel>
              <Select
                value={processingTier}
                onValueChange={(value) => update(
                  "processingTier",
                  value === "providerDefault" ? undefined : value,
                )}
              >
                <SelectTrigger className="w-full"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="providerDefault">Provider default</SelectItem>
                  <SelectItem value="standard">Standard</SelectItem>
                  <SelectItem value="fast">Fast</SelectItem>
                  <SelectItem value="flex">Flex</SelectItem>
                </SelectContent>
              </Select>
              <FieldDescription className="text-xs">
                Applied to every run. Fast prioritizes latency; Flex uses lower-priority processing.
              </FieldDescription>
            </Field>
          )}
          <Field>
            <FieldLabel>Tool choice</FieldLabel>
            <Select value={toolChoiceType} onValueChange={(value) => update("toolChoice", value === "default" ? undefined : { type: value })}>
              <SelectTrigger className="w-full"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="default">Provider default</SelectItem>
                <SelectItem value="auto">Auto</SelectItem>
                <SelectItem value="none">None</SelectItem>
                <SelectItem value="requiredAny">Require a tool</SelectItem>
                <SelectItem value="specific">Specific tool</SelectItem>
              </SelectContent>
            </Select>
          </Field>
          {toolChoiceType === "specific" && (
            <Field className="sm:col-span-2">
              <FieldLabel>Tool ID</FieldLabel>
              <Input
                value={string(toolChoice.toolId)}
                onChange={(e) => update("toolChoice", { type: "specific", toolId: e.target.value })}
                placeholder="env.run_process"
              />
              <FieldDescription className="text-xs">
                Use the enabled tool's registry ID, such as env.run_process or vfs.read_file.
                Builtin tool names are resolved for the selected model. For custom functions, use the function name.
              </FieldDescription>
            </Field>
          )}
      </div>
    </div>
  );
}

function LimitsFields({ config, change }: { config: RecordValue; change: (fn: (next: RecordValue) => void) => void }) {
  const limits = record(config.limits);
  const update = (key: string, value: string) => change((next) => {
    const item = record(next.limits);
    const number = parseNumber(value);
    if (number === undefined) delete item[key]; else item[key] = number;
    if (Object.keys(item).length) next.limits = item; else delete next.limits;
  });
  return <div className="grid gap-3 border-b pb-5"><div><p className="text-sm font-medium">Run limits</p><p className="text-xs text-muted-foreground">Leave unset to use the engine budget.</p></div><div className="grid gap-3 sm:grid-cols-2"><Field><FieldLabel>Max turns</FieldLabel><Input type="number" min="0" value={numberString(limits.maxTurns)} onChange={(e) => update("maxTurns", e.target.value)} /></Field><Field><FieldLabel>Max tool rounds</FieldLabel><Input type="number" min="0" value={numberString(limits.maxToolRounds)} onChange={(e) => update("maxToolRounds", e.target.value)} /></Field></div></div>;
}

function ContextFields({ config, change }: { config: RecordValue; change: (fn: (next: RecordValue) => void) => void }) {
  const compaction = record(record(config.context).compaction);
  const mode = string(compaction.mode) || "default";
  const update = (key: string, value: unknown) => change((next) => {
    const context = record(next.context);
    const compact = record(context.compaction);
    if (value === undefined || value === "") delete compact[key]; else compact[key] = value;
    if (compact.mode === "default") delete compact.mode;
    if (Object.keys(compact).length) context.compaction = compact; else delete context.compaction;
    if (Object.keys(context).length) next.context = context; else delete next.context;
  });
  return <div className="grid gap-3"><div><p className="text-sm font-medium">Context compaction</p><p className="text-xs text-muted-foreground">Control how long sessions compact prior context.</p></div><div className="grid gap-3 sm:grid-cols-2"><Field><FieldLabel>Mode</FieldLabel><Select value={mode} onValueChange={(value) => update("mode", value === "default" ? undefined : value)}><SelectTrigger className="w-full"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="default">Engine default</SelectItem><SelectItem value="disabled">Disabled</SelectItem><SelectItem value="providerTriggered">Provider triggered</SelectItem><SelectItem value="providerStandalone">Provider standalone</SelectItem></SelectContent></Select></Field>{mode === "providerTriggered" || mode === "providerStandalone" ? <Field><FieldLabel>Compact threshold tokens</FieldLabel><Input type="number" min="0" value={numberString(compaction.compactThresholdTokens)} onChange={(e) => update("compactThresholdTokens", parseNumber(e.target.value))} /></Field> : null}{mode === "providerStandalone" ? <Field><FieldLabel>Target tokens</FieldLabel><Input type="number" min="0" value={numberString(compaction.targetTokens)} onChange={(e) => update("targetTokens", parseNumber(e.target.value))} /></Field> : null}</div></div>;
}

function FeaturePanel({
  name,
  enabled,
  feature,
  disableReason,
  expandable,
  onEnabledChange,
  children,
}: {
  name: FeatureName;
  enabled: boolean;
  feature: RecordValue;
  disableReason?: string;
  expandable?: boolean;
  onEnabledChange: (enabled: boolean) => void;
  children?: React.ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const info = featureInfo[name];
  const Icon = info.icon;
  const configurable = expandable ?? Boolean(children);
  useEffect(() => {
    if (!enabled) setOpen(false);
  }, [enabled]);
  return (
    <div className={cn("min-w-0 max-w-full rounded-lg border", enabled ? "border-border" : "border-dashed")}>
      <div className="flex min-w-0 min-h-16 items-stretch gap-3 px-4">
        <div className="flex items-center">
          <Switch
            checked={enabled}
            disabled={enabled && Boolean(disableReason)}
            onCheckedChange={(checked) => {
              const nextEnabled = checked === true;
              setOpen(nextEnabled && configurable);
              onEnabledChange(nextEnabled);
            }}
            aria-label={`Enable ${info.title}`}
          />
        </div>
        <button
          type="button"
          disabled={!enabled || !configurable}
          aria-expanded={enabled && configurable ? open : undefined}
          className={cn(
            "-mr-2 flex min-w-0 flex-1 items-center gap-3 rounded-md px-2 py-3 text-left outline-none focus-visible:ring-2 focus-visible:ring-ring",
            enabled && configurable && "cursor-pointer",
          )}
          onClick={() => setOpen((value) => !value)}
        >
          <Icon className="size-4 shrink-0 text-muted-foreground" />
          <div className="min-w-0 flex-1">
            <p className="text-sm font-medium">{info.title}</p>
            <p className="text-xs text-muted-foreground">{info.description}</p>
            {enabled && disableReason && (
              <p className="mt-1 text-xs text-amber-700 dark:text-amber-400">
                {disableReason}
              </p>
            )}
          </div>
          {enabled && configurable && (
            <span className="flex size-8 shrink-0 items-center justify-center" aria-hidden="true">
              {open ? <ChevronDown /> : <ChevronRight />}
            </span>
          )}
        </button>
      </div>
      {enabled && configurable && open && <div className="min-w-0 border-t px-4 py-4">{children}</div>}
    </div>
  );
}

function VfsFields({
  feature,
  environmentsGranted,
  workspaces,
  workspacesLoading,
  patch,
}: {
  feature: RecordValue;
  environmentsGranted: boolean;
  workspaces: WorkspaceOption[];
  workspacesLoading: boolean;
  patch: (fn: (feature: RecordValue) => void) => void;
}) {
  const attachments = Array.isArray(feature.workspaces)
    ? feature.workspaces.map(record)
    : [];
  const workspaceOptions = new Map<string, WorkspaceOption>();
  for (const workspace of workspaces) workspaceOptions.set(workspace.workspaceId, workspace);
  for (const attachment of attachments) {
    const workspaceId = string(attachment.workspaceId);
    if ("workspaceId" in attachment && workspaceId && !workspaceOptions.has(workspaceId)) {
      workspaceOptions.set(workspaceId, { workspaceId });
    }
  }
  const options = [...workspaceOptions.values()];
  const updateAttachments = (nextAttachments: RecordValue[]) =>
    patch((next) => {
      if (nextAttachments.length) next.workspaces = nextAttachments;
      else delete next.workspaces;
    });
  const updateAttachment = (index: number, mutate: (attachment: RecordValue) => void) =>
    updateAttachments(
      attachments.map((attachment, attachmentIndex) => {
        if (attachmentIndex !== index) return attachment;
        const next = { ...attachment };
        mutate(next);
        return next;
      }),
    );
  const nextPath = nextWorkspaceAttachmentPath(attachments);

  return (
    <div className="grid gap-5">
      <div className="grid gap-3">
        <div className="flex min-w-0 items-start justify-between gap-3">
          <div className="min-w-0">
            <p className="text-sm font-medium">Workspace attachments</p>
            <p className="text-xs text-muted-foreground">
              Attach workspaces at session paths with their own access grants.
            </p>
          </div>
          <Button
            variant="outline"
            size="xs"
            onClick={() => updateAttachments([
              ...attachments,
              {
                path: nextPath,
                access: "edit",
                workspaceId: workspaces[0]?.workspaceId ?? "",
              },
            ])}
          >
            <Plus data-icon="inline-start" />
            Add workspace
          </Button>
        </div>
        {attachments.length === 0 && (
          <p className="text-xs text-muted-foreground">No workspace attachments.</p>
        )}
        {attachments.map((attachment, index) => {
          const snapshot = "snapshotRef" in attachment;
          return (
            <div
              key={index}
              className="grid gap-3 border-t pt-3 sm:grid-cols-[minmax(0,1fr)_auto]"
            >
              <div className="grid gap-3 sm:grid-cols-3">
                {snapshot ? (
                  <Field>
                    <FieldLabel>Snapshot ref</FieldLabel>
                    <Input
                      className="font-mono"
                      value={string(attachment.snapshotRef)}
                      onChange={(event) => updateAttachment(index, (next) => {
                        next.snapshotRef = event.target.value;
                      })}
                    />
                  </Field>
                ) : (
                  <Field>
                    <FieldLabel>Workspace</FieldLabel>
                    {options.length || workspacesLoading ? (
                      <Select
                        value={string(attachment.workspaceId)}
                        disabled={workspacesLoading}
                        onValueChange={(workspaceId) => updateAttachment(index, (next) => {
                          next.workspaceId = workspaceId;
                        })}
                      >
                        <SelectTrigger className="w-full">
                          <SelectValue placeholder={workspacesLoading ? "Loading…" : "Select workspace"} />
                        </SelectTrigger>
                        <SelectContent>
                          {options.map((workspace) => (
                            <SelectItem key={workspace.workspaceId} value={workspace.workspaceId}>
                              {workspace.displayName
                                ? `${workspace.displayName} (${workspace.workspaceId})`
                                : workspaces.some((item) => item.workspaceId === workspace.workspaceId)
                                  ? workspace.workspaceId
                                  : `${workspace.workspaceId} (unavailable)`}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    ) : (
                      <Input
                        className="font-mono"
                        value={string(attachment.workspaceId)}
                        onChange={(event) => updateAttachment(index, (next) => {
                          next.workspaceId = event.target.value;
                        })}
                        placeholder="workspace id"
                      />
                    )}
                  </Field>
                )}
                <Field>
                  <FieldLabel>Session path</FieldLabel>
                  <Input
                    className="font-mono"
                    value={string(attachment.path)}
                    onChange={(event) => updateAttachment(index, (next) => {
                      next.path = event.target.value;
                    })}
                  />
                </Field>
                <Field>
                  <FieldLabel>Access</FieldLabel>
                  <Select
                    value={string(attachment.access) || "edit"}
                    disabled={snapshot}
                    onValueChange={(access) => updateAttachment(index, (next) => {
                      next.access = access;
                    })}
                  >
                    <SelectTrigger className="w-full"><SelectValue /></SelectTrigger>
                    <SelectContent>
                      <SelectItem value="edit">Read and write</SelectItem>
                      <SelectItem value="read">Read only</SelectItem>
                    </SelectContent>
                  </Select>
                </Field>
              </div>
              <Button
                variant="ghost"
                size="icon-sm"
                className="self-start text-destructive"
                aria-label="Remove workspace attachment"
                onClick={() => updateAttachments(attachments.filter((_, attachmentIndex) => attachmentIndex !== index))}
              >
                <Trash2 />
              </Button>
            </div>
          );
        })}
      </div>
      <p className="text-xs text-muted-foreground">File tools follow each workspace’s access. {environmentsGranted
        ? "Materialize requires environment edit access. Capture requires workspace edit access and environment read access."
        : "Enable Environments to also transfer files between attached workspaces and an active environment."}</p>

      <SourceDiscoveryFields source="vfs-prompts" feature={feature} patch={patch} />
      <SourceDiscoveryFields source="vfs" feature={feature} patch={patch} />

      <WorkingDirectoryField feature={feature} patch={patch} />
    </div>
  );
}

function nextWorkspaceAttachmentPath(attachments: RecordValue[]): string {
  const paths = new Set(attachments.map((attachment) => string(attachment.path)));
  if (!paths.has("/workspace")) return "/workspace";
  let suffix = 2;
  while (paths.has(`/workspace-${suffix}`)) suffix += 1;
  return `/workspace-${suffix}`;
}

function WebFields({
  feature,
  apiKind,
  patch,
}: {
  feature: RecordValue;
  apiKind: string;
  patch: (fn: (feature: RecordValue) => void) => void;
}) {
  const id = useId();
  const [open, setOpen] = useState(false);
  const search = record(feature.search);
  const allowedCount = stringList(search.allowedDomains).length;
  const blockedCount = stringList(search.blockedDomains).length;
  const fetchEnabled = "fetch" in feature;
  const searchEnabled = "search" in feature;
  useEffect(() => { if (!searchEnabled) setOpen(false); }, [searchEnabled]);
  const exclusiveDomainFilters = apiKind === "anthropic:messages";
  const setSubfeature = (name: "fetch" | "search", enabled: boolean) => patch((next) => {
    if (enabled) next[name] = {};
    else {
      delete next[name];
      if (!("fetch" in next) && !("search" in next)) next.search = {};
    }
  });
  return (
    <div className="grid gap-4">
      <div className="flex flex-wrap gap-x-6 gap-y-3">
        <Label className="gap-2 font-normal"><Checkbox checked={fetchEnabled} onCheckedChange={(checked) => setSubfeature("fetch", checked === true)} />Fetch pages</Label>
        <Label className="gap-2 font-normal"><Checkbox checked={searchEnabled} onCheckedChange={(checked) => setSubfeature("search", checked === true)} />Search the web</Label>
      </div>
      {searchEnabled && (
        <button
          type="button"
          aria-label="Customize search domains"
          aria-expanded={open}
          aria-controls={`${id}-domains`}
          onClick={() => setOpen((value) => !value)}
          className="w-fit cursor-pointer rounded-sm text-left text-xs text-muted-foreground underline-offset-4 outline-none hover:text-foreground hover:underline focus-visible:ring-2 focus-visible:ring-ring"
        >
          {(allowedCount > 0 || blockedCount > 0) && <>{[allowedCount > 0 && `${allowedCount} allowed`, blockedCount > 0 && `${blockedCount} blocked`].filter(Boolean).join(", ")} · </>}
          {open ? "Hide domains" : "Customize domains"}
        </button>
      )}
      {searchEnabled && open && (
        <div id={`${id}-domains`} className="grid gap-3 sm:grid-cols-2">
          <Field>
            <FieldLabel htmlFor={`${id}-allowed`}>Allowed domains</FieldLabel>
            <Input
              id={`${id}-allowed`}
              value={commaList(search.allowedDomains)}
              onChange={(e) => patch((next) => {
                const domains = listFromInput(e.target.value);
                const item = record(next.search);
                if (domains.length) item.allowedDomains = domains;
                else delete item.allowedDomains;
                if (exclusiveDomainFilters && domains.length) delete item.blockedDomains;
                next.search = item;
              })}
              placeholder="All domains"
            />
            <FieldDescription className="text-xs">Comma-separated domains. Empty allows all.</FieldDescription>
          </Field>
          <Field>
            <FieldLabel htmlFor={`${id}-blocked`}>Blocked domains</FieldLabel>
            <Input
              id={`${id}-blocked`}
              value={commaList(search.blockedDomains)}
              onChange={(e) => patch((next) => {
                const domains = listFromInput(e.target.value);
                const item = record(next.search);
                if (domains.length) item.blockedDomains = domains;
                else delete item.blockedDomains;
                if (exclusiveDomainFilters && domains.length) delete item.allowedDomains;
                next.search = item;
              })}
              placeholder="None"
            />
            <FieldDescription className="text-xs">Comma-separated domains. Empty blocks none.</FieldDescription>
          </Field>
        </div>
      )}
      {searchEnabled && open && exclusiveDomainFilters && (
        <FieldDescription className="text-xs">
          Anthropic accepts either an allowed-domain list or a blocked-domain list, not both.
        </FieldDescription>
      )}
    </div>
  );
}

function SubagentFields({
  feature,
  profiles,
  patch,
}: {
  feature: RecordValue;
  profiles: ProfileOption[];
  patch: (fn: (feature: RecordValue) => void) => void;
}) {
  const id = useId();
  const [open, setOpen] = useState(false);
  const agents = subagentProfileIds(feature.agents);
  const limits = [
    { key: "maxDepth", label: "Max depth", placeholder: "2", hint: "How deep sub-agents may nest below this session." },
    { key: "maxDescendants", label: "Max descendants", placeholder: "16", hint: "Lifetime total of sub-agent sessions under the root." },
    { key: "maxConcurrent", label: "Max concurrent", placeholder: "4", hint: "Open sub-agent sessions under the root at once." },
    { key: "deadlineMs", label: "Deadline (ms)", placeholder: "3600000", hint: "Per-child run deadline; at most 24 hours." },
  ] as const;
  const customLimits = limits.filter((limit) => feature[limit.key] != null).length;
  return (
    <div className="grid gap-4 sm:grid-cols-2">
      <Field className="sm:col-span-2">
        <FieldLabel>Agents</FieldLabel>
        <ProfileMultiSelect
          value={agents}
          profiles={profiles}
          onChange={(values) => patch((next) => {
            next.agents = values;
          })}
          placeholder="Pick the profiles the agent may run"
        />
        <FieldDescription className="text-xs">
          The agent menu. Every listed profile must exist; the model sees ids and descriptions in its sub-agent catalog.
        </FieldDescription>
      </Field>
      <button
        type="button"
        aria-label="Customize sub-agent limits"
        aria-expanded={open}
        aria-controls={`${id}-limits`}
        onClick={() => setOpen((value) => !value)}
        className="w-fit cursor-pointer rounded-sm text-left text-xs text-muted-foreground underline-offset-4 outline-none hover:text-foreground hover:underline focus-visible:ring-2 focus-visible:ring-ring sm:col-span-2"
      >
        {customLimits > 0 && <>{customLimits} custom {customLimits === 1 ? "limit" : "limits"} · </>}
        {open ? "Hide limits" : "Customize limits"}
      </button>
      {open && <div id={`${id}-limits`} className="grid gap-4 sm:col-span-2 sm:grid-cols-2">
        {limits.map((limit) => (
          <Field key={limit.key}>
            <FieldLabel htmlFor={`${id}-${limit.key}`}>{limit.label}</FieldLabel>
            <Input
              id={`${id}-${limit.key}`}
              type="number"
              min="1"
              placeholder={limit.placeholder}
              value={numberString(feature[limit.key])}
              onChange={(e) => patch((next) => {
                const value = parseNumber(e.target.value);
                if (value === undefined) delete next[limit.key];
                else next[limit.key] = value;
              })}
            />
            <FieldDescription className="text-xs">{limit.hint}</FieldDescription>
          </Field>
        ))}
      </div>}
    </div>
  );
}

function ProfileMultiSelect({
  value,
  profiles,
  onChange,
  placeholder,
}: {
  value: string[];
  profiles: ProfileOption[];
  onChange: (value: string[]) => void;
  placeholder: string;
}) {
  const profileMap = new Map(profiles.map((profile) => [profile.profileId, profile]));
  const items = [
    ...new Set([...profiles.map((profile) => profile.profileId), ...value]),
  ].sort((left, right) => profileLabel(profileMap.get(left), left).localeCompare(profileLabel(profileMap.get(right), right)));
  return (
    <Combobox
      items={items}
      multiple
      value={value}
      onValueChange={onChange}
      itemToStringLabel={(profileId) => profileLabel(profileMap.get(profileId), profileId)}
      filter={(profileId, query) => {
        const profile = profileMap.get(profileId);
        const search = `${profileLabel(profile, profileId)} ${profileId}`.toLocaleLowerCase();
        return search.includes(query.toLocaleLowerCase());
      }}
    >
      <ComboboxChips>
        <ComboboxValue>
          {value.map((profileId) => (
            <ComboboxChip key={profileId}>{profileLabel(profileMap.get(profileId), profileId)}</ComboboxChip>
          ))}
        </ComboboxValue>
        <ComboboxChipsInput placeholder={value.length ? "Add profile" : placeholder} />
      </ComboboxChips>
      <ComboboxContent>
        <ComboboxEmpty>No matching profiles.</ComboboxEmpty>
        <ComboboxList>
          {(profileId: string) => (
            <ComboboxItem key={profileId} value={profileId}>
              <span className="min-w-0">
                <span className="block truncate">{profileLabel(profileMap.get(profileId), profileId)}</span>
                {profileMap.get(profileId)?.displayName && (
                  <span className="block truncate font-mono text-xs text-muted-foreground">{profileId}</span>
                )}
              </span>
            </ComboboxItem>
          )}
        </ComboboxList>
      </ComboboxContent>
    </Combobox>
  );
}

function profileLabel(profile: ProfileOption | undefined, profileId: string): string {
  return profile?.displayName || profileId;
}

function WorkingDirectoryField({ environment, feature, patch }: {
  environment?: boolean;
  feature: RecordValue;
  patch: (fn: (feature: RecordValue) => void) => void;
}) {
  const id = useId();
  const [open, setOpen] = useState(false);
  const directory = string(feature.workingDirectory);
  return (
    <div className="grid min-w-0 gap-3">
      {environment && (
        <button
          type="button"
          aria-label="Configure Environment working directory"
          aria-expanded={open}
          aria-controls={`${id}-settings`}
          onClick={() => setOpen((value) => !value)}
          className="flex min-w-0 max-w-full w-fit items-center gap-1 cursor-pointer rounded-sm text-left text-xs text-muted-foreground underline-offset-4 outline-none hover:text-foreground hover:underline focus-visible:ring-2 focus-visible:ring-ring"
        >
          {directory && <><span className="truncate font-mono">{directory}</span><span>·</span></>}
          <span className="shrink-0">{open ? "Hide working directory" : directory ? "Edit working directory" : "Customize working directory"}</span>
        </button>
      )}
      {(!environment || open) && (
        <Field id={`${id}-settings`}>
          <FieldLabel htmlFor={id}>Working directory</FieldLabel>
          <Input id={id} aria-label={`${environment ? "Environment" : "VFS"} working directory`}
            className="font-mono" value={directory} placeholder={environment ? "Environment default" : "/"}
            onChange={(event) => patch((next) => {
              if (event.target.value) next.workingDirectory = event.target.value;
              else delete next.workingDirectory;
            })} />
          <FieldDescription className="text-xs">{environment
            ? "Absolute machine directory for file tools, commands, jobs, and discovery. Empty uses the environment default."
            : "Absolute attached VFS directory for relative file paths. Empty uses /. Source discovery still searches every workspace attachment."}</FieldDescription>
        </Field>
      )}
    </div>
  );
}

function SourceDiscoveryFields({ source, feature, patch }: {
  source: "vfs" | "vfs-prompts" | "environment" | "environment-prompts";
  feature: RecordValue;
  patch: (fn: (feature: RecordValue) => void) => void;
}) {
  const id = useId();
  const environment = source.startsWith("environment");
  const prompts = source.endsWith("prompts");
  const configKey = prompts ? "prompts" : "skills";
  const enabled = feature[configKey] != null;
  const settings = record(feature[configKey]);
  const roots = stringList(settings.roots);
  const [open, setOpen] = useState(false);
  useEffect(() => { if (!enabled) setOpen(false); }, [enabled]);
  const domain = environment ? "Environment" : "VFS";
  const switchLabel = `${domain} ${prompts ? "prompt loading" : "skill discovery"}`;
  const update = (key: string, value: unknown) => patch((next) => {
    const settings = { ...record(next[configKey]) };
    if (value === undefined) delete settings[key]; else settings[key] = value;
    next[configKey] = settings;
  });
  return (
    <div className="grid min-w-0 max-w-full gap-3 rounded-lg border p-3">
      <div className="flex items-start justify-between gap-4">
        <div className="grid min-w-0 gap-1">
          <Label htmlFor={id}>{prompts ? "Prompt loading" : "Skill discovery"}</Label>
          <p className="text-xs text-muted-foreground">
            {prompts ? "Load .md and .txt files as instructions." : "Let the agent discover and read available skills."}
          </p>
          {enabled && (
            <button
              type="button"
              aria-label={`Configure ${switchLabel}`}
              aria-expanded={open}
              aria-controls={`${id}-settings`}
              onClick={() => setOpen((value) => !value)}
              className="w-fit cursor-pointer rounded-sm text-left text-xs text-muted-foreground underline-offset-4 outline-none hover:text-foreground hover:underline focus-visible:ring-2 focus-visible:ring-ring"
            >
              {roots.length > 0 && <>{roots.length} custom {roots.length === 1 ? "root" : "roots"} · </>}
              {open ? "Hide root settings" : roots.length ? "Edit roots" : "Customize roots"}
            </button>
          )}
        </div>
        <Switch id={id} aria-label={switchLabel}
          checked={enabled}
          onCheckedChange={(checked) => patch((next) => {
            if (checked) next[configKey] = {};
            else delete next[configKey];
          })}
        />
      </div>
      {enabled && open && (
        <div id={`${id}-settings`} className="grid gap-3 border-t pt-3">
          <p className="text-xs text-muted-foreground">
            {prompts ? `Load direct .md and .txt files from ${domain} prompt roots in alphabetical order. Number prefixes are optional; subfolders are ignored.` : `Advertise ${domain} skills for the agent to read when relevant.`}
          </p>
          <Field>
            <FieldLabel htmlFor={`${id}-roots`}>{domain} {prompts ? "prompt" : "skill"} roots</FieldLabel>
            <Input id={`${id}-roots`} className="font-mono" value={commaList(settings.roots)}
              placeholder="Default directories"
              onChange={(e) => {
                const roots = listFromInput(e.target.value);
                update("roots", roots.length ? roots : undefined);
              }} />
            <FieldDescription className="text-xs">
              {`Empty searches .agents/${configKey} and .lightspeed/${configKey} ${environment ? "under the working directory and execution user’s home" : "beneath each workspace attachment"}. `}
              {environment
                ? "Comma-separated overrides replace all defaults, including home. Paths may be absolute or relative to the working directory."
                : "Comma-separated overrides replace all defaults and must be absolute paths inside workspace attachments."}
            </FieldDescription>
          </Field>
        </div>
      )}
    </div>
  );
}

function EnvironmentFields({
  feature,
  environments,
  allowInherit,
  patch,
}: {
  feature: RecordValue;
  environments: EnvironmentOption[];
  allowInherit: boolean;
  patch: (fn: (feature: RecordValue) => void) => void;
}) {
  const selectionId = useId();
  const attachments = Array.isArray(feature.environments) ? feature.environments.map(record) : [];
  const update = (index: number, mutate: (attachment: RecordValue) => void) =>
    patch((next) => {
      next.environments = attachments.map((attachment, position) => {
        const value = { ...attachment };
        if (position === index) mutate(value);
        return value;
      });
    });
  return (
    <div className="grid gap-5">
      <div className="flex items-start justify-between gap-3">
        <div>
          <p className="text-sm font-medium">Environment attachments</p>
          <p className="text-xs text-muted-foreground">
            Access applies to the active environment. A default fills an empty selection when a
            profile is applied.
          </p>
        </div>
        <Button
          variant="outline"
          size="xs"
          onClick={() =>
            patch((next) => {
              const environment = selectableEnvironments(environments).find(
                (candidate) =>
                  !attachments.some((item) => item.environmentId === candidate.environmentId),
              );
              next.environments = [
                ...attachments,
                {
                  environmentId: environment?.environmentId ?? "",
                  access: "jobs",
                  ...(attachments.length === 0 ? { default: true } : {}),
                },
              ];
              if (attachments.length + 1 >= 2) next.selection = true;
            })
          }
        >
          <Plus data-icon="inline-start" />
          Add environment
        </Button>
      </div>
      {!attachments.length && (
        <p className="text-xs text-muted-foreground">No environments attached.</p>
      )}
      {attachments.map((attachment, index) => {
        const id = string(attachment.environmentId);
        const inherited = attachment.inherit === true;
        const options = selectableEnvironments(environments, id).filter(
          (candidate) =>
            candidate.environmentId === id ||
            !attachments.some((item) => item.environmentId === candidate.environmentId),
        );
        if (id && !options.some((candidate) => candidate.environmentId === id))
          options.push({ environmentId: id, status: "unavailable" });
        return (
          <div key={index} className="grid gap-3 border-t pt-3">
            <div className="grid gap-3 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto]">
              <Field>
                <FieldLabel>Environment</FieldLabel>
                <Select
                  value={inherited ? "__inherit__" : id}
                  onValueChange={(value) =>
                    update(index, (next) => {
                      if (value === "__inherit__") {
                        next.inherit = true;
                        delete next.environmentId;
                      } else {
                        next.environmentId = value;
                        delete next.inherit;
                      }
                    })
                  }
                >
                  <SelectTrigger className="w-full" aria-label={`Environment ${index + 1}`}>
                    <SelectValue placeholder="Select environment" />
                  </SelectTrigger>
                  <SelectContent>
                    {allowInherit && (
                      <SelectItem
                        value="__inherit__"
                        disabled={!inherited && attachments.some((item) => item.inherit === true)}
                      >
                        Inherit parent environment
                      </SelectItem>
                    )}
                    {options.map((environment) => (
                      <SelectItem key={environment.environmentId} value={environment.environmentId}>
                        {environment.displayName
                          ? `${environment.displayName} (${environment.environmentId})`
                          : environment.environmentId}
                        {environment.status && environment.status !== "ready"
                          ? ` (${environment.status})`
                          : ""}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                {inherited && (
                  <FieldDescription className="text-xs">
                    Uses the parent’s active environment captured when the sub-agent starts.
                  </FieldDescription>
                )}
              </Field>
              <Field>
                <FieldLabel>Access</FieldLabel>
                <Select
                  value={string(attachment.access)}
                  onValueChange={(access) =>
                    update(index, (next) => {
                      next.access = access;
                    })
                  }
                >
                  <SelectTrigger className="w-full" aria-label={`Environment ${index + 1} access`}>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="read">Read files</SelectItem>
                    <SelectItem value="edit">Edit files</SelectItem>
                    <SelectItem value="exec">Run commands</SelectItem>
                    <SelectItem value="jobs">Run durable jobs</SelectItem>
                  </SelectContent>
                </Select>
                <FieldDescription className="text-xs">
                  {environmentAccessDescriptions[string(attachment.access)]}
                </FieldDescription>
              </Field>
              <Button
                variant="ghost"
                size="icon-sm"
                className="self-start text-destructive"
                aria-label="Remove environment attachment"
                onClick={() =>
                  patch((next) => {
                    const remaining = attachments.filter((_, position) => position !== index);
                    if (attachment.default === true && remaining.length) {
                      const defaultIndex = Math.min(index, remaining.length - 1);
                      remaining[defaultIndex] = { ...remaining[defaultIndex], default: true };
                    }
                    next.environments = remaining;
                  })
                }
              >
                <Trash2 />
              </Button>
            </div>
            <div className="flex items-center gap-2">
              <Checkbox
                aria-label={`Default environment ${index + 1}`}
                checked={attachment.default === true}
                onCheckedChange={(checked) =>
                  patch((next) => {
                    next.environments = attachments.map((item, position) => {
                      const value = { ...item };
                      if (position === index && checked === true) value.default = true;
                      else if (position === index || checked === true) delete value.default;
                      return value;
                    });
                  })
                }
              />
              <span className="text-sm">Default environment</span>
            </div>
            <WorkingDirectoryField
              environment
              feature={attachment}
              patch={(mutate) => update(index, mutate)}
            />
          </div>
        );
      })}
      <SourceDiscoveryFields source="environment-prompts" feature={feature} patch={patch} />
      <SourceDiscoveryFields source="environment" feature={feature} patch={patch} />
      <div className="flex items-start justify-between gap-4 rounded-lg border p-3">
        <div className="grid gap-1">
          <Label htmlFor={selectionId}>Environment selection tools</Label>
          <p className="text-xs text-muted-foreground">
            Let the agent list, activate, and deactivate attached environments.
          </p>
        </div>
        <Switch
          id={selectionId}
          checked={feature.selection === true}
          onCheckedChange={(checked) =>
            patch((next) => {
              if (checked) next.selection = true;
              else delete next.selection;
            })
          }
        />
      </div>
    </div>
  );
}

function McpFields({
  feature,
  servers,
  discoverySource,
  patch,
}: {
  feature: RecordValue;
  servers: McpServerOption[];
  discoverySource?: McpToolDiscoverySource;
  patch: (fn: (feature: RecordValue) => void) => void;
}) {
  const attachments = Array.isArray(feature.servers) ? feature.servers.map(record) : [];
  const options = new Map<string, McpServerOption>();
  for (const server of servers) options.set(server.serverId, server);
  for (const attachment of attachments) {
    const serverId = string(attachment.serverId);
    if (serverId && !options.has(serverId)) options.set(serverId, { serverId });
  }
  const serverOptions = [...options.values()];
  const firstServerId = firstUsableMcpServerId(serverOptions);
  const updateAttachments = (nextAttachments: RecordValue[]) =>
    patch((next) => {
      next.servers = nextAttachments;
    });
  const updateAttachment = (index: number, mutate: (attachment: RecordValue) => void) =>
    updateAttachments(
      attachments.map((attachment, attachmentIndex) => {
        if (attachmentIndex !== index) return attachment;
        const next = { ...attachment };
        mutate(next);
        return next;
      }),
    );

  return (
    <div className="grid gap-3">
      <div className="flex min-w-0 items-center justify-between gap-3">
        <p className="min-w-0 text-xs text-muted-foreground">
          Attach servers to make their tools available.
        </p>
        <Button
          variant="outline"
          size="xs"
          onClick={() => updateAttachments([...attachments, { serverId: firstServerId }])}
        >
          <Plus data-icon="inline-start" />
          Add server
        </Button>
      </div>
      {!attachments.length && <p className="text-xs text-muted-foreground">No server attachments.</p>}
      {attachments.map((attachment, index) => (
        <div
          key={index}
          className="grid gap-3 border-t pt-3 sm:grid-cols-[minmax(0,1fr)_auto]"
        >
          <div>
            <Field>
              <FieldLabel>Server</FieldLabel>
              {serverOptions.length ? (
                <Select
                  value={string(attachment.serverId)}
                  onValueChange={(value) => updateAttachment(index, (next) => { next.serverId = value as string; delete next.tools; })}
                >
                  <SelectTrigger className="w-full"><SelectValue /></SelectTrigger>
                  <SelectContent>
                    {serverOptions.map((server) => (
                      <SelectItem
                        key={server.serverId}
                        value={server.serverId}
                        disabled={server.status !== undefined && server.status !== "active"}
                      >
                        {mcpServerOptionLabel(server)}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              ) : (
                <Input
                  className="font-mono"
                  value={string(attachment.serverId)}
                  onChange={(e) => updateAttachment(index, (next) => { next.serverId = e.target.value; delete next.tools; })}
                />
              )}
            </Field>
            <div className="mt-3">
              <McpToolPicker
                scope="session"
                serverId={string(attachment.serverId)}
                revision={options.get(string(attachment.serverId))?.revision}
                allowedTools={options.get(string(attachment.serverId))?.allowedTools}
                source={discoverySource}
                discoveryDisabledReason={!discoverySource ? "Live tool discovery requires permission to configure resources." : undefined}
                value={Array.isArray(attachment.tools) ? stringList(attachment.tools) : undefined}
                onChange={(tools) => updateAttachment(index, (next) => {
                  if (tools === undefined) delete next.tools;
                  else next.tools = tools;
                })}
              />
            </div>
          </div>
          <Button
            variant="ghost"
            size="icon-sm"
            className="self-start text-destructive"
            aria-label="Remove MCP server"
            onClick={() => updateAttachments(attachments.filter((_, attachmentIndex) => attachmentIndex !== index))}
          >
            <Trash2 />
          </Button>
        </div>
      ))}
    </div>
  );
}

function firstUsableMcpServerId(servers: McpServerOption[]): string {
  return servers.find((server) => server.status === undefined || server.status === "active")
    ?.serverId ?? "";
}

function mcpServerOptionLabel(server: McpServerOption): string {
  const name = server.displayName
    ? `${server.displayName} (${server.serverId})`
    : server.serverId;
  return server.status && server.status !== "active" ? `${name} — ${server.status}` : name;
}
