import { useEffect, useId, useState } from "react";
import type { WorkspaceLinkDraft } from "@/api";
import {
  ChevronDown,
  ChevronRight,
  Clock3,
  FolderOpen,
  Globe2,
  Network,
  Plus,
  Server,
  SlidersHorizontal,
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
import { cn } from "@/lib/utils";

export type SessionConfig = Record<string, unknown>;
type FeatureName = "vfs" | "web" | "fleet" | "timers" | "environments" | "mcp";

export type McpServerOption = {
  serverId: string;
  displayName?: string | null;
  status?: "active" | "needsAuthConfig" | "unverified" | "disabled";
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

export type EnvironmentProviderOption = {
  providerId: string;
  displayName?: string | null;
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
  environmentProviders?: EnvironmentProviderOption[];
  featureDisableReasons?: Partial<Record<FeatureName, string>>;
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
    description: "Grant access to workspace-linked files and source instructions or skills from them.",
    icon: FolderOpen,
  },
  web: {
    title: "Web",
    description: "Grant independent web search and fetch capabilities.",
    icon: Globe2,
  },
  fleet: {
    title: "Fleet",
    description: "Allow the agent to inspect and coordinate subagents and profiles.",
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
      if (["readOnly", "edit"].includes(string(feature.tools))) next.tools = feature.tools;
      if (Array.isArray(feature.workspaceLinks) && feature.workspaceLinks.length) {
        next.workspaceLinks = feature.workspaceLinks.map((item) => {
          const link = record(item);
          const target = record(link.target);
          const normalizedTarget: RecordValue = { type: string(target.type) || "workspace" };
          if (normalizedTarget.type === "snapshot") {
            normalizedTarget.snapshotRef = string(target.snapshotRef).trim();
          } else {
            normalizedTarget.type = "workspace";
            normalizedTarget.workspaceId = string(target.workspaceId).trim();
          }
          return {
            path: string(link.path).trim(),
            access: ["readOnly", "readWrite"].includes(string(link.access))
              ? link.access
              : "readWrite",
            target: normalizedTarget,
          };
        });
      }
      for (const key of ["prompts", "skills"] as const) {
        const roots = stringList(record(feature[key]).roots).filter(Boolean);
        if (roots.length) next[key] = { roots };
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
    if (name === "fleet") {
      const profiles = record(feature.profiles);
      const profilesResult: RecordValue = {};
      for (const key of ["allow", "deny"] as const) {
        const values = stringList(profiles[key]).filter(Boolean);
        if (values.length) profilesResult[key] = values;
      }
      if (profiles.inline === false) profilesResult.inline = false;
      if (omitEmptyRecord(profilesResult)) next.profiles = profilesResult;
      const bases = stringList(record(feature.spawn).bases).filter((item) =>
        ["self", "session", "profile"].includes(item),
      );
      if (bases.length) next.spawn = { bases };
    }
    if (name === "environments") {
      const providers = stringList(feature.providers).filter(Boolean);
      if (providers.length) next.providers = providers;
      if (feature.selectionTools === true) next.selectionTools = true;
      if (feature.jobs === true) next.jobs = true;
    }
    if (name === "mcp") {
      const servers = Array.isArray(feature.servers)
        ? feature.servers.map((item) => {
              const server = record(item);
              const serverId = string(server.serverId).trim();
              if (!serverId) return { serverId: "" };
              const link: RecordValue = { serverId };
              const tools = stringList(server.allowedTools).filter(Boolean);
              if (tools.length) link.allowedTools = tools;
              if (["always", "never"].includes(string(server.approval))) link.approval = server.approval;
              if (typeof server.deferLoading === "boolean") link.deferLoading = server.deferLoading;
              return link;
            })
        : [];
      // Keep an incomplete row while it is being edited. Validation prevents
      // saving it, and a completed row is normalized to the thin document.
      if (!servers.length) servers.push({ serverId: "" });
      next.servers = servers;
    }

    // Versions are supplied by admission; leaving the current version out
    // keeps authored documents forward-compatible and minimal.
    features[name] = next;
  }
  if (omitEmptyRecord(features)) result.features = features;

  return omitEmptyRecord(result);
}

function configError(config: SessionConfig | undefined): string | null {
  if (!config) return null;
  const model = record(config.model);
  if (Object.keys(model).length && !["providerId", "apiKind", "model"].every((key) => string(model[key]))) {
    return "A model override needs provider id, API kind, and model name.";
  }
  const toolChoice = record(record(config.generation).toolChoice);
  if (toolChoice.type === "specific" && !string(toolChoice.toolId)) {
    return "A specific tool choice needs a tool id.";
  }
  const mcp = record(record(config.features).mcp);
  if ("mcp" in record(config.features) && (!Array.isArray(mcp.servers) || mcp.servers.some((server) => !string(record(server).serverId)))) {
    return "Each enabled MCP server needs a server id.";
  }
  const linkError = workspaceLinksError(workspaceLinksFromConfig(config));
  if (linkError) return linkError;
  return null;
}

export function workspaceLinksFromConfig(config: unknown): WorkspaceLinkDraft[] {
  const links = record(record(record(config).features).vfs).workspaceLinks;
  return Array.isArray(links)
    ? structuredClone(links.filter((link) => link && typeof link === "object")) as WorkspaceLinkDraft[]
    : [];
}

export function workspaceLinksError(links: WorkspaceLinkDraft[]): string | null {
  const paths: string[] = [];
  for (const link of links) {
    const path = link.path?.trim() ?? "";
    if (!path) return "Each workspace link needs a session path.";
    if (!isCanonicalAbsolutePath(path)) {
      return `Workspace link path must be canonical and absolute: ${path}`;
    }
    if (paths.some((existing) => pathsOverlap(existing, path))) {
      return `Workspace link paths cannot overlap: ${path}`;
    }
    paths.push(path);
    if (link.target?.type === "workspace") {
      if (!link.target.workspaceId?.trim()) return `Workspace link ${path} needs a workspace.`;
    } else if (link.target?.type === "snapshot") {
      if (!link.target.snapshotRef?.trim()) return `Workspace link ${path} needs a snapshot ref.`;
      if (link.access !== "readOnly") return `Snapshot link ${path} must be read only.`;
    } else {
      return `Workspace link ${path} needs a target.`;
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
  environmentProviders = [],
  featureDisableReasons = {},
  pinnedApiKind,
  className,
}: Props) {
  const config = normalizeSessionConfig(value) ?? {};
  const error = configError(config);
  const [manualModel, setManualModel] = useState(false);

  useEffect(() => onValidityChange?.(error), [error, onValidityChange]);

  const change = (mutate: (next: RecordValue) => void) => {
    const next = structuredClone(config) as RecordValue;
    mutate(next);
    onChange(normalizeSessionConfig(next));
  };

  const features = record(config.features);
  const setFeature = (name: FeatureName, enabled: boolean) =>
    change((next) => {
      const nextFeatures = record(next.features);
      if (enabled) {
        if (name === "web") nextFeatures.web = { search: {} };
        else if (name === "mcp") {
          nextFeatures.mcp = { servers: [{ serverId: firstUsableMcpServerId(mcpServers) }] };
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
    <div className={cn("grid gap-8", className)}>
      <section>
        <div className="grid gap-5 rounded-lg border p-4">
          <ModelFields
            config={config}
            models={models}
            manualModel={manualModel}
            onManualModelChange={setManualModel}
            pinnedApiKind={pinnedApiKind}
            change={change}
          />
          <AdvancedFields config={config} change={change} />
        </div>
      </section>

      <section className="grid gap-4">
        <div className="grid gap-0.5">
          <h2 className="text-sm font-semibold">Features</h2>
          <p className="text-xs text-muted-foreground">
            Features are capability grants. Disabled features are absent from the config.
          </p>
        </div>
        <div className="grid gap-2">
          {(Object.keys(featureInfo) as FeatureName[]).map((name) => (
            <FeaturePanel
              key={name}
              name={name}
              enabled={name in features}
              feature={record(features[name])}
              disableReason={featureDisableReasons[name]}
              onEnabledChange={(enabled) => setFeature(name, enabled)}
            >
              {name === "vfs" && (
                <VfsFields
                  feature={record(features.vfs)}
                  workspaces={workspaces}
                  workspacesLoading={workspacesLoading}
                  patch={(fn) => patchFeature("vfs", fn)}
                />
              )}
              {name === "web" && <WebFields feature={record(features.web)} patch={(fn) => patchFeature("web", fn)} />}
              {name === "fleet" && <FleetFields feature={record(features.fleet)} profiles={profiles} patch={(fn) => patchFeature("fleet", fn)} />}
              {name === "environments" && <EnvironmentFields feature={record(features.environments)} providers={environmentProviders} patch={(fn) => patchFeature("environments", fn)} />}
              {name === "mcp" && <McpFields feature={record(features.mcp)} servers={mcpServers} patch={(fn) => patchFeature("mcp", fn)} />}
            </FeaturePanel>
          ))}
        </div>
      </section>
    </div>
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
    ...models
      .filter((option) => !pinnedApiKind || option.apiKind === pinnedApiKind)
      .map((option) => ({
        key: modelOptionKey(option),
        label: `${option.displayName} (${option.providerId})`,
        search: `${option.displayName} ${option.providerId} ${option.apiKind} ${option.model}`,
        option,
      }))
      .sort((left, right) => left.label.localeCompare(right.label)),
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
    <div className="grid gap-3 border-b pb-5">
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
                <ComboboxItem key={choice.key} value={choice.key} className="py-2">
                  <span className="min-w-0">
                    <span className="block truncate">{choice.label}</span>
                    {choice.option && (
                      <span className="block truncate text-xs text-muted-foreground">
                        {choice.option.model} · {choice.option.apiKind}
                      </span>
                    )}
                  </span>
                </ComboboxItem>
                );
              }}
            </ComboboxList>
          </ComboboxContent>
        </Combobox>
        <FieldDescription>
          {currentModel
            ? `${currentModel.providerId} · ${currentModel.apiKind}`
            : models.length
              ? "Choose a provider-discovered model, or enter a route manually."
              : "No models were discovered. You can still enter a route manually."}
        </FieldDescription>
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
        <FieldDescription>
          {currentModel?.capabilities.reasoningEfforts?.length
            ? "Choose a reported tier or enter any provider-supported value."
            : "No tiers were reported. Enter a provider-supported value, or leave unset for its default."}
        </FieldDescription>
      </Field>
      {selected === "manual" && (
        <div className="grid gap-3 sm:grid-cols-3">
          <Field><FieldLabel>Provider id</FieldLabel><Input value={string(model.providerId)} onChange={(e) => update("providerId", e.target.value)} placeholder="openai" /></Field>
          <Field><FieldLabel>API kind</FieldLabel><Input value={pinnedApiKind ?? string(model.apiKind)} disabled={Boolean(pinnedApiKind)} onChange={(e) => update("apiKind", e.target.value)} placeholder="openai:responses" /></Field>
          <Field><FieldLabel>Model</FieldLabel><Input value={string(model.model)} onChange={(e) => update("model", e.target.value)} placeholder="gpt-5.5" /></Field>
        </div>
      )}
      {currentModel && <ModelCapabilities option={currentModel} />}
    </div>
  );
}

function modelOptionKey(option: Pick<ModelOption, "providerId" | "apiKind" | "model">): string {
  return JSON.stringify([option.providerId, option.apiKind, option.model]);
}

function ModelCapabilities({ option }: { option: ModelOption }) {
  const details = [
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
  const generation = record(config.generation);
  const hasGenerationOverrides = Object.keys(generation).some((key) => key !== "reasoningEffort");
  const hasAdvancedSettings = hasGenerationOverrides || Object.keys(record(config.limits)).length > 0 || Object.keys(record(config.context)).length > 0;
  const [open, setOpen] = useState(hasAdvancedSettings);
  return (
    <div className="grid gap-3">
      <Button
        variant="ghost"
        className="h-auto justify-start gap-2 px-0 py-0 text-left hover:bg-transparent"
        onClick={() => setOpen((value) => !value)}
        aria-expanded={open}
      >
        <SlidersHorizontal className="size-4 text-muted-foreground" />
        <span>
          <span className="block text-sm font-medium">Advanced</span>
          <span className="block text-xs font-normal text-muted-foreground">
            Generation, run limits, and context compaction.
          </span>
        </span>
        {open ? <ChevronDown className="ml-auto" /> : <ChevronRight className="ml-auto" />}
      </Button>
      {open && (
        <div className="ml-2 grid gap-5 border-l border-border/70 py-1 pl-5">
          <GenerationFields config={config} change={change} />
          <LimitsFields config={config} change={change} />
          <ContextFields config={config} change={change} />
        </div>
      )}
    </div>
  );
}

function GenerationFields({ config, change }: { config: RecordValue; change: (fn: (next: RecordValue) => void) => void }) {
  const generation = record(config.generation);
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
              <FieldLabel>Tool id</FieldLabel>
              <Input
                value={string(toolChoice.toolId)}
                onChange={(e) => update("toolChoice", { type: "specific", toolId: e.target.value })}
                placeholder="tool id"
              />
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
  onEnabledChange,
  children,
}: {
  name: FeatureName;
  enabled: boolean;
  feature: RecordValue;
  disableReason?: string;
  onEnabledChange: (enabled: boolean) => void;
  children?: React.ReactNode;
}) {
  const [open, setOpen] = useState(enabled);
  const info = featureInfo[name];
  const Icon = info.icon;
  const configurable = Boolean(children);
  useEffect(() => {
    if (!enabled) setOpen(false);
  }, [enabled]);
  return (
    <div className={cn("rounded-lg border", enabled ? "border-border" : "border-dashed")}>
      <div className="flex min-h-16 items-center gap-3 px-4 py-3">
        <Switch
          checked={enabled}
          disabled={enabled && Boolean(disableReason)}
          onCheckedChange={(checked) => onEnabledChange(checked === true)}
          aria-label={`Enable ${info.title}`}
        />
        <Icon className="size-4 text-muted-foreground" />
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
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label={`Configure ${info.title}`}
            onClick={() => setOpen((value) => !value)}
          >
            {open ? <ChevronDown /> : <ChevronRight />}
          </Button>
        )}
      </div>
      {enabled && configurable && open && <div className="border-t px-4 py-4">{children}</div>}
    </div>
  );
}

function VfsFields({
  feature,
  workspaces,
  workspacesLoading,
  patch,
}: {
  feature: RecordValue;
  workspaces: WorkspaceOption[];
  workspacesLoading: boolean;
  patch: (fn: (feature: RecordValue) => void) => void;
}) {
  const prompts = record(feature.prompts);
  const skills = record(feature.skills);
  const links = Array.isArray(feature.workspaceLinks)
    ? feature.workspaceLinks.map(record)
    : [];
  const workspaceOptions = new Map<string, WorkspaceOption>();
  for (const workspace of workspaces) workspaceOptions.set(workspace.workspaceId, workspace);
  for (const link of links) {
    const target = record(link.target);
    const workspaceId = string(target.workspaceId);
    if (target.type === "workspace" && workspaceId && !workspaceOptions.has(workspaceId)) {
      workspaceOptions.set(workspaceId, { workspaceId });
    }
  }
  const options = [...workspaceOptions.values()];
  const updateLinks = (nextLinks: RecordValue[]) =>
    patch((next) => {
      if (nextLinks.length) next.workspaceLinks = nextLinks;
      else delete next.workspaceLinks;
    });
  const updateLink = (index: number, mutate: (link: RecordValue) => void) =>
    updateLinks(
      links.map((link, linkIndex) => {
        if (linkIndex !== index) return link;
        const next = { ...link };
        mutate(next);
        return next;
      }),
    );
  const nextPath = nextWorkspaceLinkPath(links);

  return (
    <div className="grid gap-5">
      <div className="grid gap-4 sm:grid-cols-2">
        <Field className="sm:col-span-2">
          <FieldLabel>File tools</FieldLabel>
          <Select
            value={string(feature.tools) || "none"}
            onValueChange={(value) => patch((next) => {
              if (value === "none") delete next.tools;
              else next.tools = value;
            })}
          >
            <SelectTrigger className="w-full"><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="none">No file tools</SelectItem>
              <SelectItem value="readOnly">Read only</SelectItem>
              <SelectItem value="edit">Edit files</SelectItem>
            </SelectContent>
          </Select>
        </Field>
        <Field>
          <FieldLabel>Prompt roots</FieldLabel>
          <Input
            className="font-mono"
            value={commaList(prompts.roots)}
            onChange={(e) => patch((next) => {
              const roots = listFromInput(e.target.value);
              if (roots.length) next.prompts = { roots };
              else delete next.prompts;
            })}
            placeholder="/prompts, /team/prompts"
          />
          <FieldDescription className="text-xs">Comma-separated paths.</FieldDescription>
        </Field>
        <Field>
          <FieldLabel>Skill roots</FieldLabel>
          <Input
            className="font-mono"
            value={commaList(skills.roots)}
            onChange={(e) => patch((next) => {
              const roots = listFromInput(e.target.value);
              if (roots.length) next.skills = { roots };
              else delete next.skills;
            })}
            placeholder="/skills, /team/skills"
          />
          <FieldDescription className="text-xs">Comma-separated paths.</FieldDescription>
        </Field>
      </div>

      <div className="grid gap-3 border-t pt-4">
        <div className="flex items-center justify-between gap-3">
          <div>
            <p className="text-sm font-medium">Workspace links</p>
            <p className="text-xs text-muted-foreground">
              Expose catalog workspaces or pinned snapshots at session paths.
            </p>
          </div>
          <Button
            variant="outline"
            size="xs"
            onClick={() => updateLinks([
              ...links,
              {
                path: nextPath,
                access: "readWrite",
                target: {
                  type: "workspace",
                  workspaceId: workspaces[0]?.workspaceId ?? "",
                },
              },
            ])}
          >
            <Plus data-icon="inline-start" />
            Add link
          </Button>
        </div>
        {links.length === 0 && (
          <p className="text-sm text-muted-foreground">No workspace links.</p>
        )}
        {links.map((link, index) => {
          const target = record(link.target);
          const targetType = string(target.type) || "workspace";
          return (
            <div
              key={index}
              className="grid gap-3 border-t pt-3 sm:grid-cols-[minmax(0,1fr)_auto]"
            >
              <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
                <Field>
                  <FieldLabel>Target type</FieldLabel>
                  <Select
                    value={targetType}
                    onValueChange={(value) => updateLink(index, (next) => {
                      if (value === "snapshot") {
                        next.target = { type: "snapshot", snapshotRef: "" };
                        next.access = "readOnly";
                      } else {
                        next.target = {
                          type: "workspace",
                          workspaceId: workspaces[0]?.workspaceId ?? "",
                        };
                        next.access = "readWrite";
                      }
                    })}
                  >
                    <SelectTrigger className="w-full"><SelectValue /></SelectTrigger>
                    <SelectContent>
                      <SelectItem value="workspace">Workspace</SelectItem>
                      <SelectItem value="snapshot">Snapshot</SelectItem>
                    </SelectContent>
                  </Select>
                </Field>
                {targetType === "snapshot" ? (
                  <Field>
                    <FieldLabel>Snapshot ref</FieldLabel>
                    <Input
                      className="font-mono"
                      value={string(target.snapshotRef)}
                      onChange={(event) => updateLink(index, (next) => {
                        next.target = { ...record(next.target), type: "snapshot", snapshotRef: event.target.value };
                      })}
                    />
                  </Field>
                ) : (
                  <Field>
                    <FieldLabel>Workspace</FieldLabel>
                    {options.length || workspacesLoading ? (
                      <Select
                        value={string(target.workspaceId)}
                        disabled={workspacesLoading}
                        onValueChange={(workspaceId) => updateLink(index, (next) => {
                          next.target = { ...record(next.target), type: "workspace", workspaceId };
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
                        value={string(target.workspaceId)}
                        onChange={(event) => updateLink(index, (next) => {
                          next.target = { ...record(next.target), type: "workspace", workspaceId: event.target.value };
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
                    value={string(link.path)}
                    onChange={(event) => updateLink(index, (next) => {
                      next.path = event.target.value;
                    })}
                  />
                </Field>
                <Field>
                  <FieldLabel>Access</FieldLabel>
                  <Select
                    value={string(link.access) || "readWrite"}
                    disabled={targetType === "snapshot"}
                    onValueChange={(access) => updateLink(index, (next) => {
                      next.access = access;
                    })}
                  >
                    <SelectTrigger className="w-full"><SelectValue /></SelectTrigger>
                    <SelectContent>
                      <SelectItem value="readWrite">Read and write</SelectItem>
                      <SelectItem value="readOnly">Read only</SelectItem>
                    </SelectContent>
                  </Select>
                </Field>
              </div>
              <Button
                variant="ghost"
                size="icon-sm"
                className="self-end text-destructive"
                aria-label="Remove workspace link"
                onClick={() => updateLinks(links.filter((_, linkIndex) => linkIndex !== index))}
              >
                <Trash2 />
              </Button>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function nextWorkspaceLinkPath(links: RecordValue[]): string {
  const paths = new Set(links.map((link) => string(link.path)));
  if (!paths.has("/workspace")) return "/workspace";
  let suffix = 2;
  while (paths.has(`/workspace-${suffix}`)) suffix += 1;
  return `/workspace-${suffix}`;
}

function WebFields({ feature, patch }: { feature: RecordValue; patch: (fn: (feature: RecordValue) => void) => void }) {
  const search = record(feature.search);
  const fetchEnabled = "fetch" in feature; const searchEnabled = "search" in feature;
  const setSubfeature = (name: "fetch" | "search", enabled: boolean) => patch((next) => { if (enabled) next[name] = {}; else { delete next[name]; if (!("fetch" in next) && !("search" in next)) next.search = {}; } });
  return (
    <div className="grid gap-4">
      <div className="flex flex-wrap gap-x-6 gap-y-3">
        <Label className="gap-2 font-normal"><Checkbox checked={fetchEnabled} onCheckedChange={(checked) => setSubfeature("fetch", checked === true)} />Fetch pages</Label>
        <Label className="gap-2 font-normal"><Checkbox checked={searchEnabled} onCheckedChange={(checked) => setSubfeature("search", checked === true)} />Search the web</Label>
      </div>
      {searchEnabled && (
        <div className="grid gap-3 sm:grid-cols-2">
          <Field>
            <FieldLabel>Allowed domains</FieldLabel>
            <Input
              value={commaList(search.allowedDomains)}
              onChange={(e) => patch((next) => {
                const domains = listFromInput(e.target.value);
                const item = record(next.search);
                if (domains.length) item.allowedDomains = domains;
                else delete item.allowedDomains;
                next.search = item;
              })}
              placeholder="All domains"
            />
            <FieldDescription className="text-xs">Comma-separated domains. Empty allows all.</FieldDescription>
          </Field>
          <Field>
            <FieldLabel>Blocked domains</FieldLabel>
            <Input
              value={commaList(search.blockedDomains)}
              onChange={(e) => patch((next) => {
                const domains = listFromInput(e.target.value);
                const item = record(next.search);
                if (domains.length) item.blockedDomains = domains;
                else delete item.blockedDomains;
                next.search = item;
              })}
              placeholder="None"
            />
            <FieldDescription className="text-xs">Comma-separated domains. Empty blocks none.</FieldDescription>
          </Field>
        </div>
      )}
    </div>
  );
}

function FleetFields({
  feature,
  profiles,
  patch,
}: {
  feature: RecordValue;
  profiles: ProfileOption[];
  patch: (fn: (feature: RecordValue) => void) => void;
}) {
  const policy = record(feature.profiles);
  const spawn = record(feature.spawn);
  const bases = stringList(spawn.bases);
  const updateProfiles = (key: "allow" | "deny", values: string[]) =>
    patch((next) => {
      const item = record(next.profiles);
      if (values.length) item[key] = values;
      else delete item[key];
      if (Object.keys(item).length) next.profiles = item;
      else delete next.profiles;
    });

  return (
    <div className="grid gap-4 sm:grid-cols-2">
      <Field>
        <FieldLabel>Allowed profiles</FieldLabel>
        <ProfileMultiSelect
          value={stringList(policy.allow)}
          profiles={profiles}
          onChange={(values) => updateProfiles("allow", values)}
          placeholder="All profiles"
        />
        <FieldDescription className="text-xs">Empty allows every named profile.</FieldDescription>
      </Field>
      <Field>
        <FieldLabel>Denied profiles</FieldLabel>
        <ProfileMultiSelect
          value={stringList(policy.deny)}
          profiles={profiles}
          onChange={(values) => updateProfiles("deny", values)}
          placeholder="No denied profiles"
        />
        <FieldDescription className="text-xs">Denied profiles take precedence.</FieldDescription>
      </Field>
      <Label className="gap-2 font-normal">
        <Checkbox
          checked={policy.inline !== false}
          onCheckedChange={(checked) => patch((next) => {
            const item = record(next.profiles);
            if (checked === true) delete item.inline;
            else item.inline = false;
            if (Object.keys(item).length) next.profiles = item;
            else delete next.profiles;
          })}
        />
        Allow inline agents
      </Label>
      <Field>
        <FieldLabel>Spawn bases</FieldLabel>
        <SpawnBaseMultiSelect
          value={bases}
          onChange={(values) => patch((next) => {
            if (values.length) next.spawn = { bases: values };
            else delete next.spawn;
          })}
        />
        <FieldDescription className="text-xs">Empty allows self, session, and profile bases.</FieldDescription>
      </Field>
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

const spawnBaseOptions = [
  { value: "self", label: "Self" },
  { value: "session", label: "Session" },
  { value: "profile", label: "Profile" },
] as const;

function SpawnBaseMultiSelect({
  value,
  onChange,
}: {
  value: string[];
  onChange: (value: string[]) => void;
}) {
  const labels = new Map(spawnBaseOptions.map((option) => [option.value, option.label]));
  const items = spawnBaseOptions.map((option) => option.value);
  return (
    <Combobox items={items} multiple value={value} onValueChange={onChange}>
      <ComboboxChips>
        <ComboboxValue>
          {value.map((base) => (
            <ComboboxChip key={base}>{labels.get(base as (typeof items)[number]) ?? base}</ComboboxChip>
          ))}
        </ComboboxValue>
        <ComboboxChipsInput placeholder={value.length ? "Add base" : "All spawn bases"} />
      </ComboboxChips>
      <ComboboxContent>
        <ComboboxEmpty>No matching bases.</ComboboxEmpty>
        <ComboboxList>
          {(base: string) => (
            <ComboboxItem key={base} value={base}>
              {labels.get(base as (typeof items)[number]) ?? base}
            </ComboboxItem>
          )}
        </ComboboxList>
      </ComboboxContent>
    </Combobox>
  );
}

function EnvironmentFields({
  feature,
  providers,
  patch,
}: {
  feature: RecordValue;
  providers: EnvironmentProviderOption[];
  patch: (fn: (feature: RecordValue) => void) => void;
}) {
  const jobsId = useId();
  const selectionToolsId = useId();
  const value = stringList(feature.providers);
  const providerMap = new Map(providers.map((provider) => [provider.providerId, provider]));
  const items = [
    ...new Set([...providers.map((provider) => provider.providerId), ...value]),
  ].sort((left, right) =>
    providerLabel(providerMap.get(left), left).localeCompare(
      providerLabel(providerMap.get(right), right),
    ),
  );
  return (
    <div className="grid gap-4">
      <Field>
        <FieldLabel>Allowed providers</FieldLabel>
        <Combobox
          items={items}
          multiple
          value={value}
          onValueChange={(nextValue) => patch((next) => {
            if (nextValue.length) next.providers = nextValue;
            else delete next.providers;
          })}
          itemToStringLabel={(providerId) =>
            providerLabel(providerMap.get(providerId), providerId)
          }
          filter={(providerId, query) => {
            const provider = providerMap.get(providerId);
            const search = `${providerLabel(provider, providerId)} ${providerId}`.toLocaleLowerCase();
            return search.includes(query.toLocaleLowerCase());
          }}
        >
          <ComboboxChips>
            <ComboboxValue>
              {value.map((providerId) => (
                <ComboboxChip key={providerId}>
                  {providerLabel(providerMap.get(providerId), providerId)}
                </ComboboxChip>
              ))}
            </ComboboxValue>
            <ComboboxChipsInput
              placeholder={value.length ? "Add provider" : "All registered providers"}
            />
          </ComboboxChips>
          <ComboboxContent>
            <ComboboxEmpty>No matching providers.</ComboboxEmpty>
            <ComboboxList>
              {(providerId: string) => (
                <ComboboxItem key={providerId} value={providerId}>
                  <span className="min-w-0">
                    <span className="block truncate">
                      {providerLabel(providerMap.get(providerId), providerId)}
                    </span>
                    {providerMap.get(providerId)?.displayName && (
                      <span className="block truncate font-mono text-xs text-muted-foreground">
                        {providerId}
                      </span>
                    )}
                  </span>
                </ComboboxItem>
              )}
            </ComboboxList>
          </ComboboxContent>
        </Combobox>
        <FieldDescription className="text-xs">
          Empty allows every registered provider. Selection resolves live universe environments.
        </FieldDescription>
      </Field>
      <div className="flex items-start justify-between gap-4 rounded-lg border p-3">
        <div className="grid gap-1">
          <Label htmlFor={selectionToolsId}>Environment selection tools</Label>
          <p className="text-xs text-muted-foreground">
            Let the model list, activate, and deactivate allowed environments. Reading the active environment is always available.
          </p>
        </div>
        <Switch
          id={selectionToolsId}
          checked={feature.selectionTools === true}
          onCheckedChange={(checked) => patch((next) => {
            if (checked === true) next.selectionTools = true;
            else delete next.selectionTools;
          })}
        />
      </div>
      <div className="flex items-start justify-between gap-4 rounded-lg border p-3">
        <div className="grid gap-1">
          <Label htmlFor={jobsId}>Durable jobs</Label>
          <p className="text-xs text-muted-foreground">
            Allow the agent to use durable job tools on capable environments.
          </p>
        </div>
        <Switch
          id={jobsId}
          checked={feature.jobs === true}
          onCheckedChange={(checked) => patch((next) => {
            if (checked === true) next.jobs = true;
            else delete next.jobs;
          })}
        />
      </div>
    </div>
  );
}

function providerLabel(
  provider: EnvironmentProviderOption | undefined,
  providerId: string,
): string {
  return provider?.displayName || providerId;
}

function McpFields({
  feature,
  servers,
  patch,
}: {
  feature: RecordValue;
  servers: McpServerOption[];
  patch: (fn: (feature: RecordValue) => void) => void;
}) {
  const links = Array.isArray(feature.servers) ? feature.servers.map(record) : [];
  const options = new Map<string, McpServerOption>();
  for (const server of servers) options.set(server.serverId, server);
  for (const link of links) {
    const serverId = string(link.serverId);
    if (serverId && !options.has(serverId)) options.set(serverId, { serverId });
  }
  const serverOptions = [...options.values()];
  const firstServerId = firstUsableMcpServerId(serverOptions);
  const updateLinks = (nextLinks: RecordValue[]) =>
    patch((next) => {
      next.servers = nextLinks;
    });
  const updateLink = (index: number, mutate: (link: RecordValue) => void) =>
    updateLinks(
      links.map((link, linkIndex) => {
        if (linkIndex !== index) return link;
        const next = { ...link };
        mutate(next);
        return next;
      }),
    );

  return (
    <div className="grid gap-3">
      <div className="flex items-center justify-between gap-3">
        <p className="text-xs text-muted-foreground">
          Only declared servers can materialize remote tools.
        </p>
        <Button
          variant="outline"
          size="xs"
          onClick={() => updateLinks([...links, { serverId: firstServerId }])}
        >
          <Plus data-icon="inline-start" />
          Add server
        </Button>
      </div>
      {links.map((link, index) => (
        <div
          key={index}
          className="grid gap-3 border-t pt-3 sm:grid-cols-[minmax(0,1fr)_auto]"
        >
          <div className="grid gap-3 sm:grid-cols-3">
            <Field>
              <FieldLabel>Server</FieldLabel>
              {serverOptions.length ? (
                <Select
                  value={string(link.serverId)}
                  onValueChange={(value) => updateLink(index, (next) => { next.serverId = value as string; })}
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
                  value={string(link.serverId)}
                  onChange={(e) => updateLink(index, (next) => { next.serverId = e.target.value; })}
                />
              )}
            </Field>
            <Field>
              <FieldLabel>Allowed tools</FieldLabel>
              <Input
                value={commaList(link.allowedTools)}
                onChange={(e) => updateLink(index, (next) => {
                  const tools = listFromInput(e.target.value);
                  if (tools.length) next.allowedTools = tools;
                  else delete next.allowedTools;
                })}
                placeholder="All tools"
              />
            </Field>
            <Field>
              <FieldLabel>Approval</FieldLabel>
              <Select
                value={string(link.approval) || "providerDefault"}
                onValueChange={(value) => updateLink(index, (next) => {
                  if (value === "providerDefault") delete next.approval;
                  else next.approval = value;
                })}
              >
                <SelectTrigger className="w-full"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="providerDefault">Server default</SelectItem>
                  <SelectItem value="always">Always approve</SelectItem>
                  <SelectItem value="never">Never approve</SelectItem>
                </SelectContent>
              </Select>
            </Field>
          </div>
          <Button
            variant="ghost"
            size="icon-sm"
            className="self-end text-destructive"
            aria-label="Remove MCP server"
            onClick={() => updateLinks(links.filter((_, linkIndex) => linkIndex !== index))}
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
