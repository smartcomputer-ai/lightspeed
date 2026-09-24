import { useActionPermissions } from "@/lib/permissions";
import { useState, type FormEvent } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { RefreshCw, Search } from "lucide-react";
import {
  api,
  type ModelEndpointConfig,
  type ModelListResponse,
  type ModelOption,
  type SecretGrant,
  type SecretProvider,
} from "@/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { DialogFooter } from "@/components/ui/dialog";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { IdText } from "@/components/ui/table";
import { ConfirmDangerButton } from "@/components/confirm-danger-button";
import { providerEndpoint } from "./use-model-providers";

export type ModelKeyProvider = "openai" | "anthropic";

const COPY: Record<
  ModelKeyProvider,
  { name: string; where: string; placeholder: string }
> = {
  openai: {
    name: "OpenAI",
    where: "platform.openai.com → API keys",
    placeholder: "sk-…",
  },
  anthropic: {
    name: "Anthropic",
    where: "console.anthropic.com → API keys",
    placeholder: "sk-ant-api…",
  },
};

const COMPATIBLE_PROVIDER_PRESETS = [
  {
    id: "deepseek",
    label: "DeepSeek",
    baseUrl: "https://api.deepseek.com",
  },
  {
    id: "openrouter",
    label: "OpenRouter",
    baseUrl: "https://openrouter.ai/api/v1",
  },
  {
    id: "ollama",
    label: "Ollama",
    baseUrl: "http://localhost:11434/v1",
  },
  {
    id: "vllm",
    label: "vLLM",
    baseUrl: "http://localhost:8000/v1",
  },
] as const;

type CompatibleProviderPresetId =
  (typeof COMPATIBLE_PROVIDER_PRESETS)[number]["id"];
type CompatibleProviderChoice = CompatibleProviderPresetId | "custom";

function compatibleProviderPreset(id: string) {
  return COMPATIBLE_PROVIDER_PRESETS.find((preset) => preset.id === id);
}

/// Add or replace the API key Lightspeed sessions use for a model provider
/// (`model:<provider>` row). Rendered in the Add-model-provider dialog and in
/// the details dialog (replace mode).
export function ModelApiKeyForm({
  universeId,
  provider,
  replace,
  onSaved,
  onCancel,
  cancelLabel = "Back",
}: {
  universeId: string;
  provider: ModelKeyProvider;
  replace: boolean;
  onSaved: (provider: SecretProvider) => void;
  onCancel: () => void;
  cancelLabel?: string;
}) {
  const copy = COPY[provider];
  const [displayName, setDisplayName] = useState("");
  const [key, setKey] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [headers, setHeaders] = useState("");
  const [error, setError] = useState<string | null>(null);

  const save = useMutation<SecretProvider, Error, void>({
    mutationFn: () => {
      const endpoint = baseUrl.trim()
        ? {
            baseUrl: baseUrl.trim(),
            headers: parseHeaders(headers),
            apiKinds: ["openai:responses", "openai:completions"],
          }
        : undefined;
      return api<SecretProvider>(
        "POST",
        `/api/v1/universes/${universeId}/integrations/model-keys`,
        {
          provider,
          credential: key,
          replace,
          endpoint,
          ...(displayName.trim() ? { displayName: displayName.trim() } : {}),
        },
      );
    },
    onSuccess: (saved) => {
      setKey("");
      onSaved(saved);
    },
    onError: (reason) => setError(reason.message),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!key.trim()) {
      setError("paste the API key first");
      return;
    }
    save.mutate();
  };

  return (
    <form onSubmit={submit} className="grid gap-4">
      <p className="text-sm text-muted-foreground">
        Sessions using{" "}
        <span className="font-mono">model.providerId = {provider}</span> will
        use this key for model discovery and inference instead of the
        deployment-wide fallback key.
        {replace && " Saving replaces the current key."} The key is sent once to
        Lightspeed, encrypted, and never returned by an API.
      </p>
      <Field>
        <FieldLabel htmlFor={`model-key-name-${provider}`}>
          Display name
        </FieldLabel>
        <Input
          id={`model-key-name-${provider}`}
          value={displayName}
          onChange={(event) => setDisplayName(event.target.value)}
          placeholder={`${copy.name} production`}
        />
      </Field>
      {provider === "openai" && (
        <>
          <Field>
            <FieldLabel htmlFor="model-key-base-url">
              Compatible endpoint override
            </FieldLabel>
            <Input
              id="model-key-base-url"
              value={baseUrl}
              onChange={(event) => setBaseUrl(event.target.value)}
              placeholder="https://openrouter.ai/api/v1"
            />
            <FieldDescription>
              Optional. HTTPS is required except for localhost/loopback HTTP.
            </FieldDescription>
          </Field>
          {baseUrl.trim() && (
            <Field>
              <FieldLabel htmlFor="model-key-headers">Extra headers</FieldLabel>
              <Textarea
                id="model-key-headers"
                value={headers}
                onChange={(event) => setHeaders(event.target.value)}
                placeholder={
                  "HTTP-Referer: https://example.com\nX-Title: Lightspeed"
                }
                className="font-mono"
              />
              <FieldDescription>
                One non-secret header per line. Authorization is reserved.
              </FieldDescription>
            </Field>
          )}
        </>
      )}
      <Field>
        <FieldLabel htmlFor={`model-key-${provider}`}>API key</FieldLabel>
        <Input
          id={`model-key-${provider}`}
          type="password"
          value={key}
          onChange={(event) => {
            setKey(event.target.value);
            setError(null);
          }}
          autoComplete="new-password"
          spellCheck={false}
          className="font-mono"
          placeholder={copy.placeholder}
          autoFocus
        />
        <FieldDescription>
          Create one at {copy.where}. Whitespace is preserved.
        </FieldDescription>
      </Field>
      {error && <p className="text-sm text-destructive">{error}</p>}
      <DialogFooter>
        <Button type="button" variant="outline" onClick={onCancel}>
          {cancelLabel}
        </Button>
        <Button type="submit" disabled={save.isPending}>
          {save.isPending
            ? "Encrypting…"
            : replace
              ? "Replace key"
              : "Save key"}
        </Button>
      </DialogFooter>
    </form>
  );
}

/// Details for a connected model API key: status, replace, remove.
export function ModelApiKeyDetails({
  universeId,
  provider,
  onChanged,
  onRemoved,
}: {
  universeId: string;
  provider: SecretProvider;
  onChanged: () => void;
  onRemoved: () => void;
}) {
  const writable = useActionPermissions(universeId).can("configure_resource");
  const [replacing, setReplacing] = useState(false);
  const remove = useMutation({
    mutationFn: () =>
      api<SecretProvider>(
        "DELETE",
        `/api/v1/universes/${universeId}/secrets/providers/${encodeURIComponent(provider.credentialId)}`,
      ),
    onSuccess: onRemoved,
  });
  const providerKey: ModelKeyProvider =
    provider.providerId === "openai" ? "openai" : "anthropic";
  const endpoint = providerEndpoint(provider);

  if (writable && replacing) {
    return (
      <ModelApiKeyForm
        universeId={universeId}
        provider={providerKey}
        replace
        onSaved={() => {
          setReplacing(false);
          onChanged();
        }}
        onCancel={() => setReplacing(false)}
        cancelLabel="Cancel"
      />
    );
  }

  return (
    <div className="grid gap-4">
      <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
        <dt className="text-muted-foreground">Provider</dt>
        <dd className="font-mono">{provider.providerId}</dd>
        <dt className="text-muted-foreground">Credential ID</dt>
        <dd>
          <IdText>{provider.credentialId}</IdText>
        </dd>
        <dt className="text-muted-foreground">Status</dt>
        <dd>
          <ProviderStatusBadge provider={provider} />
        </dd>
      </dl>
      <p className="text-sm text-muted-foreground">
        Used by Lightspeed sessions with{" "}
        <span className="font-mono">
          model.providerId = {provider.providerId}
        </span>
        . Not injected into environments; coding-agent subscriptions are
        added separately.
      </p>
      {endpoint && <EndpointSummary endpoint={endpoint} />}
      <ProviderModelList
        universeId={universeId}
        providerId={provider.providerId}
      />
      {remove.error && (
        <p className="text-sm text-destructive">{remove.error.message}</p>
      )}
      {writable && (
        <DialogFooter>
          <ConfirmDangerButton
            label="Remove key"
            title="Remove this API key?"
            description={
              <>
                Sessions using{" "}
                <span className="font-mono text-xs">{provider.providerId}</span>{" "}
                fall back to the deployment-wide key, or fail if none is
                configured.
              </>
            }
            pending={remove.isPending}
            onConfirm={() => remove.mutate()}
          />
          <Button onClick={() => setReplacing(true)}>Replace key</Button>
        </DialogFooter>
      )}
    </div>
  );
}

export function OpenAiCompatibleForm({
  universeId,
  replace = false,
  initial,
  onSaved,
  onCancel,
}: {
  universeId: string;
  replace?: boolean;
  initial?: SecretProvider;
  onSaved: (provider: SecretProvider) => void;
  onCancel: () => void;
}) {
  const initialEndpoint =
    initial?.config.type === "modelApiKey" ||
    initial?.config.type === "modelOAuth"
      ? initial.config.endpoint
      : initial?.config.type === "modelEndpoint"
        ? initial.config.endpoint
        : undefined;
  const initialProviderId = initial?.providerId ?? "deepseek";
  const initialPreset = compatibleProviderPreset(initialProviderId);
  const [providerChoice, setProviderChoice] =
    useState<CompatibleProviderChoice>(initialPreset?.id ?? "custom");
  const [providerId, setProviderId] = useState(initialProviderId);
  const [displayName, setDisplayName] = useState(
    initial?.displayName ?? initialPreset?.label ?? "",
  );
  const [baseUrl, setBaseUrl] = useState(
    initialEndpoint?.baseUrl ?? initialPreset?.baseUrl ?? "",
  );
  const [key, setKey] = useState("");
  const [headers, setHeaders] = useState(
    Object.entries(initialEndpoint?.headers ?? {})
      .map(([name, value]) => `${name}: ${value}`)
      .join("\n"),
  );
  const [responses, setResponses] = useState(
    initialEndpoint?.apiKinds.includes("openai:responses") ?? false,
  );
  const [completions, setCompletions] = useState(
    initialEndpoint?.apiKinds.includes("openai:completions") ?? true,
  );
  const [error, setError] = useState<string | null>(null);
  const changeProvider = (choice: CompatibleProviderChoice) => {
    setProviderChoice(choice);
    setError(null);
    if (choice === "custom") {
      setProviderId("");
      setDisplayName("");
      setBaseUrl("");
      return;
    }
    const preset = compatibleProviderPreset(choice);
    if (!preset) return;
    setProviderId(preset.id);
    setDisplayName(preset.label);
    setBaseUrl(preset.baseUrl);
    setResponses(false);
    setCompletions(true);
  };
  const save = useMutation<SecretProvider, Error, void>({
    mutationFn: () => {
      const apiKinds = [
        ...(responses ? (["openai:responses"] as const) : []),
        ...(completions ? (["openai:completions"] as const) : []),
      ];
      if (!apiKinds.length) throw new Error("select at least one API kind");
      return api<SecretProvider>(
        "POST",
        `/api/v1/universes/${universeId}/integrations/model-keys`,
        {
          provider: providerId.trim(),
          ...(key ? { credential: key } : {}),
          endpoint: {
            baseUrl: baseUrl.trim(),
            headers: parseHeaders(headers),
            apiKinds,
          },
          replace,
          ...(displayName.trim() ? { displayName: displayName.trim() } : {}),
        },
      );
    },
    onSuccess: onSaved,
    onError: (reason) => setError(reason.message),
  });
  return (
    <form
      className="grid gap-4"
      onSubmit={(event) => {
        event.preventDefault();
        if (!providerId.trim() || !baseUrl.trim()) {
          setError("provider ID and base URL are required");
          return;
        }
        if (replace && initial?.hasCredential && !key) {
          setError(
            "paste the replacement API key; existing keys cannot be read back",
          );
          return;
        }
        save.mutate();
      }}
    >
      <p className="text-sm text-muted-foreground">
        Add an OpenAI-compatible Responses or Chat Completions endpoint. API
        keys are optional for local credentialless servers.
      </p>
      <Field>
        <FieldLabel htmlFor="compatible-provider">Provider</FieldLabel>
        <Select
          value={providerChoice}
          onValueChange={(value) =>
            changeProvider(value as CompatibleProviderChoice)
          }
          disabled={replace}
        >
          <SelectTrigger id="compatible-provider">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {COMPATIBLE_PROVIDER_PRESETS.map((preset) => (
              <SelectItem key={preset.id} value={preset.id}>
                {preset.label}
              </SelectItem>
            ))}
            <SelectItem value="custom">Custom provider</SelectItem>
          </SelectContent>
        </Select>
        <FieldDescription>
          {providerChoice === "custom"
            ? "Custom IDs use the generic OpenAI-compatible dialect."
            : providerChoice === "deepseek" || providerChoice === "openrouter"
              ? `Uses the canonical provider ID “${providerId}” so provider-specific compatibility rules apply.`
              : `Uses the conventional provider ID “${providerId}” with the generic OpenAI-compatible dialect.`}
        </FieldDescription>
      </Field>
      {providerChoice === "custom" && (
        <Field>
          <FieldLabel htmlFor="compatible-provider-id">
            Custom provider ID
          </FieldLabel>
          <Input
            id="compatible-provider-id"
            value={providerId}
            disabled={replace}
            onChange={(event) => setProviderId(event.target.value)}
            placeholder="my-provider"
          />
          <FieldDescription>
            Lowercase letters, numbers, dots, underscores, and hyphens are
            supported.
          </FieldDescription>
        </Field>
      )}
      <Field>
        <FieldLabel>Display name</FieldLabel>
        <Input
          value={displayName}
          onChange={(event) => setDisplayName(event.target.value)}
          placeholder="OpenRouter production"
        />
      </Field>
      <Field>
        <FieldLabel>Base URL</FieldLabel>
        <Input
          value={baseUrl}
          onChange={(event) => setBaseUrl(event.target.value)}
          placeholder="https://openrouter.ai/api/v1"
        />
      </Field>
      <Field>
        <FieldLabel>API key (optional)</FieldLabel>
        <Input
          type="password"
          value={key}
          onChange={(event) => setKey(event.target.value)}
          autoComplete="new-password"
        />
      </Field>
      <Field>
        <FieldLabel>API kinds</FieldLabel>
        <label className="flex gap-2 text-sm">
          <input
            type="checkbox"
            checked={completions}
            onChange={(event) => setCompletions(event.target.checked)}
          />{" "}
          Chat Completions
        </label>
        <label className="flex gap-2 text-sm">
          <input
            type="checkbox"
            checked={responses}
            onChange={(event) => setResponses(event.target.checked)}
          />{" "}
          Responses
        </label>
      </Field>
      <Field>
        <FieldLabel>Extra headers</FieldLabel>
        <Textarea
          value={headers}
          onChange={(event) => setHeaders(event.target.value)}
          placeholder={"HTTP-Referer: https://example.com\nX-Title: Lightspeed"}
          className="font-mono"
        />
        <FieldDescription>
          Non-secret headers only; Authorization, Host, and Content-Type are
          reserved.
        </FieldDescription>
      </Field>
      {error && <p className="text-sm text-destructive">{error}</p>}
      <DialogFooter>
        <Button type="button" variant="outline" onClick={onCancel}>
          Cancel
        </Button>
        <Button type="submit" disabled={save.isPending}>
          {save.isPending ? "Saving…" : "Save provider"}
        </Button>
      </DialogFooter>
    </form>
  );
}

/// Details for an OpenAI-compatible provider, and for any other model
/// provider row: status, endpoint, authentication, edit, remove.
export function OpenAiCompatibleDetails({
  universeId,
  provider,
  oauthGrant,
  onChanged,
  onRemoved,
}: {
  universeId: string;
  provider: SecretProvider;
  oauthGrant?: SecretGrant;
  onChanged: () => void;
  onRemoved: () => void;
}) {
  const writable = useActionPermissions(universeId).can("configure_resource");
  const [editing, setEditing] = useState(false);
  const remove = useMutation({
    mutationFn: () =>
      api(
        "DELETE",
        `/api/v1/universes/${universeId}/secrets/providers/${encodeURIComponent(provider.credentialId)}`,
      ),
    onSuccess: onRemoved,
  });
  const endpoint = providerEndpoint(provider);
  if (writable && editing && endpoint)
    return (
      <OpenAiCompatibleForm
        universeId={universeId}
        replace
        initial={provider}
        onSaved={() => {
          setEditing(false);
          onChanged();
        }}
        onCancel={() => setEditing(false)}
      />
    );
  return (
    <div className="grid gap-4">
      <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
        <dt className="text-muted-foreground">Provider</dt>
        <dd className="font-mono">{provider.providerId}</dd>
        <dt className="text-muted-foreground">Credential ID</dt>
        <dd>
          <IdText>{provider.credentialId}</IdText>
        </dd>
        <dt className="text-muted-foreground">Status</dt>
        <dd>
          <ProviderStatusBadge provider={provider} oauthGrant={oauthGrant} />
        </dd>
        <dt className="text-muted-foreground">Authentication</dt>
        <dd>
          {provider.config.type === "modelOAuth" ? (
            <>
              OAuth · <IdText>{provider.config.grantId}</IdText>
            </>
          ) : provider.hasCredential ? (
            "API key"
          ) : (
            "None"
          )}
        </dd>
      </dl>
      {endpoint && <EndpointSummary endpoint={endpoint} />}
      <ProviderModelList
        universeId={universeId}
        providerId={provider.providerId}
      />
      {remove.error && (
        <p className="text-sm text-destructive">{remove.error.message}</p>
      )}
      {writable && (
        <DialogFooter>
          <ConfirmDangerButton
            label="Remove provider"
            title="Remove this model provider?"
            description="New calls using this provider ID will fail immediately."
            pending={remove.isPending}
            onConfirm={() => remove.mutate()}
          />
          {endpoint && <Button onClick={() => setEditing(true)}>Edit provider</Button>}
        </DialogFooter>
      )}
    </div>
  );
}

/// Why a provider row is or is not used for model calls.
function ProviderStatusBadge({
  provider,
  oauthGrant,
}: {
  provider: SecretProvider;
  oauthGrant?: SecretGrant;
}) {
  const attention = (label: string) => (
    <Badge variant="outline" className="border-destructive/50 text-destructive">
      {label}
    </Badge>
  );
  if (!provider.usableForModels) return attention("legacy id — replace");
  if (provider.status === "disabled") return <Badge variant="outline">disabled</Badge>;
  if (provider.config.type === "modelOAuth") {
    if (!oauthGrant || oauthGrant.status === "revoked") return attention("OAuth grant missing");
    if (oauthGrant.status !== "active") {
      return attention(oauthGrant.status === "needsReauth" ? "needs reauth" : oauthGrant.status);
    }
  } else if (provider.config.type !== "modelEndpoint" && !provider.hasCredential) {
    return attention("needs key");
  }
  if (provider.status !== "active") return attention("needs configuration");
  return <Badge variant="secondary">active</Badge>;
}

export interface ProviderModelCatalogEntry {
  model: string;
  displayName: string;
  apiKinds: string[];
  createdAtMs?: number | null;
}

/** Filter one provider's live model results and collapse API-kind variants. */
export function providerModelCatalog(
  models: ModelOption[] | undefined,
  providerId: string,
  search: string,
): ProviderModelCatalogEntry[] {
  const normalizedSearch = search.trim().toLocaleLowerCase();
  const catalog = new Map<string, ProviderModelCatalogEntry>();
  for (const model of models ?? []) {
    if (model.providerId !== providerId) continue;
    if (
      normalizedSearch &&
      ![model.displayName, model.model]
        .join(" ")
        .toLocaleLowerCase()
        .includes(normalizedSearch)
    ) {
      continue;
    }
    const existing = catalog.get(model.model);
    if (existing) {
      if (!existing.apiKinds.includes(model.apiKind))
        existing.apiKinds.push(model.apiKind);
      if ((model.createdAtMs ?? 0) > (existing.createdAtMs ?? 0)) {
        existing.createdAtMs = model.createdAtMs;
      }
    } else {
      catalog.set(model.model, {
        model: model.model,
        displayName: model.displayName || model.model,
        apiKinds: [model.apiKind],
        createdAtMs: model.createdAtMs,
      });
    }
  }
  return [...catalog.values()].sort((left, right) => {
    if (
      left.createdAtMs &&
      right.createdAtMs &&
      left.createdAtMs !== right.createdAtMs
    ) {
      return right.createdAtMs - left.createdAtMs;
    }
    if (left.createdAtMs) return -1;
    if (right.createdAtMs) return 1;
    return left.displayName.localeCompare(right.displayName);
  });
}

/** Live provider model catalog. Credentials remain server-side during discovery. */
export function ProviderModelList({
  universeId,
  providerId,
}: {
  universeId: string;
  providerId: string;
}) {
  const [search, setSearch] = useState("");
  const discovery = useQuery({
    queryKey: ["models", universeId],
    queryFn: () =>
      api<ModelListResponse>(
        "GET",
        "/api/v1/universes/" + universeId + "/models",
      ),
    staleTime: 60_000,
    refetchOnMount: "always",
  });
  const provider = discovery.data?.providers?.find(
    (item) => item.providerId === providerId,
  );
  const allModels = providerModelCatalog(
    discovery.data?.models,
    providerId,
    "",
  );
  const visibleModels = providerModelCatalog(
    discovery.data?.models,
    providerId,
    search,
  );

  return (
    <section
      className="grid gap-2"
      aria-labelledby={"provider-models-" + providerId}
    >
      <div className="flex items-center justify-between gap-2">
        <div>
          <h3
            id={"provider-models-" + providerId}
            className="text-sm font-medium"
          >
            Available models
            {!discovery.isFetching && !discovery.error
              ? " (" + allModels.length + ")"
              : ""}
          </h3>
          <p className="text-xs text-muted-foreground">
            Fetched live through Lightspeed; the credential is never sent to the
            browser.
          </p>
        </div>
        <Button
          type="button"
          variant="outline"
          size="icon-sm"
          aria-label="Refresh available models"
          title="Refresh available models"
          disabled={discovery.isFetching}
          onClick={() => void discovery.refetch()}
        >
          <RefreshCw
            className={discovery.isFetching ? "animate-spin" : undefined}
          />
        </Button>
      </div>

      {allModels.length > 8 && (
        <div className="relative">
          <Search className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={search}
            onChange={(event) => setSearch(event.target.value)}
            className="pl-8"
            placeholder="Search available models"
            aria-label="Search available models"
          />
        </div>
      )}

      {discovery.isLoading ||
      (discovery.isFetching && allModels.length === 0) ? (
        <p className="text-sm text-muted-foreground">Discovering models…</p>
      ) : discovery.error ? (
        <p className="text-sm text-destructive">
          Model discovery failed: {discovery.error.message}
        </p>
      ) : provider?.error ? (
        <p className="text-sm text-destructive">
          Model discovery failed: {provider.error}
        </p>
      ) : allModels.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          The provider returned no selectable models.
        </p>
      ) : visibleModels.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          No models match “{search.trim()}”.
        </p>
      ) : (
        <ul className="max-h-72 divide-y overflow-y-auto rounded-md border">
          {visibleModels.map((model) => (
            <li key={model.model} className="grid gap-1 px-3 py-2">
              {model.displayName !== model.model && (
                <span className="text-sm font-medium">{model.displayName}</span>
              )}
              <span
                className={
                  model.displayName === model.model
                    ? "break-all font-mono text-sm font-medium"
                    : "break-all font-mono text-xs text-muted-foreground"
                }
              >
                {model.model}
              </span>
              <span className="flex flex-wrap gap-1">
                {model.apiKinds.map((apiKind) => (
                  <Badge
                    key={apiKind}
                    variant="outline"
                    className="text-[10px]"
                  >
                    {formatApiKind(apiKind)}
                  </Badge>
                ))}
                {model.createdAtMs && (
                  <span className="self-center text-[10px] text-muted-foreground">
                    Created {new Date(model.createdAtMs).toLocaleDateString()}
                  </span>
                )}
              </span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function formatApiKind(apiKind: string): string {
  if (apiKind === "openai:completions") return "Chat Completions";
  if (apiKind === "openai:responses") return "Responses";
  return apiKind;
}

function EndpointSummary({ endpoint }: { endpoint: ModelEndpointConfig }) {
  return (
    <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
      <dt className="text-muted-foreground">Base URL</dt>
      <dd className="break-all font-mono">{endpoint.baseUrl}</dd>
      <dt className="text-muted-foreground">API kinds</dt>
      <dd>{endpoint.apiKinds.join(", ")}</dd>
      <dt className="text-muted-foreground">Extra headers</dt>
      <dd>{Object.keys(endpoint.headers ?? {}).join(", ") || "None"}</dd>
    </dl>
  );
}

function parseHeaders(value: string): Record<string, string> {
  const headers: Record<string, string> = {};
  for (const line of value
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean)) {
    const separator = line.indexOf(":");
    if (separator < 1) throw new Error(`invalid header line: ${line}`);
    headers[line.slice(0, separator).trim()] = line.slice(separator + 1).trim();
  }
  return headers;
}
