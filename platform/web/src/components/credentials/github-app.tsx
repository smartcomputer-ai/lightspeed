import { useState, type ChangeEvent, type FormEvent } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { ChevronDown, RefreshCw } from "lucide-react";
import { api, type GitHubApp, type GitHubInstallation, type SecretGrant } from "@/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import { DialogFooter } from "@/components/ui/dialog";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
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
import { LoadingNote } from "@/components/page";
import { installationGrantFor, permissionEntries, validateGitHubAppForm } from "@/lib/integrations";
import { ConfirmDangerButton } from "@/components/confirm-danger-button";

const DEFAULT_GITHUB_API_BASE_URL = "https://api.github.com";

/// Register an existing GitHub App (App ID + private key). Rendered inside
/// the Credentials page's GitHub App dialog.
export function GitHubAppForm({
  universeId,
  onCreated,
  onCancel,
}: {
  universeId: string;
  onCreated: () => void;
  onCancel: () => void;
}) {
  const [displayName, setDisplayName] = useState("");
  const [appId, setAppId] = useState("");
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [apiBaseUrl, setApiBaseUrl] = useState(DEFAULT_GITHUB_API_BASE_URL);
  const [providerId, setProviderId] = useState("");
  const [privateKey, setPrivateKey] = useState("");
  const [error, setError] = useState<string | null>(null);

  const create = useMutation<GitHubApp, Error, void>({
    mutationFn: () =>
      api<GitHubApp>("POST", `/api/v1/universes/${universeId}/integrations/github/apps`, {
        appId: appId.trim(),
        privateKey,
        ...(displayName.trim() ? { displayName: displayName.trim() } : {}),
        ...(providerId.trim() ? { providerId: providerId.trim() } : {}),
        ...(apiBaseUrl.trim() && apiBaseUrl.trim() !== DEFAULT_GITHUB_API_BASE_URL
          ? { apiBaseUrl: apiBaseUrl.trim() }
          : {}),
      }),
    onSuccess: onCreated,
    onError: (reason) => setError(reason.message),
  });

  const readPemFile = (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    if (!file) return;
    file
      .text()
      .then((text) => {
        setPrivateKey(text);
        setError(null);
      })
      .catch(() => setError("could not read the selected file"));
    event.target.value = "";
  };

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const validation = validateGitHubAppForm({ appId, privateKey });
    if (validation) {
      setError(validation);
      return;
    }
    create.mutate();
  };

  return (
    <form onSubmit={submit} className="grid gap-4">
      <p className="text-sm text-muted-foreground">
        Register a GitHub App you already created. The private key is sent once to Lightspeed,
        encrypted, and never returned. Afterwards, install the App on the GitHub accounts you want
        to reach and grant those installations from the App's details.
      </p>
      <Field>
        <FieldLabel htmlFor="github-app-name">Display name</FieldLabel>
        <Input
          id="github-app-name"
          value={displayName}
          onChange={(event) => setDisplayName(event.target.value)}
          placeholder="Acme Lightspeed"
          autoFocus
        />
      </Field>
      <Field>
        <FieldLabel htmlFor="github-app-id">App ID</FieldLabel>
        <Input
          id="github-app-id"
          value={appId}
          onChange={(event) => {
            setAppId(event.target.value);
            setError(null);
          }}
          inputMode="numeric"
          placeholder="123456"
          className="font-mono"
        />
        <FieldDescription>
          The numeric App ID shown on the App's settings page (not the client ID or slug).
        </FieldDescription>
      </Field>
      <Field>
        <FieldLabel htmlFor="github-app-key">Private key (PEM)</FieldLabel>
        <Textarea
          id="github-app-key"
          value={privateKey}
          onChange={(event) => {
            setPrivateKey(event.target.value);
            setError(null);
          }}
          autoComplete="off"
          spellCheck={false}
          rows={4}
          className="max-h-24 min-h-20 resize-y overflow-y-auto font-mono text-xs"
          placeholder="-----BEGIN RSA PRIVATE KEY-----"
        />
        <div className="flex items-center gap-2">
          <Input
            id="github-app-key-file"
            type="file"
            accept=".pem,application/x-pem-file,text/plain"
            onChange={readPemFile}
            className="max-w-xs"
            aria-label="Upload private key file"
          />
          <FieldDescription>Or upload the .pem file downloaded from GitHub.</FieldDescription>
        </div>
      </Field>
      <Collapsible open={advancedOpen} onOpenChange={setAdvancedOpen}>
        <CollapsibleTrigger className="inline-flex h-8 items-center gap-1.5 rounded-md px-2.5 text-sm font-medium text-muted-foreground outline-none hover:bg-muted hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50">
          Advanced options
          <ChevronDown className={`size-3.5 transition-transform ${advancedOpen ? "rotate-180" : ""}`} />
        </CollapsibleTrigger>
        <CollapsibleContent>
          <div className="mt-2 grid gap-4 rounded-lg border bg-muted/15 p-3">
            <Field>
              <FieldLabel htmlFor="github-app-api-url">API base URL</FieldLabel>
              <Input
                id="github-app-api-url"
                value={apiBaseUrl}
                onChange={(event) => setApiBaseUrl(event.target.value)}
                className="font-mono"
              />
              <FieldDescription>
                Change only for GitHub Enterprise Server, e.g.{" "}
                <span className="font-mono">https://github.example.com/api/v3</span>.
              </FieldDescription>
            </Field>
            <Field>
              <FieldLabel htmlFor="github-app-provider-id">Custom provider ID</FieldLabel>
              <Input
                id="github-app-provider-id"
                value={providerId}
                onChange={(event) => setProviderId(event.target.value)}
                placeholder="github-app:<App ID> when blank"
                className="font-mono"
              />
              <FieldDescription>
                Stable identifier referenced by installation credentials and automation.
              </FieldDescription>
            </Field>
          </div>
        </CollapsibleContent>
      </Collapsible>
      {error && <p className="text-sm text-destructive">{error}</p>}
      <DialogFooter>
        <Button type="button" variant="outline" onClick={onCancel}>
          Back
        </Button>
        <Button type="submit" disabled={create.isPending}>
          {create.isPending ? "Encrypting…" : "Add GitHub App"}
        </Button>
      </DialogFooter>
    </form>
  );
}

/// Details for a registered GitHub App: installations (grant / re-grant) and
/// removal. Rendered inside the Credentials page's GitHub App dialog.
export function GitHubAppDetails({
  universeId,
  app,
  grants,
  onChanged,
  onRemoved,
}: {
  universeId: string;
  app: GitHubApp;
  grants: SecretGrant[];
  onChanged: () => void;
  onRemoved: () => void;
}) {
  const installations = useQuery({
    queryKey: ["integrations", "github", universeId, app.providerId, "installations"],
    queryFn: () =>
      api<GitHubInstallation[]>(
        "GET",
        `/api/v1/universes/${universeId}/integrations/github/apps/${encodeURIComponent(app.providerId)}/installations`,
      ),
  });
  const grant = useMutation({
    mutationFn: (installation: GitHubInstallation) =>
      api<SecretGrant>(
        "POST",
        `/api/v1/universes/${universeId}/integrations/github/apps/${encodeURIComponent(app.providerId)}/installations/${installation.installationId}/grant`,
        installation.accountLogin ? { displayName: `GitHub: ${installation.accountLogin}` } : {},
      ),
    onSuccess: onChanged,
  });
  const remove = useMutation({
    mutationFn: () =>
      api<GitHubApp>(
        "DELETE",
        `/api/v1/universes/${universeId}/integrations/github/apps/${encodeURIComponent(app.providerId)}`,
      ),
    onSuccess: onRemoved,
  });

  return (
    <div className="grid gap-4">
      <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
        <dt className="text-muted-foreground">App ID</dt>
        <dd className="font-mono">{app.config.appId}</dd>
        <dt className="text-muted-foreground">API base URL</dt>
        <dd className="font-mono">{app.config.apiBaseUrl}</dd>
        <dt className="text-muted-foreground">Provider ID</dt>
        <dd>
          <IdText>{app.providerId}</IdText>
        </dd>
        <dt className="text-muted-foreground">Status</dt>
        <dd>
          <GitHubAppStatusBadge app={app} />
        </dd>
      </dl>

      <div>
        <div className="mb-2 flex items-center justify-between gap-2">
          <p className="text-sm font-medium">Installations</p>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => void installations.refetch()}
            disabled={installations.isFetching}
          >
            <RefreshCw className={installations.isFetching ? "animate-spin" : undefined} />
            Refresh
          </Button>
        </div>
        <p className="mb-2 text-sm text-muted-foreground">
          Accounts where this App is installed, live from GitHub. Grant an installation to let this
          universe mint tokens for its repositories.
        </p>
        {installations.isLoading && <LoadingNote />}
        {installations.error && (
          <p className="text-sm text-destructive">{installations.error.message}</p>
        )}
        {grant.error && <p className="mb-2 text-sm text-destructive">{grant.error.message}</p>}
        {installations.data && installations.data.length === 0 && (
          <p className="text-sm text-muted-foreground">
            No installations yet. Install the App on a GitHub account or organization, then refresh.
          </p>
        )}
        {installations.data && installations.data.length > 0 && (
          <TableCard>
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Account</TableHead>
                  <TableHead>Repositories</TableHead>
                  <TableHead>Permissions</TableHead>
                  <TableHead>Status</TableHead>
                  <TableHead className="w-0" />
                </TableRow>
              </TableHeader>
              <TableBody>
                {installations.data.map((installation) => {
                  const existing = installationGrantFor(grants, installation.installationId);
                  const granting =
                    grant.isPending && grant.variables?.installationId === installation.installationId;
                  return (
                    <TableRow key={installation.installationId}>
                      <TableTitleCell
                        title={installation.accountLogin ?? "Unknown account"}
                        subtitle={`installation ${installation.installationId}`}
                      />
                      <TableCell className="text-muted-foreground">
                        {installation.repositorySelection ?? "—"}
                      </TableCell>
                      <TableCell className="whitespace-normal text-muted-foreground">
                        <PermissionList permissions={installation.permissions} />
                      </TableCell>
                      <TableCell>
                        {existing ? (
                          <InstallationStatusBadge status={existing.status} />
                        ) : (
                          <Badge variant="outline">not granted</Badge>
                        )}
                      </TableCell>
                      <TableActionsCell>
                        {(!existing || existing.status !== "active") && (
                          <Button
                            size="sm"
                            variant={existing ? "outline" : "default"}
                            disabled={granting}
                            onClick={() => grant.mutate(installation)}
                          >
                            {granting ? "Granting…" : existing ? "Re-grant" : "Grant"}
                          </Button>
                        )}
                      </TableActionsCell>
                    </TableRow>
                  );
                })}
              </TableBody>
            </Table>
          </TableCard>
        )}
      </div>

      {remove.error && <p className="text-sm text-destructive">{remove.error.message}</p>}
      <DialogFooter>
        <ConfirmDangerButton
          label="Remove GitHub App"
          title="Remove this GitHub App?"
          description={
            <>
              The stored private key is deleted and installation credentials of{" "}
              <span className="font-mono text-xs">{app.providerId}</span> stop resolving tokens.
              The App itself stays installed on GitHub.
            </>
          }
          pending={remove.isPending}
          onConfirm={() => remove.mutate()}
        />
      </DialogFooter>
    </div>
  );
}

export function GitHubAppStatusBadge({ app }: { app: GitHubApp }) {
  if (app.status === "active" && app.hasCredential) return <Badge variant="secondary">active</Badge>;
  return (
    <Badge variant="outline" className="border-destructive/50 text-destructive">
      needs private key
    </Badge>
  );
}

function PermissionList({ permissions }: { permissions: Record<string, unknown> | undefined }) {
  const entries = permissionEntries(permissions);
  if (entries.length === 0) return <>—</>;
  return (
    <div className="flex max-w-md flex-wrap gap-1">
      {entries.map(([name, level]) => (
        <Badge key={name} variant="outline" className="font-mono text-[11px] font-normal">
          {name}: {level}
        </Badge>
      ))}
    </div>
  );
}

function InstallationStatusBadge({ status }: { status: SecretGrant["status"] }) {
  if (status === "active") return <Badge variant="secondary">granted</Badge>;
  if (status === "needsReauth" || status === "failed") {
    return (
      <Badge variant="outline" className="border-destructive/50 text-destructive">
        {status === "needsReauth" ? "needs reinstall" : status}
      </Badge>
    );
  }
  return <Badge variant="outline">revoked</Badge>;
}
