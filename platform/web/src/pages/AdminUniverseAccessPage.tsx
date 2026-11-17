import { useState } from "react";
import type { Universe } from "@/api";
import { AddIcon } from "@/icons";
import { AdminUniverseHeader } from "@/components/admin/universe-header";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from "@/components/ui/dialog";
import {
  AlertDialog,
  AlertDialogContent,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogCancel,
} from "@/components/ui/alert-dialog";
import { Link } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  UNIVERSE_ROLES,
  type PrincipalRef,
  type UniverseRole,
} from "@lightspeed-ai/platform-shared";
import { platformApi } from "@/lib/platform-api";
import { useActiveUniverse } from "@/lib/universes";
import { useActionPermissions } from "@/lib/permissions";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Input } from "@/components/ui/input";
import { Field, FieldLabel } from "@/components/ui/field";
import {
  PageHeader,
  SectionHeader,
  LoadingNote,
  UniverseNotFound,
} from "@/components/page";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  TableCard,
} from "@/components/ui/table";

const roleLabels: Record<UniverseRole, string> = {
  owner: "Owner",
  operator: "Operator",
  contributor: "Contributor",
  viewer: "Viewer",
};

export function AdminUniverseAccessPage() {
  const { universe, handle, isLoading } = useActiveUniverse();
  const permissions = useActionPermissions(universe?.id);
  if (isLoading) return <LoadingNote />;
  if (!universe) return <UniverseNotFound handle={handle} />;
  if (!permissions.can("administer"))
    return (
      <>
        <PageHeader title="Access" />
        <p>
          Only a universe Owner, Organization Admin, or Platform Admin can view or
          manage access.
        </p>
      </>
    );
  return (
    <UniverseAccess
      key={universe.id}
      record={universe}
      universe={universe.lightspeedUniverseId}
      organizationId={universe.organizationId}
      organizationAdmin={universe.organizationActions?.includes("manageMembers") === true}
    />
  );
}

function UniverseAccess({
  record,
  universe,
  organizationId,
  organizationAdmin,
}: {
  record: Universe;
  universe: string;
  organizationId?: string;
  organizationAdmin: boolean;
}) {
  const client = useQueryClient();
  const [editorOpen, setEditorOpen] = useState(false);
  const [editing, setEditing] = useState<{
    principal: PrincipalRef;
    name: string;
    role: UniverseRole;
  } | null>(null);
  const [removing, setRemoving] = useState<{ id: string; name: string } | null>(
    null,
  );
  const [principal, setPrincipal] = useState("");
  const [role, setRole] = useState<UniverseRole>("viewer");
  const grants = useQuery({
    queryKey: ["grants", universe],
    queryFn: () => platformApi("platform/universes/grants/list", { universe }),
  });
  const members = useQuery({
    queryKey: ["organization-members", organizationId],
    enabled: !!organizationId,
    queryFn: () =>
      platformApi("platform/organizations/members/list", { organizationId: organizationId! }),
  });
  const serviceUsers = useQuery({
    queryKey: ["service-users", organizationId],
    enabled: !!organizationId,
    queryFn: () =>
      platformApi("platform/organizations/service-users/list", {
        organizationId: organizationId!,
      }),
  });
  const refresh = async () => {
    await client.invalidateQueries({ queryKey: ["grants", universe] });
    await client.invalidateQueries({ queryKey: ["universes"] });
  };
  const put = useMutation({
    mutationFn: (input: { principal: PrincipalRef; role: UniverseRole }) =>
      platformApi("platform/universes/grants/put", { universe, ...input }),
    onSuccess: async () => {
      await refresh();
      setEditorOpen(false);
    },
  });
  const remove = useMutation({
    mutationFn: (grantId: string) =>
      platformApi("platform/universes/grants/delete", { universe, grantId }),
    onSuccess: async () => {
      await refresh();
      setRemoving(null);
    },
  });
  const choices = [
    ...(members.data?.members ?? []).map((m) => ({
      value: `user:${m.userId}`,
      label: `${m.name}${m.email ? ` (${m.email})` : ""}`,
    })),
    ...(serviceUsers.data?.serviceUsers ?? [])
      .filter((a) => a.enabled)
      .map((a) => ({
        value: `serviceUser:${a.id}`,
        label: `${a.name} (service user)`,
      })),
  ];
  const directoriesLoading =
    !!organizationId && (members.isLoading || serviceUsers.isLoading);
  const directoriesUnavailable =
    !organizationId || !!members.error || !!serviceUsers.error;
  return (
    <>
      <AdminUniverseHeader universe={record} section="access" />
      <SectionHeader
        title="Access"
        description="Manage explicit access for people and service users. Organization Admins and Platform Admins retain access through their role."
        actions={
          <Button
            onClick={() => {
              setEditing(null);
              setPrincipal("");
              setRole("viewer");
              put.reset();
              setEditorOpen(true);
            }}
          >
            <AddIcon data-icon="inline-start" />
            Add access
          </Button>
        }
      />
      <div className="mb-6 grid gap-2 text-sm text-muted-foreground">
        <p>
          Universe Owners manage these grants. Organization membership alone does not
          grant universe access to Members or Creators. Access granted by a
          configured universe policy is managed in that policy.
        </p>
        <p>
          API keys belong to a person or a service user and can be limited to
          this universe. Manage{" "}
          <Link className="underline" to="/account#api-keys">
            your API keys
          </Link>
          {organizationAdmin && organizationId ? (
            <>
              {" "}
              or{" "}
              <Link
                className="underline"
                to={`/admin/organizations/${encodeURIComponent(organizationId)}/people`}
              >
                organization members
              </Link>{" and "}<Link className="underline" to={`/admin/organizations/${encodeURIComponent(organizationId)}/api-keys`}>service user keys</Link>
            </>
          ) : (
            "; ask an Organization Admin to add people or manage service user keys"
          )}
          .
        </p>
      </div>
      {[grants.error, members.error, serviceUsers.error]
        .filter(Boolean)
        .map((e, i) => (
          <p role="alert" className="text-destructive" key={i}>
            {e?.message}
          </p>
        ))}
      <Dialog open={editorOpen} onOpenChange={setEditorOpen}>
        <DialogContent className="max-h-[calc(100dvh-2rem)] overflow-y-auto sm:max-w-lg">
          <DialogHeader>
            <DialogTitle>
              {editing ? "Edit universe role" : "Add universe access"}
            </DialogTitle>
            <DialogDescription>
              {editing
                ? `Change ${editing.name}’s role in ${record.name}.`
                : `Choose an existing organization member or service user and its role in ${record.name}.`}
            </DialogDescription>
          </DialogHeader>
          <form
            className="grid min-w-0 gap-4"
            onSubmit={(event) => {
              event.preventDefault();
              if (
                put.isPending ||
                (!editing &&
                  (directoriesLoading ||
                    directoriesUnavailable ||
                    !choices.some((choice) => choice.value === principal)))
              )
                return;
              const split = principal.indexOf(":");
              put.mutate({
                principal: editing?.principal ?? {
                  kind: principal.slice(0, split) as PrincipalRef["kind"],
                  id: principal.slice(split + 1),
                },
                role,
              });
            }}
          >
            <Field className="min-w-0 sm:w-auto">
              <FieldLabel htmlFor="universe-access-recipient">
                Organization member or service user
              </FieldLabel>
              {editing ? (
                <Input
                  id="universe-access-recipient"
                  value={editing.name}
                  readOnly
                />
              ) : (
                <Select
                  value={principal}
                  onValueChange={(value) => value !== null && setPrincipal(value)}
                  disabled={directoriesLoading || directoriesUnavailable}
                  required
                >
                  <SelectTrigger id="universe-access-recipient"><SelectValue /></SelectTrigger>
                  <SelectContent>
                    <SelectItem value="">
                      {directoriesLoading
                        ? "Loading organization members and service users…"
                        : "Select a person or service user"}
                    </SelectItem>
                    {choices.map((c) => (
                      <SelectItem key={c.value} value={c.value}>
                        {c.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              )}
            </Field>
            <Field className="sm:w-auto">
              <FieldLabel htmlFor="universe-access-role">
                Universe role
              </FieldLabel>
              <Select
                value={role}
                onValueChange={(value) => value !== null && setRole(value as UniverseRole)}
              >
                <SelectTrigger id="universe-access-role"><SelectValue /></SelectTrigger>
                <SelectContent>
                  {UNIVERSE_ROLES.map((r) => (
                    <SelectItem key={r} value={r}>
                      {roleLabels[r]}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
            {put.error && (
              <p role="alert" className="text-destructive">
                {put.error.message}
              </p>
            )}
            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={() => setEditorOpen(false)}
              >
                Cancel
              </Button>
              <Button
                type="submit"
                disabled={
                  put.isPending ||
                  (editing
                    ? role === editing.role
                    : !principal ||
                      directoriesLoading ||
                      directoriesUnavailable)
                }
              >
                {put.isPending
                  ? "Saving…"
                  : editing
                    ? "Save changes"
                    : "Add access"}
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
      {!directoriesLoading &&
        !directoriesUnavailable &&
        choices.length === 0 && (
          <p className="mb-6 text-sm text-muted-foreground">
            No organization members or enabled service users are available. An Organization
            Admin can add them in Organizations.
          </p>
        )}
      {!organizationId && (
        <p role="alert">
          This universe has no organization. Contact a Platform Admin to restore its
          organization before adding access.
        </p>
      )}
      {grants.isLoading && <LoadingNote />}
      {grants.data?.grants.length === 0 && (
        <p className="mb-3 text-sm text-muted-foreground">
          No explicit universe grants. Organization Admins and Platform Admins retain
          access.
        </p>
      )}
      {!!grants.data?.grants.length && (
        <TableCard>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Person or service user</TableHead>
                <TableHead>Type</TableHead>
                <TableHead>Universe role</TableHead>
                <TableHead />
              </TableRow>
            </TableHeader>
            <TableBody>
              {grants.data.grants.map((g) => (
                <TableRow key={g.id}>
                  <TableCell>{g.name}</TableCell>
                  <TableCell>
                    {g.principal.kind === "user" ? "Person" : "Service user"}
                  </TableCell>
                  <TableCell>{roleLabels[g.role]}</TableCell>
                  <TableCell className="whitespace-nowrap text-right">
                    <Button
                      variant="ghost"
                      aria-label={`Edit role for ${g.name}`}
                      onClick={() => {
                        setEditing({
                          principal: g.principal,
                          name: g.name,
                          role: g.role,
                        });
                        setPrincipal(`${g.principal.kind}:${g.principal.id}`);
                        setRole(g.role);
                        put.reset();
                        setEditorOpen(true);
                      }}
                    >
                      Edit
                    </Button>
                    <Button
                      variant="ghost"
                      aria-label={`Remove access for ${g.name}`}
                      disabled={remove.isPending}
                      onClick={() => {
                        remove.reset();
                        setRemoving({ id: g.id, name: g.name });
                      }}
                    >
                      Remove
                    </Button>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </TableCard>
      )}
      <AlertDialog
        open={!!removing}
        onOpenChange={(open) => {
          if (!open) setRemoving(null);
        }}
      >
        <AlertDialogContent className="max-h-[calc(100dvh-2rem)] overflow-y-auto">
          <AlertDialogHeader>
            <AlertDialogTitle>
              Remove access for {removing?.name}?
            </AlertDialogTitle>
            <AlertDialogDescription>
              This removes their explicit universe grant. Access granted by
              policy or an Organization Admin or Platform Admin role remains.
            </AlertDialogDescription>
          </AlertDialogHeader>
          {remove.error && (
            <p role="alert" className="text-destructive">
              {remove.error.message}
            </p>
          )}
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <Button
              variant="destructive"
              disabled={remove.isPending}
              onClick={() => removing && remove.mutate(removing.id)}
            >
              {remove.isPending ? "Removing…" : "Remove access"}
            </Button>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      <p className="mt-3 text-sm text-muted-foreground">
        Keep at least one explicit Owner. Universe roles apply equally to people
        and service users.
      </p>
    </>
  );
}
