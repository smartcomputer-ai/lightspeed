import { useState } from "react";
import { Link, useParams, useSearchParams } from "react-router-dom";
import {
  useMutation,
  useQueries,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";
import { platformApi, useOrganizations } from "@/lib/platform-api";
import { organizationAllows } from "@/lib/permissions";
import { ApiKeyRevokeDialog } from "@/components/api-key-revoke-dialog";
import { ApiKeysCard } from "@/components/api-keys-card";
import { PageHeader, LoadingNote } from "@/components/page";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
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

/** Organization selection bounds directory and key requests; opening a deep link never falls back to another owner. */
export function AdminApiKeysPage() {
  const organizations = useOrganizations();
  const { organizationId = "" } = useParams();
  const managed =
    organizations.data?.organizations.filter(
      (organization) => organizationAllows(organization, "manageServiceUsers"),
    ) ?? [];
  const organization = managed.find((organization) => organization.id === organizationId);
  return (
    <>
      <PageHeader
        title="API keys"
        description="Manage service user keys in organizations you administer. Keys act as their service user and can only narrow its permissions."
      />
      <p className="mb-6 text-sm text-muted-foreground">
        For scripts acting as you, use{" "}
        <Link className="underline" to="/account#api-keys">
          your API keys
        </Link>
        .
      </p>
      {organizations.isLoading && <LoadingNote />}
      {organizations.error && (
        <p role="alert" className="text-destructive">
          {organizations.error.message}
        </p>
      )}
      {organizations.data && !managed.length && (
        <p>You need an Organization Admin role to manage service user keys.</p>
      )}
      {organizationId && organizations.data && !organization && (
        <p role="alert">
          This organization is unavailable or you do not administer it.
        </p>
      )}
      {organization && (
        <OrganizationKeys
          key={organization.id}
          organizationId={organization.id}
          organizationName={organization.name}
        />
      )}
      {!organizationId && !!managed.length && (
        <p className="text-sm text-muted-foreground">
          Select an organization to see keys across its service users.
        </p>
      )}
    </>
  );
}

function OrganizationKeys({
  organizationId,
  organizationName,
}: {
  organizationId: string;
  organizationName: string;
}) {
  const client = useQueryClient();
  const [search, setSearch] = useSearchParams();
  const serviceUserId = search.get("serviceUser") ?? "";
  const [ownerOpen, setOwnerOpen] = useState(false);
  const [ownerId, setOwnerId] = useState("");
  const [createOpen, setCreateOpen] = useState(false);
  const [revoking, setRevoking] = useState<{
    serviceUserId: string;
    keyId: string;
    name: string;
    owner: string;
  } | null>(null);
  const serviceUsers = useQuery({
    queryKey: ["service-users", organizationId],
    queryFn: () =>
      platformApi("platform/organizations/service-users/list", { organizationId }),
  });
  const apps = serviceUsers.data?.serviceUsers ?? [];
  const serviceUser = apps.find((app) => app.id === serviceUserId);
  const inventoryApps = serviceUserId ? [] : apps;
  const lists = useQueries({
    queries: inventoryApps.map((app) => ({
      queryKey: ["api-keys", organizationId, app.id],
      queryFn: () =>
        platformApi("platform/service-users/credentials/list", {
          organizationId,
          serviceUserId: app.id,
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
    mutationFn: (input: { serviceUserId: string; keyId: string }) =>
      platformApi("platform/service-users/credentials/revoke", {
        organizationId,
        ...input,
      }),
    onSuccess: async (_, input) => {
      setRevoking(null);
      await client.invalidateQueries({
        queryKey: ["api-keys", organizationId, input.serviceUserId],
      });
    },
  });
  const selectServiceUser = (id: string) => {
    setCreateOpen(false);
    setSearch(
      id ? { serviceUser: id } : {},
    );
  };
  return (
    <>
      <div className="mb-6 flex flex-wrap items-end gap-3">
        <Field className="min-w-0 flex-1 sm:max-w-xl">
          <FieldLabel htmlFor="key-serviceUser">
            Service user in {organizationName}
          </FieldLabel>
          <Select
            value={serviceUserId}
            onValueChange={(value) => value !== null && selectServiceUser(value)}
            disabled={serviceUsers.isLoading}
          >
            <SelectTrigger id="key-serviceUser"><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="">All service users</SelectItem>
              {apps.map((app) => (
                <SelectItem key={app.id} value={app.id}>
                  {app.name}
                  {!app.enabled ? " (disabled)" : ""}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </Field>
        {!serviceUser && (
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
      {serviceUsers.isLoading && <LoadingNote />}
      {serviceUsers.error && (
        <p role="alert" className="text-destructive">
          {serviceUsers.error.message}
        </p>
      )}
      {serviceUserId && serviceUsers.data && !serviceUser && (
        <p role="alert">
          This service user is unavailable in the selected organization.
        </p>
      )}
      {serviceUser && (
        <ApiKeysCard
          key={serviceUser.id}
          serviceUser={{
            organizationId,
            serviceUserId: serviceUser.id,
            name: serviceUser.name,
            enabled: serviceUser.enabled,
          }}
          createOpen={createOpen}
          onCreateOpenChange={setCreateOpen}
        />
      )}
      {!serviceUserId && serviceUsers.data && (
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
              No service users yet.{" "}
              <Link
                className="underline"
                to={`/admin/organizations/${encodeURIComponent(organizationId)}/service-users`}
              >
                Create a service user in this organization
              </Link>{" "}
              to give an integration its own identity.
            </p>
          )}
          {!!apps.length &&
            lists.every((list) => !list.isLoading && !list.error) &&
            !rows.length && (
              <p className="text-sm text-muted-foreground">
                No service user keys in this organization yet.
              </p>
            )}
          {!!rows.length && (
            <TableCard>
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Key</TableHead>
                    <TableHead>Service user</TableHead>
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
                          to={`/admin/organizations/${encodeURIComponent(organizationId)}/api-keys?serviceUser=${encodeURIComponent(app.id)}`}
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
                                ? "Service user disabled"
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
                              serviceUserId: app.id,
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
              serviceUserId: revoking.serviceUserId,
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
            <DialogTitle>New service user API key</DialogTitle>
            <DialogDescription>
              Choose the service user this key will act as in {organizationName}.
            </DialogDescription>
          </DialogHeader>
          <form
            className="grid min-w-0 gap-5"
            onSubmit={(event) => {
              event.preventDefault();
              if (!apps.some((app) => app.id === ownerId && app.enabled))
                return;
              selectServiceUser(ownerId);
              setOwnerOpen(false);
              setCreateOpen(true);
            }}
          >
            <Field>
              <FieldLabel htmlFor="key-owner">Service user</FieldLabel>
              <Select
                required
                value={ownerId}
                onValueChange={(value) => value !== null && setOwnerId(value)}
              >
                <SelectTrigger id="key-owner"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="">Select a service user</SelectItem>
                  {apps
                    .filter((app) => app.enabled)
                    .map((app) => (
                      <SelectItem key={app.id} value={app.id}>
                        {app.name}
                      </SelectItem>
                    ))}
                </SelectContent>
              </Select>
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
