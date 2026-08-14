import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { slugify } from "@lightspeed/platform-shared";
import { Pencil, Plus, Trash2 } from "lucide-react";
import { api, type AuthGrantOption, type McpServer } from "@/api";
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
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { LoadingNote, PageHeader, UniverseNotFound } from "@/components/page";
import { canManage, useActiveUniverse } from "@/lib/universes";

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

const TRANSPORTS = ["auto", "streamableHttp", "sse"] as const;
const APPROVALS = ["providerDefault", "always", "never"] as const;
/// The form covers none/bearer; OAuth policies tie into the engine's auth
/// broker and are managed via the lightspeed CLI for now.
const FORM_AUTH_POLICIES = ["none", "optionalBearer", "requiredBearer"] as const;

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

  const remove = useMutation({
    mutationFn: (serverId: string) =>
      api("DELETE", `/api/v1/universes/${universeId}/mcp-servers/${serverId}`),
    onSuccess: invalidate,
  });

  const rows = (servers.data ?? [])
    .slice()
    .sort((a, b) => a.serverId.localeCompare(b.serverId));

  return (
    <>
      <PageHeader
        title="MCP servers"
        description="Remote tool servers sessions can link — profiles reference them by id."
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
        <div className="overflow-x-auto rounded-xl border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead>Id</TableHead>
                <TableHead>URL</TableHead>
                <TableHead>Transport</TableHead>
                <TableHead>Authentication</TableHead>
                <TableHead>Approval</TableHead>
                <TableHead>Status</TableHead>
                <TableHead className="w-0" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map((server) => (
                <TableRow key={server.serverId}>
                  <TableCell className="font-medium">
                    {server.displayName ?? server.serverId}
                  </TableCell>
                  <TableCell className="font-mono text-xs">{server.serverId}</TableCell>
                  <TableCell className="max-w-64 truncate text-muted-foreground">
                    {server.serverUrl}
                  </TableCell>
                  <TableCell className="text-muted-foreground">
                    {server.transport}
                  </TableCell>
                  <TableCell>
                    <div className="grid gap-0.5">
                      <span className="font-mono text-xs">{server.authPolicy.type}</span>
                      {server.credential && (
                        <span className="max-w-48 truncate font-mono text-xs text-muted-foreground">
                          {server.credential.grantId}
                        </span>
                      )}
                    </div>
                  </TableCell>
                  <TableCell className="text-muted-foreground">
                    {server.approvalDefault}
                  </TableCell>
                  <TableCell>
                    <StatusBadge status={server.status} />
                  </TableCell>
                  <TableCell className="whitespace-nowrap">
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
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
      <p className="mt-4 text-sm text-muted-foreground">
        Authentication is configured once on the universe server. Sessions and profiles only
        select its id. OAuth login remains available through{" "}
        <code className="font-mono text-xs">lightspeed mcp server login &lt;server-id&gt;</code>.
      </p>
      <ServerDialog
        universeId={universeId}
        open={createOpen}
        server={null}
        authGrants={authGrants.data ?? []}
        authGrantsLoading={authGrants.isLoading}
        onOpenChange={setCreateOpen}
        onDone={invalidate}
      />
      <ServerDialog
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
        onDone={invalidate}
      />
    </>
  );
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
  onDone: () => void;
}) {
  const editing = server !== null;
  const [displayName, setDisplayName] = useState("");
  const [serverId, setServerId] = useState("");
  const [idTouched, setIdTouched] = useState(false);
  const [serverUrl, setServerUrl] = useState("");
  const [transport, setTransport] = useState<string>("auto");
  const [approval, setApproval] = useState<string>("providerDefault");
  const [allowedTools, setAllowedTools] = useState("");
  const [description, setDescription] = useState("");
  const [authPolicy, setAuthPolicy] = useState<string>("none");
  const [credentialGrantId, setCredentialGrantId] = useState("");
  const [status, setStatus] = useState<McpServer["status"]>("active");
  const [error, setError] = useState<string | null>(null);
  const [seeded, setSeeded] = useState<string | null>(null);

  // Seed the form when the edit dialog opens for a server (keyed by id so
  // switching rows re-seeds).
  const seedKey = server ? server.serverId : null;
  if (seedKey !== seeded) {
    setSeeded(seedKey);
    setDisplayName(server?.displayName ?? "");
    setServerId(server?.serverId ?? "");
    setIdTouched(false);
    setServerUrl(server?.serverUrl ?? "");
    setTransport(server?.transport ?? "auto");
    setApproval(server?.approvalDefault ?? "providerDefault");
    setAllowedTools((server?.allowedTools ?? []).join(", "));
    setDescription(server?.description ?? "");
    setAuthPolicy(server?.authPolicy.type ?? "none");
    setCredentialGrantId(server?.credential?.grantId ?? "");
    setStatus(server?.status ?? "active");
    setError(null);
  }

  const oauthManaged = editing && !FORM_AUTH_POLICIES.includes(
    server.authPolicy.type as (typeof FORM_AUTH_POLICIES)[number],
  );
  const effectiveAuthPolicy = oauthManaged ? server.authPolicy.type : authPolicy;
  const compatibleGrants = authGrants
    .filter((grant) => mcpGrantCompatible(effectiveAuthPolicy, grant.providerKind))
    .slice()
    .sort((left, right) => authGrantLabel(left).localeCompare(authGrantLabel(right)));
  const boundGrantAvailable = compatibleGrants.some(
    (grant) => grant.grantId === credentialGrantId,
  );

  const parsedTools = allowedTools
    .split(",")
    .map((tool) => tool.trim())
    .filter(Boolean);

  const save = useMutation({
    mutationFn: () => {
      const credential = effectiveAuthPolicy === "none" || !credentialGrantId
        ? null
        : { type: "authGrant" as const, grantId: credentialGrantId };
      const nextStatus = mcpServerStatusForCredential(
        effectiveAuthPolicy,
        editing ? status : "active",
        credentialGrantId,
      );
      if (!editing) {
        return api<McpServer>("POST", `/api/v1/universes/${universeId}/mcp-servers`, {
          serverId,
          serverUrl,
          defaultServerLabel: serverId,
          transport,
          approvalDefault: approval,
          authPolicy: { type: authPolicy },
          credential,
          status: nextStatus,
          ...(displayName.trim() ? { displayName: displayName.trim() } : {}),
          ...(description.trim() ? { description: description.trim() } : {}),
          ...(parsedTools.length > 0 ? { allowedTools: parsedTools } : {}),
        });
      }
      return api<McpServer>(
        "PUT",
        `/api/v1/universes/${universeId}/mcp-servers/${server.serverId}`,
        {
          serverId: server.serverId,
          serverUrl,
          defaultServerLabel: server.defaultServerLabel,
          revision: server.revision,
          transport,
          approvalDefault: approval,
          authPolicy: oauthManaged ? server.authPolicy : { type: authPolicy },
          credential,
          status: nextStatus,
          displayName: displayName.trim() || null,
          description: description.trim() || null,
          allowedTools: parsedTools.length > 0 ? parsedTools : null,
          deferLoadingDefault: server.deferLoadingDefault ?? null,
        },
      );
    },
    onSuccess: () => {
      onOpenChange(false);
      setSeeded(null);
      setDisplayName("");
      setServerId("");
      setIdTouched(false);
      setServerUrl("");
      setAllowedTools("");
      setDescription("");
      setCredentialGrantId("");
      setError(null);
      onDone();
    },
    onError: (err) => setError(err.message),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!serverId.trim() || !serverUrl.trim()) {
      setError("an id and a URL are required");
      return;
    }
    const credentialError = mcpServerCredentialError(
      effectiveAuthPolicy,
      credentialGrantId,
    );
    if (credentialError) {
      setError(credentialError);
      return;
    }
    save.mutate();
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{editing ? `Edit ${server.serverId}` : "Add MCP server"}</DialogTitle>
          <DialogDescription>
            {editing
              ? "Changes apply to new session links; running sessions keep their spec."
              : "Registers a remote MCP server profiles can link into sessions."}
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <div className="grid gap-4 sm:grid-cols-2">
            <Field>
              <FieldLabel htmlFor="mcp-name">Display name</FieldLabel>
              <Input
                id="mcp-name"
                value={displayName}
                onChange={(e) => {
                  setDisplayName(e.target.value);
                  if (!editing && !idTouched) {
                    setServerId(e.target.value ? slugify(e.target.value) : "");
                  }
                }}
                placeholder="GitHub"
                autoFocus={!editing}
              />
            </Field>
            <Field>
              <FieldLabel htmlFor="mcp-id">Server id</FieldLabel>
              <Input
                id="mcp-id"
                value={serverId}
                onChange={(e) => {
                  setServerId(e.target.value);
                  setIdTouched(e.target.value.length > 0);
                }}
                placeholder="github"
                className="font-mono"
                disabled={editing}
              />
            </Field>
          </div>
          <Field>
            <FieldLabel htmlFor="mcp-url">Server URL</FieldLabel>
            <Input
              id="mcp-url"
              value={serverUrl}
              onChange={(e) => setServerUrl(e.target.value)}
              placeholder="https://mcp.example.com/mcp"
              className="font-mono"
            />
          </Field>
          <div className="grid gap-4 sm:grid-cols-2">
            <Field>
              <FieldLabel>Transport</FieldLabel>
              <Select value={transport} onValueChange={(v) => setTransport(v as string)}>
                <SelectTrigger className="w-full" aria-label="Transport">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {TRANSPORTS.map((value) => (
                    <SelectItem key={value} value={value}>
                      {value}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
            <Field>
              <FieldLabel>Approval default</FieldLabel>
              <Select value={approval} onValueChange={(v) => setApproval(v as string)}>
                <SelectTrigger className="w-full" aria-label="Approval default">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {APPROVALS.map((value) => (
                    <SelectItem key={value} value={value}>
                      {value}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
          </div>
          <div className="grid gap-4 sm:grid-cols-2">
            <Field>
              <FieldLabel>Auth policy</FieldLabel>
              {oauthManaged ? (
                <p className="text-sm text-muted-foreground">
                  <span className="font-mono text-xs">{server.authPolicy.type}</span> —
                  managed via the lightspeed CLI.
                </p>
              ) : (
                <Select
                  value={authPolicy}
                  onValueChange={(v) => {
                    setAuthPolicy(v as string);
                    setCredentialGrantId("");
                  }}
                >
                  <SelectTrigger className="w-full" aria-label="Auth policy">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {FORM_AUTH_POLICIES.map((value) => (
                      <SelectItem key={value} value={value}>
                        {value}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              )}
            </Field>
            {editing && (
              <Field>
                <FieldLabel>Status</FieldLabel>
                <Select
                  value={status}
                  onValueChange={(v) => setStatus(v as McpServer["status"])}
                >
                  <SelectTrigger className="w-full" aria-label="Status">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="active">active</SelectItem>
                    <SelectItem value="disabled">disabled</SelectItem>
                    {["needsAuthConfig", "unverified"].includes(status) && (
                      <SelectItem value={status}>{status}</SelectItem>
                    )}
                  </SelectContent>
                </Select>
              </Field>
            )}
          </div>
          {effectiveAuthPolicy !== "none" && (
            <Field>
              <FieldLabel>Access credential</FieldLabel>
              <Select
                value={credentialGrantId || "none"}
                onValueChange={(value) =>
                  setCredentialGrantId(value === "none" ? "" : value as string)
                }
              >
                <SelectTrigger className="w-full" aria-label="Access credential">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="none">No access credential</SelectItem>
                  {credentialGrantId && !boundGrantAvailable && (
                    <SelectItem value={credentialGrantId}>
                      {credentialGrantId} (unavailable)
                    </SelectItem>
                  )}
                  {compatibleGrants.map((grant) => (
                    <SelectItem key={grant.grantId} value={grant.grantId}>
                      {authGrantLabel(grant)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <FieldDescription>
                Owned by this universe server and used by every session that selects it.
                {effectiveAuthPolicy.startsWith("required") && !credentialGrantId
                  ? " The server will remain in needs-auth-config until one is bound."
                  : ""}
                {authGrantsLoading
                  ? " Loading access credentials…"
                  : compatibleGrants.length === 0 && !credentialGrantId
                    ? " Create a compatible access credential on the Secrets page first."
                    : ""}
              </FieldDescription>
            </Field>
          )}
          <Field>
            <FieldLabel htmlFor="mcp-tools">Allowed tools</FieldLabel>
            <Input
              id="mcp-tools"
              value={allowedTools}
              onChange={(e) => setAllowedTools(e.target.value)}
              placeholder="search, get_issue (comma-separated; empty = all)"
            />
            <FieldDescription>
              Provider-side allowlist applied by default when linking.
            </FieldDescription>
          </Field>
          <Field>
            <FieldLabel htmlFor="mcp-description">Description</FieldLabel>
            <Input
              id="mcp-description"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="What this server offers"
            />
          </Field>
          {error && <p className="text-sm text-destructive">{error}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={save.isPending}>
              {save.isPending ? "Saving…" : editing ? "Save" : "Add server"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
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

function authGrantLabel(grant: AuthGrantOption): string {
  const name = grant.displayName || grant.subjectHint || grant.grantId;
  return `${name} (${grant.providerId})`;
}
