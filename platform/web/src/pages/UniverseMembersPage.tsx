import { useRef, useState, type FormEvent } from "react";
import { Link } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { PlatformOperationResult, PrincipalRef, UniverseRole } from "@lightspeed-ai/platform-shared";
import type { Universe } from "@/api";
import { platformApi, useOrganizations } from "@/lib/platform-api";
import { organizationAllows, useActionPermissions } from "@/lib/permissions";
import { organizationAdminPath } from "@/lib/admin-access";
import { useActiveUniverse } from "@/lib/universes";
import { AddIcon, MemberIcon } from "@/icons";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Field, FieldLabel } from "@/components/ui/field";
import { SaveButton } from "@/components/ui/save-button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { TableCell } from "@/components/ui/table";
import { ConfirmDangerButton } from "@/components/confirm-danger-button";
import { EmptyState, LoadingNote, PageHeader, UniverseNotFound } from "@/components/page";
import { ReadError } from "@/components/read-error";
import { ResourceActions, ResourceDetail, ResourceDetailsDialog, ResourceDetailsGrid } from "@/components/resource-details-dialog";
import { ResourceIdentityCell, ResourceRow, ResourceTable, ResourceValue } from "@/components/resource-table";

type Grant = PlatformOperationResult<"platform/universes/grants/list">["grants"][number];

const ROLE_LABELS: Record<UniverseRole, string> = { viewer: "Viewer", contributor: "Contributor", operator: "Operator", owner: "Owner" };
const ROLES: UniverseRole[] = ["viewer", "contributor", "operator", "owner"];

/// Who can use a universe beyond the organization's default access, which
/// every person gets, and Organization Admins, who are always Owners.
export function UniverseMembersPage() {
  const { universe, handle, isLoading } = useActiveUniverse();
  const permissions = useActionPermissions(universe?.id);
  if (isLoading) return <LoadingNote />;
  if (!universe || !permissions.can("administer")) return <UniverseNotFound handle={handle} />;
  return <Members key={universe.id} universe={universe} />;
}

function Members({ universe }: { universe: Universe }) {
  const client = useQueryClient();
  const universeId = universe.lightspeedUniverseId;
  const organizationId = universe.organizationId!;
  const organization = useOrganizations().data?.organizations.find((candidate) => candidate.id === organizationId);
  const grants = useQuery({
    queryKey: ["grants", universeId],
    queryFn: () => platformApi("platform/universes/grants/list", { universe: universeId }),
  });
  const people = useQuery({
    queryKey: ["organization-members", organizationId],
    queryFn: () => platformApi("platform/organizations/members/list", { organizationId }),
  });
  const serviceUsers = useQuery({
    queryKey: ["service-users", organizationId],
    queryFn: () => platformApi("platform/organizations/service-users/list", { organizationId }),
  });
  const [adding, setAdding] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const triggerRef = useRef<HTMLElement | null>(null);
  const selected = grants.data?.grants.find((grant) => grant.id === selectedId) ?? null;
  const refresh = () => Promise.all([
    client.invalidateQueries({ queryKey: ["grants", universeId] }),
    client.invalidateQueries({ queryKey: ["universes"] }),
  ]);
  const email = (grant: Grant) => grant.principal.kind === "user"
    ? people.data?.members.find((member) => member.userId === grant.principal.id)?.email
    : undefined;
  const base = organization?.baseUniverseRole ?? null;
  const organizationName = organization?.name ?? "the organization";

  return (
    <>
      <PageHeader title="Universe Members"
        description={(
          <>
            {organization?.personal
              ? "This universe is in a personal organization: its owner and the service users added here can use it."
              : base
                ? `Everyone in ${organizationName} can use this universe as a ${ROLE_LABELS[base]}, and its admins as Owners. Add members here to set permissions explicitly.`
                : `Only the members below can use this universe, plus ${organizationName}'s admins as Owners.`}
            {organization && !organization.personal && organizationAllows(organization, "managePolicy") && (
              <> <Link className="underline" to={`${organizationAdminPath(organizationId)}/settings`}>Change the default</Link></>
            )}
          </>
        )}
        actions={(
          <Button onClick={() => setAdding(true)}>
            <AddIcon data-icon="inline-start" />
            Add member
          </Button>
        )} />
      {grants.isPending && <LoadingNote />}
      {grants.error && <ReadError error={grants.error} loading={!grants.data} prefix="Members unavailable" />}
      {grants.data?.grants.length === 0 && <EmptyState icon={MemberIcon} title="No members added" />}
      {!!grants.data?.grants.length && (
        <ResourceTable label="Universe members" columns={["Member", "Role"]}>
          {grants.data.grants.map((grant) => (
            <ResourceRow key={grant.id} name={grant.name} onOpen={(trigger) => { triggerRef.current = trigger; setSelectedId(grant.id); }}>
              <ResourceIdentityCell name={grant.name} subtitle={grant.principal.kind === "serviceUser" ? "Service user" : email(grant)} />
              <TableCell><ResourceValue description={grant.active ? undefined : inactiveNote(grant)}>{ROLE_LABELS[grant.role]}</ResourceValue></TableCell>
            </ResourceRow>
          ))}
        </ResourceTable>
      )}
      {adding && (
        <AddMemberDialog universeId={universeId} grants={grants.data?.grants ?? []} close={() => setAdding(false)} saved={refresh}
          people={people} serviceUsers={serviceUsers} />
      )}
      <ResourceDetailsDialog open={selected !== null} onOpenChange={(open) => { if (!open) setSelectedId(null); }} finalFocus={triggerRef}
        title={selected?.name ?? "Member"} description={selected?.principal.kind === "serviceUser" ? "Service user" : (selected && email(selected)) ?? "Person"}>
        {selected && <GrantDetails key={selected.id} universeId={universeId} grant={selected} saved={refresh} removed={() => setSelectedId(null)} />}
      </ResourceDetailsDialog>
    </>
  );
}

/// Grants outlive the membership that made them useful, until removed here.
function inactiveNote(grant: Grant) {
  return grant.principal.kind === "serviceUser" ? "Service user disabled" : "No longer an organization member";
}

function RoleSelect({ id, value, onChange }: { id: string; value: UniverseRole; onChange: (role: UniverseRole) => void }) {
  return (
    <Select value={value} onValueChange={(next) => next !== null && onChange(next as UniverseRole)}>
      <SelectTrigger id={id}><SelectValue /></SelectTrigger>
      <SelectContent>
        {ROLES.map((role) => <SelectItem key={role} value={role}>{ROLE_LABELS[role]}</SelectItem>)}
      </SelectContent>
    </Select>
  );
}

function GrantDetails({ universeId, grant, saved, removed }: { universeId: string; grant: Grant; saved: () => Promise<unknown>; removed: () => void }) {
  const [role, setRole] = useState<UniverseRole>(grant.role);
  const put = useMutation({
    mutationFn: () => platformApi("platform/universes/grants/put", { universe: universeId, principal: grant.principal, role }),
    onSuccess: () => void saved(),
  });
  const remove = useMutation({
    mutationFn: () => platformApi("platform/universes/grants/delete", { universe: universeId, grantId: grant.id }),
    onSuccess: async () => { await saved(); removed(); },
  });
  const submit = (event: FormEvent) => { event.preventDefault(); if (role !== grant.role) put.mutate(); };
  return (
    <>
      <ResourceDetailsGrid>
        <ResourceDetail label="Kind">{grant.principal.kind === "serviceUser" ? "Service user" : "Person"}</ResourceDetail>
        <ResourceDetail label="Permission granted">{new Date(grant.createdAt).toLocaleDateString()}</ResourceDetail>
      </ResourceDetailsGrid>
      {!grant.active && (
        <p className="text-sm text-muted-foreground">
          {inactiveNote(grant)}, so this role gives nothing until they return.
        </p>
      )}
      <form onSubmit={submit} className="grid gap-2">
        <div className="flex flex-wrap items-end gap-3">
          <Field className="min-w-40 flex-1">
            <FieldLabel htmlFor="grant-role">Role in this universe</FieldLabel>
            <RoleSelect id="grant-role" value={role} onChange={setRole} />
          </Field>
          <SaveButton type="submit" dirty={role !== grant.role} saving={put.isPending} error={put.isError} />
        </div>
        {put.error && <p role="alert" className="text-sm text-destructive">{put.error.message}</p>}
      </form>
      <ResourceActions>
        <ConfirmDangerButton label="Remove role" title={`Remove ${grant.name}'s role?`} pending={remove.isPending}
          description={grant.principal.kind === "serviceUser"
            ? "The service user loses access to this universe."
            : "They keep the organization's default access."}
          onConfirm={() => remove.mutate()} />
        {remove.error && <p role="alert" className="text-sm text-destructive">{remove.error.message}</p>}
      </ResourceActions>
    </>
  );
}

type Directory<T> = { data?: T; isLoading: boolean; error: Error | null };

function AddMemberDialog({ universeId, grants, close, saved, people, serviceUsers }: {
  universeId: string;
  grants: Grant[];
  close: () => void;
  saved: () => Promise<unknown>;
  people: Directory<PlatformOperationResult<"platform/organizations/members/list">>;
  serviceUsers: Directory<PlatformOperationResult<"platform/organizations/service-users/list">>;
}) {
  const [principal, setPrincipal] = useState("");
  const [role, setRole] = useState<UniverseRole>("contributor");
  const given = new Set(grants.map((grant) => `${grant.principal.kind}:${grant.principal.id}`));
  const choices = [
    ...(people.data?.members ?? []).map((member) => ({ value: `user:${member.userId}`, label: `${member.name} (${member.email})` })),
    ...(serviceUsers.data?.serviceUsers ?? []).filter((candidate) => candidate.enabled)
      .map((candidate) => ({ value: `serviceUser:${candidate.id}`, label: `${candidate.name} (service user)` })),
  ].filter((choice) => !given.has(choice.value));
  const loading = people.isLoading || serviceUsers.isLoading;
  const unavailable = !!people.error || !!serviceUsers.error;
  const put = useMutation({
    mutationFn: () => {
      const split = principal.indexOf(":");
      return platformApi("platform/universes/grants/put", {
        universe: universeId,
        principal: { kind: principal.slice(0, split) as PrincipalRef["kind"], id: principal.slice(split + 1) },
        role,
      });
    },
    onSuccess: async () => { await saved(); close(); },
  });
  return (
    <Dialog open onOpenChange={(open) => { if (!open) close(); }}>
      <DialogContent className="max-h-[calc(100dvh-2rem)] overflow-y-auto sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Add member</DialogTitle>
          <DialogDescription>Give a person in the organization, or one of its service users, a role in this universe.</DialogDescription>
        </DialogHeader>
        <form className="grid gap-4" onSubmit={(event) => {
          event.preventDefault();
          if (choices.some((choice) => choice.value === principal)) put.mutate();
        }}>
          <Field>
            <FieldLabel htmlFor="universe-member">Person or service user</FieldLabel>
            <Select value={principal} onValueChange={(value) => value !== null && setPrincipal(value)} disabled={loading || unavailable}>
              <SelectTrigger id="universe-member"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="">{loading ? "Loading the organization's members…" : "Select a person or service user"}</SelectItem>
                {choices.map((choice) => <SelectItem key={choice.value} value={choice.value}>{choice.label}</SelectItem>)}
              </SelectContent>
            </Select>
          </Field>
          <Field>
            <FieldLabel htmlFor="universe-member-role">Role</FieldLabel>
            <RoleSelect id="universe-member-role" value={role} onChange={setRole} />
          </Field>
          {unavailable && <p role="alert" className="text-sm text-destructive">The organization's members could not be loaded.</p>}
          {put.error && <p role="alert" className="text-sm text-destructive">{put.error.message}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={close} disabled={put.isPending}>Cancel</Button>
            <Button type="submit" disabled={put.isPending || !choices.some((choice) => choice.value === principal)}>
              {put.isPending ? "Adding…" : "Add member"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
