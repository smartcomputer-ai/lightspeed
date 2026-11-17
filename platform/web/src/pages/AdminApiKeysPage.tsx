import { useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import {
  useMutation,
  useQueries,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { platformApi, useAccounts } from "@/lib/platform-api";
import { usePlatformAdmin } from "@/lib/permissions";
import { ApiKeyRevokeDialog } from "@/components/api-key-revoke-dialog";
import { ApiKeysCard } from "@/components/api-keys-card";
import { PageHeader, LoadingNote } from "@/components/page";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Field, FieldLabel } from "@/components/ui/field";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from "@/components/ui/dialog";
import {
  Table,
  TableCard,
  TableHead,
  TableHeader,
  TableBody,
  TableRow,
  TableCell,
} from "@/components/ui/table";
import { AddIcon } from "@/icons";

/** Account selection bounds directory and key requests; opening a deep link never falls back to another owner. */
export function AdminApiKeysPage() {
  const accounts = useAccounts();
  const platformAdmin = usePlatformAdmin();
  const [search, setSearch] = useSearchParams();
  const accountId = search.get("account") ?? "";
  const managed =
    accounts.data?.accounts.filter(
      (account) => platformAdmin || account.role === "admin",
    ) ?? [];
  const account = managed.find((account) => account.id === accountId);
  return (
    <>
      <PageHeader
        title="API keys"
        description="Manage application keys in accounts you administer. Keys act as their application and can only narrow its permissions."
      />
      <p className="mb-6 text-sm text-muted-foreground">
        For scripts acting as you, use{" "}
        <Link className="underline" to="/account#api-keys">
          your personal API keys
        </Link>
        .
      </p>
      {accounts.isLoading && <LoadingNote />}
      {accounts.error && (
        <p role="alert" className="text-destructive">
          {accounts.error.message}
        </p>
      )}
      {!!managed.length && (
        <Field className="mb-6 max-w-xl">
          <FieldLabel htmlFor="key-account">Account</FieldLabel>
          <select
            id="key-account"
            className="min-w-0 max-w-full rounded-md border bg-background p-2"
            value={accountId}
            onChange={(event) =>
              setSearch(
                event.target.value ? { account: event.target.value } : {},
              )
            }
          >
            <option value="">Select an account</option>
            {managed.map((item) => (
              <option key={item.id} value={item.id}>
                {item.name}
                {!item.enabled ? " (suspended)" : ""}
              </option>
            ))}
          </select>
        </Field>
      )}
      {accounts.data && !managed.length && (
        <p>You need an Account Admin role to manage application keys.</p>
      )}
      {accountId && accounts.data && !account && (
        <p role="alert">
          This account is unavailable or you do not administer it.
        </p>
      )}
      {account && !account.enabled && (
        <p>
          This account is suspended. Restore its access before managing
          application keys.
        </p>
      )}
      {account?.enabled && (
        <AccountKeys
          key={account.id}
          accountId={account.id}
          accountName={account.name}
        />
      )}
      {!accountId && !!managed.length && (
        <p className="text-sm text-muted-foreground">
          Select an account to see keys across its applications.
        </p>
      )}
    </>
  );
}

function AccountKeys({
  accountId,
  accountName,
}: {
  accountId: string;
  accountName: string;
}) {
  const client = useQueryClient();
  const [search, setSearch] = useSearchParams();
  const applicationId = search.get("application") ?? "";
  const [ownerOpen, setOwnerOpen] = useState(false);
  const [ownerId, setOwnerId] = useState("");
  const [createOpen, setCreateOpen] = useState(false);
  const [revoking, setRevoking] = useState<{
    applicationId: string;
    keyId: string;
    name: string;
    owner: string;
  } | null>(null);
  const applications = useQuery({
    queryKey: ["applications", accountId],
    queryFn: () =>
      platformApi("platform/accounts/applications/list", { accountId }),
  });
  const apps = applications.data?.applications ?? [];
  const application = apps.find((app) => app.id === applicationId);
  const inventoryApps = applicationId ? [] : apps;
  const lists = useQueries({
    queries: inventoryApps.map((app) => ({
      queryKey: ["api-keys", accountId, app.id],
      queryFn: () =>
        platformApi("platform/applications/credentials/list", {
          accountId,
          applicationId: app.id,
        }),
    })),
  });
  const rows = lists.flatMap((list, index) =>
    (list.data?.credentials ?? []).map((key) => ({
      key,
      app: inventoryApps[index]!,
    })),
  );
  const revoke = useMutation({
    mutationFn: (input: { applicationId: string; keyId: string }) =>
      platformApi("platform/applications/credentials/revoke", {
        accountId,
        ...input,
      }),
    onSuccess: async (_, input) => {
      setRevoking(null);
      await client.invalidateQueries({
        queryKey: ["api-keys", accountId, input.applicationId],
      });
    },
  });
  const selectApplication = (id: string) => {
    setCreateOpen(false);
    setSearch(
      id ? { account: accountId, application: id } : { account: accountId },
    );
  };
  return (
    <>
      <div className="mb-6 flex flex-wrap items-end gap-3">
        <Field className="min-w-0 flex-1 sm:max-w-xl">
          <FieldLabel htmlFor="key-application">
            Application in {accountName}
          </FieldLabel>
          <select
            id="key-application"
            className="min-w-0 max-w-full rounded-md border bg-background p-2"
            value={applicationId}
            onChange={(event) => selectApplication(event.target.value)}
            disabled={applications.isLoading}
          >
            <option value="">All applications</option>
            {apps.map((app) => (
              <option key={app.id} value={app.id}>
                {app.name}
                {!app.enabled ? " (disabled)" : ""}
              </option>
            ))}
          </select>
        </Field>
        {!application && (
          <Button
            disabled={!apps.some((app) => app.enabled)}
            onClick={() => {
              setOwnerId("");
              setOwnerOpen(true);
            }}
          >
            <AddIcon data-icon="inline-start" />
            New API key
          </Button>
        )}
      </div>
      {applications.isLoading && <LoadingNote />}
      {applications.error && (
        <p role="alert" className="text-destructive">
          {applications.error.message}
        </p>
      )}
      {applicationId && applications.data && !application && (
        <p role="alert">
          This application is unavailable in the selected account.
        </p>
      )}
      {application && (
        <ApiKeysCard
          key={application.id}
          application={{
            accountId,
            applicationId: application.id,
            name: application.name,
            enabled: application.enabled,
          }}
          createOpen={createOpen}
          onCreateOpenChange={setCreateOpen}
        />
      )}
      {!applicationId && applications.data && (
        <>
          {lists.some((list) => list.isLoading) && <LoadingNote />}
          {lists.map(
            (list, index) =>
              list.error && (
                <p
                  role="alert"
                  className="text-destructive"
                  key={inventoryApps[index]!.id}
                >
                  Could not load keys for {inventoryApps[index]!.name}:{" "}
                  {list.error.message}
                </p>
              ),
          )}

          {!apps.length && (
            <p className="text-sm text-muted-foreground">
              No applications yet.{" "}
              <Link
                className="underline"
                to={`/admin/accounts?account=${encodeURIComponent(accountId)}`}
              >
                Create an application in this account
              </Link>{" "}
              to give an integration its own identity.
            </p>
          )}
          {!!apps.length &&
            lists.every((list) => !list.isLoading && !list.error) &&
            !rows.length && (
              <p className="text-sm text-muted-foreground">
                No application keys in this account yet.
              </p>
            )}
          {!!rows.length && (
            <TableCard>
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Key</TableHead>
                    <TableHead>Application</TableHead>
                    <TableHead>Status</TableHead>
                    <TableHead>Expires</TableHead>
                    <TableHead />
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {rows.map(({ key, app }) => (
                    <TableRow key={`${app.id}/${key.id}`}>
                      <TableCell>
                        <span className="font-medium">{key.name}</span>
                        <p className="text-xs text-muted-foreground">
                          {key.start}…
                        </p>
                      </TableCell>
                      <TableCell>
                        <Link
                          className="underline"
                          to={`/admin/api-keys?account=${encodeURIComponent(accountId)}&application=${encodeURIComponent(app.id)}`}
                        >
                          {app.name}
                        </Link>
                      </TableCell>
                      <TableCell>
                        <Badge variant="secondary">
                          {!key.enabled
                            ? "Revoked"
                            : new Date(key.expiresAt).getTime() <= Date.now()
                              ? "Expired"
                              : !app.enabled
                                ? "Application disabled"
                                : "Active"}
                        </Badge>
                      </TableCell>
                      <TableCell>
                        {new Date(key.expiresAt).toLocaleDateString()}
                      </TableCell>
                      <TableCell>
                        <Button
                          variant="outline"
                          disabled={!key.enabled || revoke.isPending}
                          onClick={() => {
                            revoke.reset();
                            setRevoking({
                              applicationId: app.id,
                              keyId: key.id,
                              name: key.name,
                              owner: app.name,
                            });
                          }}
                        >
                          Revoke
                        </Button>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </TableCard>
          )}
        </>
      )}
      <ApiKeyRevokeDialog
        name={revoking?.name}
        owner={revoking?.owner}
        pending={revoke.isPending}
        error={revoke.error}
        onConfirm={() => {
          if (revoking)
            revoke.mutate({
              applicationId: revoking.applicationId,
              keyId: revoking.keyId,
            });
        }}
        onClose={() => {
          setRevoking(null);
          revoke.reset();
        }}
      />
      <Dialog open={ownerOpen} onOpenChange={setOwnerOpen}>
        <DialogContent className="max-h-[min(92dvh,800px)] overflow-y-auto sm:max-w-lg">
          <DialogHeader>
            <DialogTitle>New application API key</DialogTitle>
            <DialogDescription>
              Choose the application this key will act as in {accountName}.
            </DialogDescription>
          </DialogHeader>
          <form
            className="grid min-w-0 gap-5"
            onSubmit={(event) => {
              event.preventDefault();
              if (!apps.some((app) => app.id === ownerId && app.enabled))
                return;
              selectApplication(ownerId);
              setOwnerOpen(false);
              setCreateOpen(true);
            }}
          >
            <Field>
              <FieldLabel htmlFor="key-owner">Application</FieldLabel>
              <select
                id="key-owner"
                className="min-w-0 max-w-full rounded-md border bg-background p-2"
                required
                value={ownerId}
                onChange={(event) => setOwnerId(event.target.value)}
              >
                <option value="">Select an application</option>
                {apps
                  .filter((app) => app.enabled)
                  .map((app) => (
                    <option key={app.id} value={app.id}>
                      {app.name}
                    </option>
                  ))}
              </select>
            </Field>
            <DialogFooter>
              <Button
                variant="outline"
                type="button"
                onClick={() => setOwnerOpen(false)}
              >
                Cancel
              </Button>
              <Button type="submit" disabled={!ownerId}>
                Continue
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    </>
  );
}
