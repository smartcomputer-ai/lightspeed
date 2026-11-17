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

export function AdminUniverseAccessPage({ admin }: { admin: boolean }) {
  const { universe, handle, isLoading } = useActiveUniverse();
  const permissions = useActionPermissions(universe?.id);
  if (isLoading) return <LoadingNote />;
  if (!universe) return <UniverseNotFound handle={handle} />;
  if (!permissions.can("manageAccess"))
    return (
      <>
        <PageHeader title="Access" />
        <p>
          Only a universe Owner, Account Admin, or Platform Admin can view or
          manage access.
        </p>
      </>
    );
  return (
    <UniverseAccess
      key={universe.id}
      record={universe}
      universe={universe.lightspeedUniverseId}
      accountId={universe.accountId}
      accountAdmin={admin || universe.accountRole === "admin"}
    />
  );
}

function UniverseAccess({
  record,
  universe,
  accountId,
  accountAdmin,
}: {
  record: Universe;
  universe: string;
  accountId?: string;
  accountAdmin: boolean;
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
    queryKey: ["account-members", accountId],
    enabled: !!accountId,
    queryFn: () =>
      platformApi("platform/accounts/members/list", { accountId: accountId! }),
  });
  const applications = useQuery({
    queryKey: ["applications", accountId],
    enabled: !!accountId,
    queryFn: () =>
      platformApi("platform/accounts/applications/list", {
        accountId: accountId!,
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
    ...(applications.data?.applications ?? [])
      .filter((a) => a.enabled)
      .map((a) => ({
        value: `application:${a.id}`,
        label: `${a.name} (application)`,
      })),
  ];
  const directoriesLoading =
    !!accountId && (members.isLoading || applications.isLoading);
  const directoriesUnavailable =
    !accountId || !!members.error || !!applications.error;
  return (
    <>
      <AdminUniverseHeader universe={record} section="access" />
      <SectionHeader
        title="Access"
        description="Manage explicit access for people and applications. Account Admins and Platform Admins retain access through their role."
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
          Universe Owners manage these grants. Account membership alone does not
          grant universe access to Members or Creators. Access granted by a
          configured universe policy is managed in that policy.
        </p>
        <p>
          API keys belong to a person or an application and can be limited to
          this universe. Manage{" "}
          <Link className="underline" to="/account#api-keys">
            your personal API keys
          </Link>
          {accountAdmin && accountId ? (
            <>
              {" "}
              or{" "}
              <Link
                className="underline"
                to={`/admin/accounts?account=${encodeURIComponent(accountId)}`}
              >
                account members and application keys
              </Link>
            </>
          ) : (
            "; ask an Account Admin to add people or manage application keys"
          )}
          .
        </p>
      </div>
      {[grants.error, members.error, applications.error]
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
                : `Choose an existing account member or application and its role in ${record.name}.`}
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
                Account member or application
              </FieldLabel>
              {editing ? (
                <Input
                  id="universe-access-recipient"
                  value={editing.name}
                  readOnly
                />
              ) : (
                <select
                  id="universe-access-recipient"
                  className="max-w-full rounded-md border bg-background p-2"
                  value={principal}
                  onChange={(e) => setPrincipal(e.target.value)}
                  disabled={directoriesLoading || directoriesUnavailable}
                  required
                >
                  <option value="">
                    {directoriesLoading
                      ? "Loading account members and applications…"
                      : "Select a person or application"}
                  </option>
                  {choices.map((c) => (
                    <option key={c.value} value={c.value}>
                      {c.label}
                    </option>
                  ))}
                </select>
              )}
            </Field>
            <Field className="sm:w-auto">
              <FieldLabel htmlFor="universe-access-role">
                Universe role
              </FieldLabel>
              <select
                id="universe-access-role"
                className="rounded-md border bg-background p-2"
                value={role}
                onChange={(e) => setRole(e.target.value as UniverseRole)}
              >
                {UNIVERSE_ROLES.map((r) => (
                  <option key={r} value={r}>
                    {roleLabels[r]}
                  </option>
                ))}
              </select>
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
            No account members or enabled applications are available. An Account
            Admin can add them in Accounts.
          </p>
        )}
      {!accountId && (
        <p role="alert">
          This universe has no account. Contact a Platform Admin to restore its
          account before adding access.
        </p>
      )}
      {grants.isLoading && <LoadingNote />}
      {grants.data?.grants.length === 0 && (
        <p className="mb-3 text-sm text-muted-foreground">
          No explicit universe grants. Account Admins and Platform Admins retain
          access.
        </p>
      )}
      {!!grants.data?.grants.length && (
        <TableCard>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Person or application</TableHead>
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
                    {g.principal.kind === "user" ? "Person" : "Application"}
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
              policy or an Account Admin or Platform Admin role remains.
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
        and applications.
      </p>
    </>
  );
}
