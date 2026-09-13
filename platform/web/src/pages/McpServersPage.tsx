import { useEffect, useRef, useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { slugify } from "@lightspeed/platform-shared";
import {
  CheckCircle2,
  ChevronDown,
  ExternalLink,
  Loader2,
  LogIn,
  Pencil,
  Plus,
  RotateCcw,
  Search,
  Trash2,
} from "lucide-react";
import {
  api,
  type AuthGrantOption,
  type McpOAuthFlow,
  type McpOAuthFlowStart,
  type McpServer,
  type McpServerAuthDiscovery,
  type McpToolDiscovery,
} from "@/api";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  IdText,
  Table,
  TableActionsCell,
  TableBody,
  TableCard,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  TableTitleCell,
} from "@/components/ui/table";
import { LoadingNote, PageHeader, UniverseNotFound } from "@/components/page";
import { ProgressSteps } from "@/components/ui/progress-steps";
import { canManage, useActiveUniverse } from "@/lib/universes";

const MCP_CREATE_STEPS = [
  { id: 1 as const, label: "Server" },
  { id: 2 as const, label: "Connection" },
];

/// U5a: the universe's MCP server registry — what the profile editor's
/// server picker links against. Full-document saves mirror the engine's
/// put-with-revision catalog semantics.
export function McpServersPage({ admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();

  if (isLoading) {
    return <LoadingNote />;
  }
  if (!universe || !canManage(universe, admin)) {
    return <UniverseNotFound slug={slug} />;
  }

  return <ServerList universeId={universe.id} />;
}

const APPROVALS = ["always", "never"] as const;

function ServerList({ universeId }: { universeId: string }) {
  const queryClient = useQueryClient();
  const servers = useQuery({
    queryKey: ["mcp-servers", universeId],
    queryFn: () => api<McpServer[]>("GET", `/api/v1/universes/${universeId}/mcp-servers`),
  });
  const authGrants = useQuery({
    queryKey: ["auth-grants", universeId],
    queryFn: () =>
      api<AuthGrantOption[]>("GET", `/api/v1/universes/${universeId}/auth-grants`),
  });
  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: ["mcp-servers", universeId] });

  const [createOpen, setCreateOpen] = useState(false);
  const [editing, setEditing] = useState<McpServer | null>(null);
  const [oauthServer, setOAuthServer] = useState<McpServer | null>(null);

  const remove = useMutation({
    mutationFn: (serverId: string) =>
      api("DELETE", `/api/v1/universes/${universeId}/mcp-servers/${serverId}`),
    onSuccess: invalidate,
  });

  const rows = (servers.data ?? [])
    .slice()
    .sort((a, b) => a.serverId.localeCompare(b.serverId));
  const grantLabels = new Map(
    (authGrants.data ?? []).map((grant) => [grant.grantId, authGrantLabel(grant)]),
  );

  return (
    <>
      <PageHeader
        title="MCP servers"
        description="Connect remote tools once, then make them available to profiles and sessions."
        actions={
          <Button onClick={() => setCreateOpen(true)}>
            <Plus data-icon="inline-start" />
            Add server
          </Button>
        }
      />
      {servers.isLoading && <LoadingNote />}
      {servers.error && (
        <p className="text-sm text-destructive">{servers.error.message}</p>
      )}
      {authGrants.error && (
        <p className="text-sm text-destructive">
          Access credentials unavailable: {authGrants.error.message}
        </p>
      )}
      {servers.data && rows.length === 0 && (
        <p className="mb-4 text-sm text-muted-foreground">
          No MCP servers yet — add one, then link it from a profile's MCP section.
        </p>
      )}
      {rows.length > 0 && (
        <TableCard>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Server</TableHead>
                <TableHead>URL</TableHead>
                <TableHead>Authentication</TableHead>
                <TableHead>Status</TableHead>
                <TableHead className="w-0" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map((server) => (
                <TableRow key={server.serverId}>
                  <TableTitleCell
                    title={server.displayName ?? server.serverId}
                    subtitle={server.serverId}
                  />
                  <TableCell className="max-w-64">
                    <IdText className="text-muted-foreground">{server.serverUrl}</IdText>
                  </TableCell>
                  <TableCell className="max-w-56">
                    <div className="grid gap-0.5">
                      <span className="text-xs">{authPolicyLabel(server.authPolicy.type)}</span>
                      {server.credential && (
                        <span
                          className="truncate text-xs text-muted-foreground"
                          title={server.credential.grantId}
                        >
                          Connected · {grantLabels.get(server.credential.grantId) ?? server.credential.grantId}
                        </span>
                      )}
                    </div>
                  </TableCell>
                  <TableCell>
                    <div className="flex items-center gap-2">
                      <StatusBadge status={server.status} />
                      {/* The one thing to do on a row that needs auth is right here, not behind an icon. */}
                      {isOAuthPolicy(server.authPolicy.type) && !server.credential && (
                        <Button variant="outline" size="xs" onClick={() => setOAuthServer(server)}>
                          <LogIn data-icon="inline-start" /> Connect
                        </Button>
                      )}
                    </div>
                  </TableCell>
                  <TableActionsCell>
                    {isOAuthPolicy(server.authPolicy.type) && server.credential && (
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        aria-label={`Reconnect ${server.serverId} with OAuth`}
                        title="Sign in again"
                        onClick={() => setOAuthServer(server)}
                      >
                        <LogIn />
                      </Button>
                    )}
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={`Edit ${server.serverId}`}
                      onClick={() => setEditing(server)}
                    >
                      <Pencil />
                    </Button>
                    <AlertDialog>
                      <AlertDialogTrigger
                        render={
                          <Button
                            variant="ghost"
                            size="icon-sm"
                            className="text-destructive"
                            aria-label={`Delete ${server.serverId}`}
                          />
                        }
                      >
                        <Trash2 />
                      </AlertDialogTrigger>
                      <AlertDialogContent>
                        <AlertDialogHeader>
                          <AlertDialogTitle>
                            Delete {server.displayName ?? server.serverId}?
                          </AlertDialogTitle>
                          <AlertDialogDescription>
                            Profiles referencing{" "}
                            <span className="font-mono text-xs">{server.serverId}</span>{" "}
                            will fail to link it into new sessions.
                          </AlertDialogDescription>
                        </AlertDialogHeader>
                        <AlertDialogFooter>
                          <AlertDialogCancel>Cancel</AlertDialogCancel>
                          <AlertDialogAction
                            className="bg-destructive text-white hover:bg-destructive/90"
                            onClick={() => remove.mutate(server.serverId)}
                          >
                            Delete
                          </AlertDialogAction>
                        </AlertDialogFooter>
                      </AlertDialogContent>
                    </AlertDialog>
                  </TableActionsCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </TableCard>
      )}
      <p className="mt-4 text-sm text-muted-foreground">
        Authentication is configured once on the universe server. Sessions and profiles only
        select its id. OAuth servers discover their authorization metadata and store a brokered
        universe credential after you approve access.
      </p>
      <ServerDialog
        key={createOpen ? "create-open" : "create-closed"}
        universeId={universeId}
        open={createOpen}
        server={null}
        authGrants={authGrants.data ?? []}
        authGrantsLoading={authGrants.isLoading}
        onOpenChange={setCreateOpen}
        onDone={(server, connectOAuth) => {
          invalidate();
          if (connectOAuth) setOAuthServer(server);
        }}
      />
      <ServerDialog
        key={`edit-${editing?.serverId ?? "closed"}`}
        universeId={universeId}
        open={editing !== null}
        server={editing}
        authGrants={authGrants.data ?? []}
        authGrantsLoading={authGrants.isLoading}
        onOpenChange={(open) => {
          if (!open) {
            setEditing(null);
          }
        }}
        onDone={(server, connectOAuth) => {
          invalidate();
          if (connectOAuth) setOAuthServer(server);
        }}
      />
      <OAuthDialog
        key={oauthServer?.serverId ?? "closed"}
        universeId={universeId}
        server={oauthServer}
        onOpenChange={(open) => {
          if (!open) setOAuthServer(null);
        }}
        onDone={() => {
          invalidate();
          queryClient.invalidateQueries({ queryKey: ["auth-grants", universeId] });
        }}
      />
    </>
  );
}

function authPolicyLabel(type: string): string {
  switch (type) {
    case "none":
      return "No authentication";
    case "optionalBearer":
      return "Bearer token (optional)";
    case "requiredBearer":
      return "Bearer token (required)";
    case "optionalOAuth":
      return "OAuth (optional)";
    case "requiredOAuth":
      return "OAuth (required)";
    default:
      return type;
  }
}

function StatusBadge({ status }: { status: McpServer["status"] }) {
  if (status === "active") {
    return <Badge variant="secondary">active</Badge>;
  }
  if (status === "needsAuthConfig") {
    return (
      <Badge
        variant="outline"
        className="border-destructive/50 text-destructive"
        title="Needs auth configuration before sessions can link it — bind a credential here or run lightspeed mcp server login."
      >
        needs auth
      </Badge>
    );
  }
  return <Badge variant="outline">{status}</Badge>;
}

/// One dialog for both modes: `server === null` creates; otherwise edits by
/// replacing the loaded document with its revision as the CAS guard.
type McpAuthKind = "none" | "bearer" | "oauth";

function ServerDialog({
  universeId,
  open,
  server,
  authGrants,
  authGrantsLoading,
  onOpenChange,
  onDone,
}: {
  universeId: string;
  open: boolean;
  server: McpServer | null;
  authGrants: AuthGrantOption[];
  authGrantsLoading: boolean;
  onOpenChange: (open: boolean) => void;
  onDone: (server: McpServer, connectOAuth: boolean) => void;
}) {
  const editing = server !== null;
  const [step, setStep] = useState<1 | 2>(editing ? 2 : 1);
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [displayName, setDisplayName] = useState(server?.displayName ?? "");
  const [serverId, setServerId] = useState(server?.serverId ?? "");
  const [idTouched, setIdTouched] = useState(false);
  const [serverUrl, setServerUrl] = useState(server?.serverUrl ?? "");
  const [execution, setExecution] = useState<McpServer["execution"]>(
    server?.execution ?? "provider",
  );
  const [exposure, setExposure] = useState<McpServer["exposure"]>(
    server?.exposure ?? "inject",
  );
  const [allowPrivateNetwork, setAllowPrivateNetwork] = useState(
    server?.allowPrivateNetwork ?? false,
  );
  const [approval, setApproval] = useState<string>(
    server?.approval ?? "never",
  );
  const [allToolsAllowed, setAllToolsAllowed] = useState(server?.allowedTools == null);
  const [allowedTools, setAllowedTools] = useState<string[]>(server?.allowedTools ?? []);
  const [toolSearch, setToolSearch] = useState("");
  const [toolDiscoveryObservation, setToolDiscoveryObservation] = useState<{
    connectionKey: string;
    result: McpToolDiscovery;
  } | null>(null);
  const [description, setDescription] = useState(server?.description ?? "");
  const [authPolicy, setAuthPolicy] = useState<string>(server?.authPolicy.type ?? "none");
  const [authTouched, setAuthTouched] = useState(Boolean(server));
  const [oauthResource, setOAuthResource] = useState(
    oauthPolicyString(server?.authPolicy, "resource"),
  );
  const [oauthScopes, setOAuthScopes] = useState(
    oauthPolicyScopes(server?.authPolicy).join(", "),
  );
  const [oauthMetadataUrl, setOAuthMetadataUrl] = useState(
    oauthPolicyString(server?.authPolicy, "protectedResourceMetadataUrl"),
  );
  const [oauthAuthorizationServer, setOAuthAuthorizationServer] = useState(
    oauthPolicyString(server?.authPolicy, "authorizationServer"),
  );
  const [credentialGrantId, setCredentialGrantId] = useState(
    server?.credential?.grantId ?? "",
  );
  const [status, setStatus] = useState<McpServer["status"]>(server?.status ?? "active");
  const [discovery, setDiscovery] = useState<McpServerAuthDiscovery | null>(null);
  const [lastProbedUrl, setLastProbedUrl] = useState("");
  const [error, setError] = useState<string | null>(null);

  const authKind = mcpAuthKind(authPolicy);
  const compatibleGrants = authGrants
    .filter((grant) => mcpGrantCompatible(authPolicy, grant.providerKind))
    .slice()
    .sort((left, right) => authGrantLabel(left).localeCompare(authGrantLabel(right)));
  const boundGrantAvailable = compatibleGrants.some(
    (grant) => grant.grantId === credentialGrantId,
  );
  const currentOAuthScopes = oauthScopes.split(",").map((scope) => scope.trim()).filter(Boolean);
  const connectionSettingsDirty = Boolean(server && (
    serverUrl.trim() !== server.serverUrl ||
    authPolicy !== server.authPolicy.type ||
    credentialGrantId !== (server.credential?.grantId ?? "") ||
    status !== server.status ||
    (isOAuthPolicy(authPolicy) && (
      oauthResource.trim() !== oauthPolicyString(server.authPolicy, "resource") ||
      JSON.stringify(currentOAuthScopes) !== JSON.stringify(oauthPolicyScopes(server.authPolicy)) ||
      oauthMetadataUrl.trim() !== oauthPolicyString(server.authPolicy, "protectedResourceMetadataUrl") ||
      oauthAuthorizationServer.trim() !== oauthPolicyString(server.authPolicy, "authorizationServer")
    ))
  ));
  const toolConnectionKey = `${server?.serverId ?? "new"}:${server?.revision ?? 0}`;
  const toolConnectionKeyRef = useRef(toolConnectionKey);
  toolConnectionKeyRef.current = toolConnectionKey;
  const toolDiscovery = !connectionSettingsDirty &&
    toolDiscoveryObservation?.connectionKey === toolConnectionKey
    ? toolDiscoveryObservation.result
    : null;
  const parsedTools = [...new Set(allowedTools.map((tool) => tool.trim()).filter(Boolean))]
    .sort((left, right) => left.localeCompare(right));
  const advertisedTools = toolDiscovery?.status === "success"
    ? toolDiscovery.tools.slice().sort((left, right) => left.name.localeCompare(right.name))
    : [];
  const normalizedToolSearch = toolSearch.trim().toLocaleLowerCase();
  const visibleTools = advertisedTools.filter((tool) =>
    !normalizedToolSearch || `${tool.name} ${tool.title ?? ""} ${tool.description ?? ""}`
      .toLocaleLowerCase()
      .includes(normalizedToolSearch),
  );
  const advertisedNames = new Set(advertisedTools.map((tool) => tool.name));
  const unavailableSelectedTools = parsedTools.filter((name) => !advertisedNames.has(name));

  const probe = useMutation({
    mutationFn: (url: string) => api<McpServerAuthDiscovery>(
      "POST",
      `/api/v1/universes/${universeId}/mcp-servers/discover-auth`,
      { serverUrl: url },
    ),
    onSuccess: (result, url) => {
      setDiscovery(result);
      setLastProbedUrl(url);
      if (result.oauth && !authTouched) {
        setAuthPolicy("requiredOAuth");
        setOAuthResource(result.oauth.resource);
      }
    },
    onError: (_probeError, url) => {
      setDiscovery(null);
      setLastProbedUrl(url);
    },
  });

  const discoverTools = useMutation({
    mutationFn: (_connectionKey: string) => api<McpToolDiscovery>(
      "POST",
      `/api/v1/universes/${universeId}/mcp-servers/${server!.serverId}/tools/discover`,
    ),
    onSuccess: (result, connectionKey) => {
      if (connectionKey === toolConnectionKeyRef.current) {
        setToolDiscoveryObservation({ connectionKey, result });
      }
    },
    onError: (_error, connectionKey) => {
      if (connectionKey === toolConnectionKeyRef.current) {
        setToolDiscoveryObservation(null);
      }
    },
  });

  const toggleAllowedTool = (name: string, checked: boolean) => {
    setAllowedTools((current) => checked
      ? [...new Set([...current, name])]
      : current.filter((tool) => tool !== name));
  };

  const discoverAuth = async () => {
    const url = serverUrl.trim();
    if (editing || !isValidMcpUrl(url) || url === lastProbedUrl || probe.isPending) return;
    try {
      await probe.mutateAsync(url);
    } catch {
      // Detection is advisory. Step two always keeps the manual choices.
    }
  };

  const save = useMutation({
    mutationFn: () => {
      const credential = authPolicy === "none" || !credentialGrantId
        ? null
        : { type: "authGrant" as const, grantId: credentialGrantId };
      const nextStatus = mcpServerStatusForCredential(
        authPolicy,
        editing ? status : "active",
        credentialGrantId,
      );
      const policy = mcpAuthPolicyInput({
        type: authPolicy,
        serverUrl,
        resource: oauthResource,
        scopes: oauthScopes,
        metadataUrl: oauthMetadataUrl,
        authorizationServer: oauthAuthorizationServer,
      });
      if (!editing) {
        return api<McpServer>("POST", `/api/v1/universes/${universeId}/mcp-servers`, {
          serverId,
          serverUrl,
          defaultServerLabel: serverId,
          execution,
          exposure: execution === "native" ? exposure : "inject",
          approval: approval,
          allowPrivateNetwork,
          authPolicy: policy,
          credential,
          status: nextStatus,
          displayName: displayName.trim(),
          ...(description.trim() ? { description: description.trim() } : {}),
          allowedTools: null,
        });
      }
      return api<McpServer>(
        "PUT",
        `/api/v1/universes/${universeId}/mcp-servers/${server.serverId}`,
        {
          serverId: server.serverId,
          serverUrl,
          defaultServerLabel: server.defaultServerLabel,
          execution,
          exposure: execution === "native" ? exposure : "inject",
          revision: server.revision,
          approval: approval,
          authPolicy: policy,
          credential,
          status: nextStatus,
          displayName: displayName.trim() || null,
          description: description.trim() || null,
          allowedTools: allToolsAllowed ? null : parsedTools,
          deferLoading: server.deferLoading ?? null,
          allowPrivateNetwork,
        },
      );
    },
    onSuccess: (savedServer) => {
      const connectOAuth = isOAuthPolicy(savedServer.authPolicy.type) &&
        !savedServer.credential;
      onOpenChange(false);
      onDone(savedServer, connectOAuth);
    },
    onError: (saveError) => setError(saveError.message),
  });

  const continueToConnection = async () => {
    if (!displayName.trim()) {
      setError("Give this server a name.");
      return;
    }
    if (!serverId.trim()) {
      setError("The server needs an ID.");
      return;
    }
    if (!isValidMcpUrl(serverUrl.trim())) {
      setError("Enter a valid http:// or https:// MCP server URL.");
      return;
    }
    setError(null);
    await discoverAuth();
    setStep(2);
  };

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!editing && step === 1) {
      void continueToConnection();
      return;
    }
    const credentialError = mcpServerCredentialError(authPolicy, credentialGrantId);
    if (credentialError) {
      setError(credentialError);
      return;
    }
    if (editing && !allToolsAllowed && parsedTools.length === 0) {
      setError("Select at least one tool, or allow every advertised tool.");
      return;
    }
    save.mutate();
  };

  const chooseAuth = (kind: McpAuthKind) => {
    setAuthTouched(true);
    setCredentialGrantId("");
    setAuthPolicy(kind === "oauth" ? "requiredOAuth" : kind === "bearer" ? "requiredBearer" : "none");
    if (kind === "oauth" && !oauthResource) {
      setOAuthResource(discovery?.oauth?.resource ?? serverUrl.trim());
    }
  };

  const discoveryCurrent = lastProbedUrl === serverUrl.trim();
  const detectedOAuth = discoveryCurrent ? discovery?.oauth : null;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className={`max-h-[min(92dvh,860px)] gap-0 p-0 sm:max-w-xl ${editing ? "grid-rows-[auto_minmax(0,1fr)_auto]" : "grid-rows-[auto_auto_minmax(0,1fr)_auto]"}`}
      >
        <DialogHeader className="border-b p-6 pr-14">
          <DialogTitle>{editing ? `Edit ${server.displayName || server.serverId}` : "Add MCP server"}</DialogTitle>
          <DialogDescription>
            {editing
              ? "Update how Lightspeed connects to this server."
              : step === 1
                ? "Start with the server address. Lightspeed will check how it expects you to connect."
                : "Confirm the connection. Everything else is optional."}
          </DialogDescription>
        </DialogHeader>

        {!editing && (
          <ProgressSteps
            steps={MCP_CREATE_STEPS}
            current={step}
            onSelect={setStep}
            label="MCP server creation progress"
          />
        )}

        <form onSubmit={submit} className="contents">
          <div className="grid min-h-0 content-start gap-5 overflow-y-auto p-6">
          {(!editing && step === 1) ? (
            <>
              <Field>
                <FieldLabel htmlFor="mcp-name">Name</FieldLabel>
                <Input
                  id="mcp-name"
                  value={displayName}
                  onChange={(event) => {
                    setDisplayName(event.target.value);
                    if (!idTouched) setServerId(slugify(event.target.value));
                    setError(null);
                  }}
                  placeholder="GitHub"
                  autoFocus
                />
                <FieldDescription>
                  {serverId ? (
                    <>
                      Profiles reference it as <code className="font-mono">{serverId}</code>
                      {idTouched ? "" : " — change it under Advanced if you need to"}.
                    </>
                  ) : (
                    "Profiles reference the server by an id derived from this name."
                  )}
                </FieldDescription>
              </Field>
              <Field>
                <FieldLabel htmlFor="mcp-url">Server URL</FieldLabel>
                <Input
                  id="mcp-url"
                  value={serverUrl}
                  onChange={(event) => {
                    setServerUrl(event.target.value);
                    setDiscovery(null);
                    setLastProbedUrl("");
                    if (!authTouched) {
                      setAuthPolicy("none");
                      setOAuthResource("");
                    }
                    setError(null);
                  }}
                  onBlur={() => void discoverAuth()}
                  placeholder="https://mcp.example.com/mcp"
                  className="font-mono"
                />
                <AuthDiscoveryNote
                  pending={probe.isPending}
                  checked={discoveryCurrent}
                  oauth={detectedOAuth}
                  error={probe.error?.message}
                />
              </Field>
            </>
          ) : (
            <>
              {!editing && (
                <div className="flex items-start justify-between gap-4 rounded-lg border bg-muted/15 p-3">
                  <div className="min-w-0">
                    <p className="text-sm font-medium">{displayName}</p>
                    <p className="truncate font-mono text-xs text-muted-foreground">{serverUrl}</p>
                  </div>
                  <Button type="button" variant="ghost" size="sm" onClick={() => setStep(1)}>
                    Change
                  </Button>
                </div>
              )}

              {editing && (
                <div className="grid gap-4 sm:grid-cols-2">
                  <Field>
                    <FieldLabel htmlFor="mcp-name">Name</FieldLabel>
                    <Input
                      id="mcp-name"
                      value={displayName}
                      onChange={(event) => setDisplayName(event.target.value)}
                    />
                  </Field>
                  <Field>
                    <FieldLabel htmlFor="mcp-url">Server URL</FieldLabel>
                    <Input
                      id="mcp-url"
                      value={serverUrl}
                      onChange={(event) => setServerUrl(event.target.value)}
                      className="font-mono"
                    />
                  </Field>
                </div>
              )}

              <Field>
                <FieldLabel>Execution</FieldLabel>
                <Select
                  value={execution}
                  onValueChange={(value) => {
                    setExecution(value as McpServer["execution"]);
                    if (value === "provider") setExposure("inject");
                  }}
                >
                  <SelectTrigger className="w-full" aria-label="MCP execution">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="provider">Model provider connects directly</SelectItem>
                    <SelectItem value="native">Lightspeed connects</SelectItem>
                  </SelectContent>
                </Select>
                <FieldDescription>
                  {execution === "native"
                    ? "Use this for private servers and models without provider-hosted MCP. Credentials and tool traffic stay between Lightspeed and the server."
                    : "The model provider lists and calls the server directly. The endpoint must be publicly reachable."}
                </FieldDescription>
              </Field>

              {execution === "native" && (
                <Field>
                  <FieldLabel>Tool exposure</FieldLabel>
                  <Select
                    value={exposure}
                    onValueChange={(value) => setExposure(value as McpServer["exposure"])}
                  >
                    <SelectTrigger className="w-full" aria-label="MCP tool exposure">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="inject">Show tools to the model up front</SelectItem>
                      <SelectItem value="search">Let the model search on demand</SelectItem>
                    </SelectContent>
                  </Select>
                  <FieldDescription>
                    {exposure === "search"
                      ? "Recommended for large or frequently changing servers. Only two stable search and call tools are shown up front."
                      : "Best for a small, stable inventory or a selected allowlist. The live tools are injected as ordinary function tools."}
                  </FieldDescription>
                </Field>
              )}

              {editing ? (
                <Field>
                  <div className="flex items-end justify-between gap-3">
                    <div>
                      <FieldLabel>Available tools</FieldLabel>
                      <FieldDescription>
                        Read live with the connected account's permissions and never cached.
                        Server-provided descriptions and safety annotations are untrusted hints.
                      </FieldDescription>
                    </div>
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      disabled={discoverTools.isPending || connectionSettingsDirty}
                      onClick={() => discoverTools.mutate(toolConnectionKey)}
                    >
                      <RotateCcw className={discoverTools.isPending ? "animate-spin" : ""} />
                      {toolDiscovery ? "Refresh" : "Load tools"}
                    </Button>
                  </div>
                <Select
                  value={allToolsAllowed ? "all" : "selected"}
                  onValueChange={(value) => setAllToolsAllowed(value === "all")}
                >
                  <SelectTrigger className="w-full" aria-label="Allowed tools">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="all">Allow every advertised tool</SelectItem>
                    <SelectItem value="selected">Allow only selected tools</SelectItem>
                  </SelectContent>
                </Select>
                {connectionSettingsDirty && (
                  <FieldDescription>
                    Save connection or credential changes before loading its tools.
                  </FieldDescription>
                )}
                {discoverTools.error && (
                  <p className="text-sm text-destructive">{discoverTools.error.message}</p>
                )}
                {toolDiscovery?.status === "failure" && (
                  <div className="grid gap-1">
                    <p className="text-sm text-destructive">{toolDiscovery.message}</p>
                    <FieldDescription>
                      {mcpDiscoveryFailureAction(toolDiscovery.code)}
                      {toolDiscovery.requiredScopes?.length
                        ? ` Required scopes: ${toolDiscovery.requiredScopes.join(", ")}.`
                        : ""}
                    </FieldDescription>
                  </div>
                )}
                {!allToolsAllowed && toolDiscovery?.status !== "success" && parsedTools.length > 0 && (
                  <div className="rounded-md border p-3">
                    <p className="mb-2 text-xs font-medium text-muted-foreground">Authored selection</p>
                    <div className="flex flex-wrap gap-1.5">
                      {parsedTools.map((name) => (
                        <Badge key={name} variant="outline" className="font-mono">{name}</Badge>
                      ))}
                    </div>
                  </div>
                )}
                {toolDiscovery?.status === "success" && (
                  <div className="grid gap-2">
                    <div className="relative">
                      <Search className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
                      <Input
                        value={toolSearch}
                        onChange={(event) => setToolSearch(event.target.value)}
                        placeholder={`Search ${advertisedTools.length} tool${advertisedTools.length === 1 ? "" : "s"}`}
                        aria-label="Search MCP tools"
                        className="pl-8"
                      />
                    </div>
                    <div className="max-h-64 overflow-y-auto rounded-md border">
                      {visibleTools.length === 0 ? (
                        <p className="p-3 text-sm text-muted-foreground">
                          {advertisedTools.length === 0
                            ? "No tools advertised. Check this account's access, requested scopes, and workspace or admin policy, then refresh or reconnect this server."
                            : "No tools match your search."}
                        </p>
                      ) : visibleTools.map((tool) => (
                        <Label
                          key={tool.name}
                          className="flex items-start gap-3 border-b p-3 font-normal last:border-b-0"
                        >
                          {!allToolsAllowed && (
                            <Checkbox
                              className="mt-0.5"
                              checked={allowedTools.includes(tool.name)}
                              onCheckedChange={(checked) => toggleAllowedTool(tool.name, checked === true)}
                            />
                          )}
                          <span className="min-w-0 flex-1">
                            <span className="flex flex-wrap items-center gap-1.5 text-sm font-medium">
                              {tool.title ?? tool.name}
                              {tool.annotations?.readOnlyHint === true && <Badge variant="outline">read only</Badge>}
                              {tool.annotations?.readOnlyHint === false && <Badge variant="outline">may write</Badge>}
                              {tool.annotations?.destructiveHint === true && <Badge variant="outline">destructive</Badge>}
                              {tool.annotations?.idempotentHint === true && <Badge variant="outline">idempotent</Badge>}
                              {tool.annotations?.openWorldHint === true && <Badge variant="outline">external access</Badge>}
                            </span>
                            <span className="block font-mono text-xs text-muted-foreground">{tool.name}</span>
                            {tool.description && (
                              <span className="mt-1 block text-xs text-muted-foreground">{tool.description}</span>
                            )}
                          </span>
                        </Label>
                      ))}
                    </div>
                    {!allToolsAllowed && unavailableSelectedTools.length > 0 && (
                      <FieldDescription>
                        Still selected but not currently advertised: {unavailableSelectedTools.join(", ")}.
                        They are preserved until you deselect them.
                      </FieldDescription>
                    )}
                  </div>
                )}
                </Field>
              ) : (
                <div className="grid gap-1 rounded-md border bg-muted/15 p-3">
                  <p className="text-sm font-medium">Tool selection after connection</p>
                  <p className="text-xs text-muted-foreground">
                    This server will initially allow every advertised tool. After adding it and completing any
                    authentication, edit the server to load its live inventory and restrict access.
                  </p>
                </div>
              )}

              <Field>
                <FieldLabel>Authentication</FieldLabel>
                {detectedOAuth && (
                  <div className="mb-2 flex items-center gap-2 rounded-md border border-emerald-500/30 bg-emerald-500/5 px-3 py-2 text-sm text-emerald-700 dark:text-emerald-300">
                    <CheckCircle2 className="size-4" />
                    OAuth sign-in detected from the server
                  </div>
                )}
                <Select value={authKind} onValueChange={(value) => chooseAuth(value as McpAuthKind)}>
                  <SelectTrigger className="w-full" aria-label="Authentication">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="none">No authentication</SelectItem>
                    <SelectItem value="bearer">Bearer token</SelectItem>
                    <SelectItem value="oauth">OAuth sign-in</SelectItem>
                  </SelectContent>
                </Select>
                <FieldDescription>
                  {authKind === "oauth"
                    ? credentialGrantId
                      ? "An existing OAuth connection is selected."
                      : "After saving, Lightspeed will open the provider sign-in and finish setup automatically."
                    : authKind === "bearer"
                      ? "Choose a universe credential to send as a bearer token."
                      : detectedOAuth
                        ? "The server advertises OAuth, so unauthenticated access may fail."
                        : "Use this for public MCP servers."}
                </FieldDescription>
              </Field>

              {authKind === "bearer" && (
                <CredentialSelect
                  grants={compatibleGrants}
                  loading={authGrantsLoading}
                  value={credentialGrantId}
                  boundAvailable={boundGrantAvailable}
                  onChange={setCredentialGrantId}
                  emptyCopy="Create a bearer credential on the Secrets page, then return here."
                />
              )}

              <Collapsible open={advancedOpen} onOpenChange={setAdvancedOpen}>
                <CollapsibleTrigger className="flex w-full items-center justify-between rounded-md border px-3 py-2 text-left text-sm font-medium outline-none hover:bg-muted/40 focus-visible:ring-3 focus-visible:ring-ring/50">
                  <span className="min-w-0">
                    Advanced options
                    <span className="ml-2 text-xs font-normal text-muted-foreground">
                      {[
                        ...(!editing ? [`id ${serverId || "…"}`] : []),
                        approvalLabel(approval).toLowerCase(),
                        allowPrivateNetwork ? "private-network access" : "public network only",
                      ].join(" · ")}
                    </span>
                  </span>
                  <ChevronDown className={`size-4 transition-transform ${advancedOpen ? "rotate-180" : ""}`} />
                </CollapsibleTrigger>
                <CollapsibleContent className="grid gap-4 border-x border-b p-4">
                  {!editing && (
                    <Field>
                      <FieldLabel htmlFor="mcp-id">Server ID</FieldLabel>
                      <Input
                        id="mcp-id"
                        value={serverId}
                        onChange={(event) => {
                          setServerId(event.target.value);
                          setIdTouched(true);
                        }}
                        className="font-mono"
                      />
                      <FieldDescription>Stable identifier used by profiles and sessions.</FieldDescription>
                    </Field>
                  )}
                  <Field>
                    <FieldLabel htmlFor="mcp-description">Description</FieldLabel>
                    <Input
                      id="mcp-description"
                      value={description}
                      onChange={(event) => setDescription(event.target.value)}
                      placeholder="What this server offers"
                    />
                  </Field>
                  <Field>
                    <FieldLabel>Tool approval</FieldLabel>
                    <Select value={approval} onValueChange={(value) => setApproval(value as string)}>
                      <SelectTrigger className="w-full" aria-label="Tool approval">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        {APPROVALS.map((value) => (
                          <SelectItem key={value} value={value}>{approvalLabel(value)}</SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </Field>
                  <Label className="flex items-start gap-3 rounded-md border p-3 font-normal">
                    <Checkbox
                      checked={allowPrivateNetwork}
                      onCheckedChange={(checked) => setAllowPrivateNetwork(checked === true)}
                    />
                    <span>
                      <span className="block text-sm font-medium">Allow private-network egress</span>
                      <span className="block text-xs text-muted-foreground">
                        Enables live discovery and native execution only for hosts or CIDRs in the deployment-level MCP private-network allowlist.
                      </span>
                    </span>
                  </Label>
                  {authKind === "oauth" && (
                    <>
                      <CredentialSelect
                        grants={compatibleGrants}
                        loading={authGrantsLoading}
                        value={credentialGrantId}
                        boundAvailable={boundGrantAvailable}
                        onChange={setCredentialGrantId}
                        emptyCopy="Leave blank to start a new OAuth sign-in after saving."
                        optional
                      />
                      <Field>
                        <FieldLabel htmlFor="mcp-oauth-resource">OAuth resource</FieldLabel>
                        <Input
                          id="mcp-oauth-resource"
                          value={oauthResource}
                          onChange={(event) => setOAuthResource(event.target.value)}
                          placeholder={serverUrl}
                          className="font-mono"
                        />
                      </Field>
                      <Field>
                        <FieldLabel htmlFor="mcp-oauth-scopes">Requested scopes</FieldLabel>
                        <Input
                          id="mcp-oauth-scopes"
                          value={oauthScopes}
                          onChange={(event) => setOAuthScopes(event.target.value)}
                          placeholder="Use server defaults"
                        />
                        {discovery?.oauth?.scopesSupported.length ? (
                          <FieldDescription>
                            Server advertises: {discovery.oauth.scopesSupported.join(", ")}. Add
                            scopes deliberately; discovery never expands consent.
                          </FieldDescription>
                        ) : null}
                      </Field>
                      <div className="grid gap-4 sm:grid-cols-2">
                        <Field>
                          <FieldLabel htmlFor="mcp-oauth-metadata">Resource metadata URL</FieldLabel>
                          <Input
                            id="mcp-oauth-metadata"
                            value={oauthMetadataUrl}
                            onChange={(event) => setOAuthMetadataUrl(event.target.value)}
                            placeholder="Discover automatically"
                            className="font-mono"
                          />
                        </Field>
                        <Field>
                          <FieldLabel htmlFor="mcp-oauth-issuer">Authorization server</FieldLabel>
                          <Input
                            id="mcp-oauth-issuer"
                            value={oauthAuthorizationServer}
                            onChange={(event) => setOAuthAuthorizationServer(event.target.value)}
                            placeholder="Discover automatically"
                            className="font-mono"
                          />
                        </Field>
                      </div>
                    </>
                  )}

                </CollapsibleContent>
              </Collapsible>

              {editing && (
                <div className="flex items-center justify-between gap-3 rounded-md border p-3">
                  <Label htmlFor="mcp-enabled" className="text-sm">
                    Enabled
                    <span className="block text-xs font-normal text-muted-foreground">
                      {status === "needsAuthConfig"
                        ? "Needs a credential before sessions can link it; it activates once one is connected."
                        : status === "unverified"
                          ? "Unverified: enable it once you have confirmed the connection."
                          : "Disabled servers stay configured but cannot be linked into new sessions."}
                    </span>
                  </Label>
                  <Switch
                    id="mcp-enabled"
                    checked={status === "active"}
                    onCheckedChange={(checked) => setStatus(checked ? "active" : "disabled")}
                  />
                </div>
              )}
            </>
          )}
          </div>

          <div className="grid gap-2 border-t p-4">
          {error && <p className="text-sm text-destructive">{error}</p>}
          <DialogFooter>
            {!editing && step === 2 && (
              <Button type="button" variant="outline" onClick={() => setStep(1)}>Back</Button>
            )}
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={save.isPending || probe.isPending}>
              {save.isPending
                ? "Saving…"
                : !editing && step === 1
                  ? probe.isPending ? "Checking…" : "Continue"
                  : !credentialGrantId && authKind === "oauth"
                    ? editing ? "Save and connect" : "Add and connect"
                    : editing ? "Save" : "Add server"}
            </Button>
          </DialogFooter>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function AuthDiscoveryNote({
  pending,
  checked,
  oauth,
  error,
}: {
  pending: boolean;
  checked: boolean;
  oauth: McpServerAuthDiscovery["oauth"];
  error?: string;
}) {
  if (pending) {
    return (
      <FieldDescription className="flex items-center gap-1.5">
        <Loader2 className="size-3.5 animate-spin" /> Checking for OAuth sign-in…
      </FieldDescription>
    );
  }
  if (checked && oauth) {
    return (
      <FieldDescription className="flex items-center gap-1.5 text-emerald-700 dark:text-emerald-300">
        <CheckCircle2 className="size-3.5" /> OAuth sign-in detected
      </FieldDescription>
    );
  }
  if (checked) {
    return (
      <FieldDescription>
        {error
          ? "Automatic detection was unavailable. You can choose authentication on the next step."
          : "No standard OAuth metadata was found. You can choose authentication on the next step."}
      </FieldDescription>
    );
  }
  return <FieldDescription>Lightspeed will check the server without saving it.</FieldDescription>;
}

function CredentialSelect({
  grants,
  loading,
  value,
  boundAvailable,
  onChange,
  emptyCopy,
  optional = false,
}: {
  grants: AuthGrantOption[];
  loading: boolean;
  value: string;
  boundAvailable: boolean;
  onChange: (value: string) => void;
  emptyCopy: string;
  optional?: boolean;
}) {
  return (
    <Field>
      <FieldLabel>Credential</FieldLabel>
      <Select
        value={value || "none"}
        onValueChange={(next) => onChange(next === "none" ? "" : next as string)}
      >
        <SelectTrigger className="w-full" aria-label="Credential">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="none">{optional ? "Start a new sign-in" : "Choose a credential"}</SelectItem>
          {value && !boundAvailable && (
            <SelectItem value={value}>{value} (unavailable)</SelectItem>
          )}
          {grants.map((grant) => (
            <SelectItem key={grant.grantId} value={grant.grantId}>
              {authGrantLabel(grant)}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      <FieldDescription>
        {loading
          ? "Loading credentials…"
          : grants.length === 0 && !value
            ? emptyCopy
            : optional
              ? "Reuse a compatible connection, or start a new sign-in."
              : "This credential will be used whenever the server is selected."}
      </FieldDescription>
    </Field>
  );
}

function OAuthDialog({
  universeId,
  server,
  onOpenChange,
  onDone,
}: {
  universeId: string;
  server: McpServer | null;
  onOpenChange: (open: boolean) => void;
  onDone: () => void;
}) {
  const [attempt, setAttempt] = useState<McpOAuthFlowStart | null>(null);
  const [authorizationOpened, setAuthorizationOpened] = useState(false);
  const started = useRef(false);
  const finishStarted = useRef<string | null>(null);

  const start = useMutation({
    mutationFn: () => api<McpOAuthFlowStart>(
      "POST",
      `/api/v1/universes/${universeId}/mcp-servers/${server!.serverId}/oauth/start`,
      {},
    ),
    onSuccess: (nextAttempt) => setAttempt(nextAttempt),
  });
  const flow = useQuery({
    queryKey: ["mcp-oauth-flow", universeId, server?.serverId, attempt?.flowId],
    enabled: Boolean(server && attempt),
    queryFn: () => api<McpOAuthFlow>(
      "GET",
      `/api/v1/universes/${universeId}/mcp-servers/${server!.serverId}/oauth/flows/${attempt!.flowId}`,
    ),
    refetchInterval: (query) => query.state.data?.status === "pending" ? 1_500 : false,
  });
  const complete = useMutation({
    mutationFn: (flowId: string) => api<McpServer>(
      "POST",
      `/api/v1/universes/${universeId}/mcp-servers/${server!.serverId}/oauth/flows/${flowId}/complete`,
      { expectedRevision: attempt!.serverRevision },
    ),
    onSuccess: onDone,
  });

  useEffect(() => {
    if (server && !started.current) {
      started.current = true;
      start.mutate();
    }
  }, [server?.serverId]);

  useEffect(() => {
    const current = flow.data;
    if (
      current?.status === "completed" &&
      current.grantId &&
      finishStarted.current !== current.flowId
    ) {
      finishStarted.current = current.flowId;
      complete.mutate(current.flowId);
    }
  }, [flow.data?.status, flow.data?.flowId, flow.data?.grantId]);

  const retry = () => {
    finishStarted.current = null;
    setAttempt(null);
    setAuthorizationOpened(false);
    start.reset();
    complete.reset();
    start.mutate();
  };

  const terminalError = flow.data?.status === "failed"
    ? flow.data.error || "The provider refused the sign-in."
    : flow.data?.status === "expired"
      ? "This sign-in attempt expired — start it again."
      : null;

  return (
    <Dialog open={server !== null} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Connect {server?.displayName ?? server?.serverId} with OAuth</DialogTitle>
          <DialogDescription>
            Sign in with the provider once; Lightspeed keeps the connection for this universe and
            every session that uses the server.
          </DialogDescription>
        </DialogHeader>

        {(start.isPending || (attempt && flow.isLoading)) && (
          <p className="flex items-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" /> Preparing the sign-in…
          </p>
        )}
        {attempt && flow.data?.status === "pending" && (
          <div className="grid gap-3 rounded-lg border p-4">
            <p className="text-sm">
              Sign in with the provider in the tab that opens and approve access, then come back —
              this dialog finishes on its own.
            </p>
            <Button
              type="button"
              className="w-fit"
              onClick={() => {
                window.open(attempt.authorizeUrl, "_blank", "noopener,noreferrer");
                setAuthorizationOpened(true);
              }}
            >
              <ExternalLink data-icon="inline-start" />
              {authorizationOpened ? "Open the sign-in again" : "Open the sign-in"}
            </Button>
            <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
              <Loader2 className="size-3 animate-spin" /> Waiting for you to approve access…
            </p>
          </div>
        )}
        {flow.data?.status === "completed" && complete.isPending && (
          <p className="flex items-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="size-4 animate-spin" /> Access approved — connecting the server…
          </p>
        )}
        {complete.data && (
          <div className="grid gap-1 rounded-lg border border-emerald-500/40 bg-emerald-500/5 p-4">
            <p className="text-sm font-medium">Connected</p>
            <p className="text-sm text-muted-foreground">
              {complete.data.displayName || complete.data.serverId} is signed in; profiles that link it
              get its tools in their next sessions.
            </p>
          </div>
        )}
        {(start.error || flow.error || complete.error || terminalError) && (
          <div className="grid gap-3">
            <p className="text-sm text-destructive">
              {terminalError || start.error?.message || flow.error?.message || complete.error?.message}
            </p>
            <Button type="button" variant="outline" className="w-fit" onClick={retry}>
              <RotateCcw data-icon="inline-start" />
              Try again
            </Button>
          </div>
        )}
        <DialogFooter>
          <Button type="button" variant={complete.data ? "default" : "outline"} onClick={() => onOpenChange(false)}>
            {complete.data ? "Done" : "Close"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function isOAuthPolicy(authPolicy: string): boolean {
  return authPolicy === "optionalOAuth" || authPolicy === "requiredOAuth";
}

export function mcpAuthKind(authPolicy: string): McpAuthKind {
  if (isOAuthPolicy(authPolicy)) return "oauth";
  if (authPolicy === "optionalBearer" || authPolicy === "requiredBearer") return "bearer";
  return "none";
}

export function isValidMcpUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return (url.protocol === "http:" || url.protocol === "https:") &&
      Boolean(url.hostname) && !url.username && !url.password && !url.hash;
  } catch {
    return false;
  }
}

function approvalLabel(value: string): string {
  if (value === "always") return "Always require approval";
  return "Never require approval";
}

export function mcpAuthPolicyInput({
  type,
  serverUrl,
  resource,
  scopes,
  metadataUrl,
  authorizationServer,
}: {
  type: string;
  serverUrl: string;
  resource: string;
  scopes: string;
  metadataUrl: string;
  authorizationServer: string;
}): McpServer["authPolicy"] {
  if (!isOAuthPolicy(type)) return { type };
  const scopesDefault = Array.from(new Set(
    scopes
      .split(",")
      .map((scope) => scope.trim())
      .filter(Boolean),
  ));
  return {
    type,
    resource: resource.trim() || serverUrl.trim(),
    ...(scopesDefault.length > 0 ? { scopesDefault } : {}),
    ...(metadataUrl.trim()
      ? { protectedResourceMetadataUrl: metadataUrl.trim() }
      : {}),
    ...(authorizationServer.trim()
      ? { authorizationServer: authorizationServer.trim() }
      : {}),
  };
}

function oauthPolicyString(
  policy: McpServer["authPolicy"] | undefined,
  field: string,
): string {
  const value = policy?.[field];
  return typeof value === "string" ? value : "";
}

function oauthPolicyScopes(policy: McpServer["authPolicy"] | undefined): string[] {
  const value = policy?.scopesDefault;
  return Array.isArray(value) ? value.filter((scope): scope is string => typeof scope === "string") : [];
}

export function mcpGrantCompatible(authPolicy: string, providerKind: string): boolean {
  if (authPolicy === "optionalBearer" || authPolicy === "requiredBearer") {
    return providerKind === "staticBearer";
  }
  if (authPolicy === "optionalOAuth" || authPolicy === "requiredOAuth") {
    return providerKind === "mcpOAuth";
  }
  return false;
}

export function mcpServerCredentialError(
  authPolicy: string,
  grantId: string,
): string | null {
  if (authPolicy === "none" && grantId) {
    return "A server with no authentication policy cannot have an access credential.";
  }
  return null;
}

export function mcpServerStatusForCredential(
  authPolicy: string,
  status: McpServer["status"],
  grantId: string,
): McpServer["status"] {
  const required = authPolicy === "requiredBearer" || authPolicy === "requiredOAuth";
  if (grantId && status === "needsAuthConfig") return "active";
  if (!grantId && required) return "needsAuthConfig";
  if (!required && status === "needsAuthConfig") return "active";
  return status;
}

export function mcpDiscoveryFailureAction(
  code: Extract<McpToolDiscovery, { status: "failure" }>["code"],
): string {
  switch (code) {
    case "credentialAbsent":
      return "Connect a credential to this server, then try again.";
    case "grantNeedsReauth":
    case "unauthorized":
      return "Reconnect this server to refresh its access.";
    case "grantAudienceMismatch":
      return "Use a credential issued for this exact server address.";
    case "forbidden":
      return "Check the account's scopes and workspace or administrator policy.";
    case "additionalConsentRequired":
      return "Reconnect this server and explicitly approve the additional scopes.";
    case "remoteRateLimited":
      return "Wait briefly before refreshing again.";
    case "unreachable":
      return "Check the server address, network reachability, and TLS setup.";
    case "unsupportedProtocol":
    case "invalidResponse":
      return "Check that this address is a current Streamable HTTP MCP endpoint.";
    case "paginationLimit":
    case "responseTooLarge":
      return "The server's inventory exceeded safe discovery limits; narrow or fix the server response.";
    case "remoteFailure":
      return "Check the server or provider status, then retry.";
  }
}

function authGrantLabel(grant: AuthGrantOption): string {
  const name = grant.displayName || grant.subjectHint || grant.grantId;
  return `${name} (${grant.providerId})`;
}
