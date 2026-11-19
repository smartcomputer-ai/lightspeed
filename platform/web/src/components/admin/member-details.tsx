import { useState, type FormEvent, type RefObject } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import type { OrganizationRole, PlatformOperationResult } from "@lightspeed-ai/platform-shared";
import { platformApi } from "@/lib/platform-api";
import { Button } from "@/components/ui/button";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { SaveButton } from "@/components/ui/save-button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { ConfirmDangerButton } from "@/components/confirm-danger-button";
import { ResourceActions, ResourceDetail, ResourceDetailsDialog, ResourceDetailsGrid } from "@/components/resource-details-dialog";
import { ResourceStatus } from "@/components/resource-table";
import { FederatedCredentials } from "./federated-credentials";
import { ApiKeysSection } from "@/components/api-keys/api-keys-section";
import type { ResourceStatusInfo } from "@/lib/resource-status";

export type Member = PlatformOperationResult<"platform/organizations/members/list">["members"][number];
export type ServiceUser = PlatformOperationResult<"platform/organizations/service-users/list">["serviceUsers"][number];

export const ROLE_LABELS: Record<OrganizationRole, string> = { member: "Member", creator: "Creator", admin: "Admin" };
const ROLES: OrganizationRole[] = ["member", "creator", "admin"];
const ROLE_HINT = "Members get the organization's default universe access. Creators can also create universes. Admins manage the organization and all its universes.";

/// How a person joined: added by an admin, granted by the membership
/// policy, or both. Only an added membership can be removed here.
export function joinedVia(member: Member): string {
  if (member.added && member.declared) return "An admin, and the membership policy";
  return member.added ? "An admin" : "The membership policy";
}

export function serviceUserStatus(serviceUser: ServiceUser): ResourceStatusInfo {
  return serviceUser.enabled ? { label: "Active", tone: "positive" } : { label: "Disabled", tone: "neutral" };
}

function useRefreshMembers(organizationId: string) {
  const client = useQueryClient();
  return () => Promise.all([
    client.invalidateQueries({ queryKey: ["organization-members", organizationId] }),
    client.invalidateQueries({ queryKey: ["service-users", organizationId] }),
    client.invalidateQueries({ queryKey: ["universes"] }),
  ]);
}

function RoleField({ id, value, onChange, disabled }: { id: string; value: OrganizationRole; onChange: (role: OrganizationRole) => void; disabled?: boolean }) {
  return (
    <Field className="min-w-40 flex-1">
      <FieldLabel htmlFor={id}>Organization role</FieldLabel>
      <Select value={value} disabled={disabled} onValueChange={(next) => next !== null && onChange(next as OrganizationRole)}>
        <SelectTrigger id={id}><SelectValue /></SelectTrigger>
        <SelectContent>
          {ROLES.map((role) => <SelectItem key={role} value={role}>{ROLE_LABELS[role]}</SelectItem>)}
        </SelectContent>
      </Select>
    </Field>
  );
}

// ----- People -----

export function PersonDetailsDialog({ organizationId, personal, member, onOpenChange, finalFocus }: {
  organizationId: string;
  personal: boolean;
  member: Member | null;
  onOpenChange: (open: boolean) => void;
  finalFocus?: RefObject<HTMLElement | null>;
}) {
  return (
    <ResourceDetailsDialog open={member !== null} onOpenChange={onOpenChange} finalFocus={finalFocus}
      title={member?.name ?? "Person"} description={member?.email ?? ""}>
      {member && <PersonDetails key={member.id} organizationId={organizationId} personal={personal} member={member} onRemoved={() => onOpenChange(false)} />}
    </ResourceDetailsDialog>
  );
}

function PersonDetails({ organizationId, personal, member, onRemoved }: { organizationId: string; personal: boolean; member: Member; onRemoved: () => void }) {
  const refresh = useRefreshMembers(organizationId);
  const [role, setRole] = useState<OrganizationRole>(member.role);
  const save = useMutation({
    mutationFn: () => platformApi("platform/organizations/members/put", { organizationId, userId: member.userId, role }),
    onSuccess: () => void refresh(),
  });
  const remove = useMutation({
    mutationFn: () => platformApi("platform/organizations/members/delete", { organizationId, userId: member.userId }),
    onSuccess: async () => { await refresh(); onRemoved(); },
  });
  const submit = (event: FormEvent) => { event.preventDefault(); if (role !== member.role) save.mutate(); };
  return (
    <>
      <ResourceDetailsGrid>
        <ResourceDetail label="Email">{member.email}</ResourceDetail>
        <ResourceDetail label="User ID" mono>{member.userId}</ResourceDetail>
        <ResourceDetail label="Joined via">{personal ? "Owner of this personal organization" : joinedVia(member)}</ResourceDetail>
        <ResourceDetail label="Member since">{new Date(member.createdAt).toLocaleDateString()}</ResourceDetail>
      </ResourceDetailsGrid>
      {!personal && (
        <form onSubmit={submit} className="grid gap-2">
          <div className="flex flex-wrap items-end gap-3">
            <RoleField id="person-role" value={role} onChange={setRole} />
            <SaveButton type="submit" dirty={role !== member.role} saving={save.isPending} error={save.isError} />
          </div>
          <p className="text-sm text-muted-foreground">{ROLE_HINT}{member.declared && " The membership policy can give a higher role than the one set here."}</p>
          {save.error && <p role="alert" className="text-sm text-destructive">{save.error.message}</p>}
        </form>
      )}
      {!personal && (
        <ResourceActions>
          {member.added ? (
            <ConfirmDangerButton label="Remove from organization" title={`Remove ${member.name}?`} pending={remove.isPending}
              description={member.declared
                ? "The membership policy still makes them a member, so they keep their access."
                : "They lose access to this organization's universes, and the roles they were given in them."}
              onConfirm={() => remove.mutate()} />
          ) : (
            <p className="text-sm text-muted-foreground">The membership policy makes them a member. Change the policy to remove them.</p>
          )}
          {remove.error && <p role="alert" className="text-sm text-destructive">{remove.error.message}</p>}
        </ResourceActions>
      )}
    </>
  );
}

// ----- Service users -----

export function ServiceUserDetailsDialog({ organizationId, serviceUser, onOpenChange, finalFocus }: {
  organizationId: string;
  serviceUser: ServiceUser | null;
  onOpenChange: (open: boolean) => void;
  finalFocus?: RefObject<HTMLElement | null>;
}) {
  return (
    <ResourceDetailsDialog open={serviceUser !== null} onOpenChange={onOpenChange} finalFocus={finalFocus}
      title={serviceUser?.name ?? "Service user"} description="An identity for an integration, with its own role and API keys.">
      {serviceUser && <ServiceUserDetails key={serviceUser.id} organizationId={organizationId} serviceUser={serviceUser} />}
    </ResourceDetailsDialog>
  );
}

function ServiceUserDetails({ organizationId, serviceUser }: { organizationId: string; serviceUser: ServiceUser }) {
  const refresh = useRefreshMembers(organizationId);
  const [name, setName] = useState(serviceUser.name);
  const [role, setRole] = useState<OrganizationRole>(serviceUser.role);
  const dirty = (name.trim() !== serviceUser.name && !!name.trim()) || role !== serviceUser.role;
  const save = useMutation({
    mutationFn: () => platformApi("platform/organizations/service-users/update", { organizationId, serviceUserId: serviceUser.id, name: name.trim(), role }),
    onSuccess: () => void refresh(),
  });
  const setEnabled = useMutation({
    mutationFn: (enabled: boolean) => platformApi("platform/organizations/service-users/update", { organizationId, serviceUserId: serviceUser.id, enabled }),
    onSuccess: () => void refresh(),
  });
  const submit = (event: FormEvent) => { event.preventDefault(); if (dirty) save.mutate(); };
  return (
    <>
      <ResourceDetailsGrid>
        <ResourceDetail label="Service user ID" mono>{serviceUser.id}</ResourceDetail>
        <ResourceDetail label="Status"><ResourceStatus status={serviceUserStatus(serviceUser)} /></ResourceDetail>
        <ResourceDetail label="Created">{new Date(serviceUser.createdAt).toLocaleDateString()}</ResourceDetail>
      </ResourceDetailsGrid>
      <form onSubmit={submit} className="grid gap-2">
        <div className="flex flex-wrap items-end gap-3">
          <Field className="min-w-40 flex-1">
            <FieldLabel htmlFor="service-user-name">Name</FieldLabel>
            <Input id="service-user-name" value={name} onChange={(event) => setName(event.target.value)} required />
          </Field>
          <RoleField id="service-user-role" value={role} onChange={setRole} />
          <SaveButton type="submit" dirty={dirty} saving={save.isPending} error={save.isError} />
        </div>
        <p className="text-sm text-muted-foreground">Service users get only the universe roles granted under a universe's Settings → Members, not the default universe access.</p>
        {save.error && <p role="alert" className="text-sm text-destructive">{save.error.message}</p>}
      </form>
      <FederatedCredentials organizationId={organizationId} serviceUserId={serviceUser.id} />
      <ApiKeysSection owner={{ kind: "serviceUser", organizationId, serviceUserId: serviceUser.id, name: serviceUser.name, enabled: serviceUser.enabled }}
        description="Keys sign in as this service user, in this organization only." />
      <ResourceActions>
        {serviceUser.enabled ? (
          <ConfirmDangerButton label="Disable" title={`Disable ${serviceUser.name}?`} pending={setEnabled.isPending}
            description="Its API keys stop working until you enable it again. Its universe roles and the universes it created stay."
            onConfirm={() => setEnabled.mutate(false)} />
        ) : (
          <Button variant="outline" size="sm" disabled={setEnabled.isPending} onClick={() => setEnabled.mutate(true)}>Enable</Button>
        )}
        {setEnabled.error && <p role="alert" className="text-sm text-destructive">{setEnabled.error.message}</p>}
      </ResourceActions>
    </>
  );
}
