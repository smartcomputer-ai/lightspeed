import { useState, type ReactNode } from "react";
import { useMutation } from "@tanstack/react-query";
import type { OrganizationRole } from "@lightspeed-ai/platform-shared";
import { platformApi } from "@/lib/platform-api";
import { useDeploymentPermissions } from "@/lib/permissions";
import { useDeploymentUsers } from "@/lib/deployment-users";
import { Combobox, ComboboxContent, ComboboxEmpty, ComboboxInput, ComboboxItem, ComboboxList } from "@/components/ui/combobox";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Input } from "@/components/ui/input";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

const roles: OrganizationRole[] = ["member", "creator", "admin"];
const roleLabels: Record<OrganizationRole, string> = { member: "Member", creator: "Creator", admin: "Admin" };

function RoleSelect({ value, onChange }: { value: OrganizationRole; onChange: (role: OrganizationRole) => void }) {
  return (
    <Field>
      <FieldLabel htmlFor="organization-role">Organization role</FieldLabel>
      <Select
        value={value}
        onValueChange={(value) => value !== null && onChange(value as OrganizationRole)}
      >
        <SelectTrigger id="organization-role"><SelectValue /></SelectTrigger>
        <SelectContent>
          {roles.map((role) => (
            <SelectItem key={role} value={role}>
              {roleLabels[role]}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      <FieldDescription>
        Members get the organization's default universe access on every universe, plus any universe roles they are granted.
        Creators can also create universes. Admins manage the organization and all its universes.
      </FieldDescription>
    </Field>
  );
}

function OrganizationDialog({
  title,
  description,
  close,
  children,
}: {
  title: string;
  description: string;
  close: () => void;
  children: ReactNode;
}) {
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) close();
      }}
    >
      <DialogContent className="max-h-[min(92dvh,800px)] overflow-y-auto sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>{description}</DialogDescription>
        </DialogHeader>
        {children}
      </DialogContent>
    </Dialog>
  );
}

function FormActions({ close, pending, label }: { close: () => void; pending: boolean; label: string }) {
  return (
    <DialogFooter>
      <Button type="button" variant="outline" onClick={close} disabled={pending}>
        Cancel
      </Button>
      <Button type="submit" disabled={pending}>
        {pending ? "Saving…" : label}
      </Button>
    </DialogFooter>
  );
}

export function CreateOrganizationDialog({
  close,
  saved,
}: {
  close: () => void;
  saved: (organizationId: string) => Promise<void>;
}) {
  const [name, setName] = useState("");
  const [slug, setSlug] = useState("");
  const create = useMutation({
    mutationFn: () =>
      platformApi("platform/organizations/create", {
        name: name.trim(),
        slug,
      }),
    onSuccess: (result) => saved(result.organization.id),
  });
  return (
    <OrganizationDialog
      title="New organization"
      description="Create an organization for a team or business unit. You become its first Organization Admin."
      close={close}
    >
      <form
        className="grid gap-4"
        onSubmit={(event) => {
          event.preventDefault();
          create.mutate();
        }}
      >
        <Field>
          <FieldLabel htmlFor="new-organization-name">Name</FieldLabel>
          <Input
            id="new-organization-name"
            aria-label="New organization name"
            value={name}
            onChange={(event) => setName(event.target.value)}
            required
            autoFocus
          />
        </Field>
        <Field>
          <FieldLabel htmlFor="new-organization-slug">Handle</FieldLabel>
          <Input
            id="new-organization-slug"
            aria-label="New organization slug"
            value={slug}
            onChange={(event) => setSlug(event.target.value)}
            required
            pattern="[a-z0-9]+(?:-[a-z0-9]+)*"
            placeholder="team-name"
          />
        </Field>
        {create.error && (
          <p role="alert" className="text-destructive">
            {create.error.message}
          </p>
        )}
        <FormActions close={close} pending={create.isPending} label="Create organization" />
      </form>
    </OrganizationDialog>
  );
}

/// Adds an existing user. Those who manage the deployment's users pick from
/// them; others enter the user ID the person copies from their Settings.
export function AddPersonDialog({
  organizationId,
  memberIds,
  close,
  saved,
}: {
  organizationId: string;
  memberIds: string[];
  close: () => void;
  saved: () => Promise<void>;
}) {
  const picksUsers = useDeploymentPermissions().can("manageUsers");
  const [userId, setUserId] = useState("");
  const [role, setRole] = useState<OrganizationRole>("member");
  const save = useMutation({
    mutationFn: () =>
      platformApi("platform/organizations/members/put", { organizationId, userId: userId.trim(), role }),
    onSuccess: async () => {
      await saved();
      close();
    },
  });
  return (
    <OrganizationDialog
      title="Add person"
      description="Add someone who already has an account on this deployment."
      close={close}
    >
      <form
        className="grid gap-4"
        onSubmit={(event) => {
          event.preventDefault();
          if (userId.trim()) save.mutate();
        }}
      >
        {picksUsers ? (
          <UserPicker memberIds={memberIds} value={userId} onChange={setUserId} />
        ) : (
          <Field>
            <FieldLabel htmlFor="organization-member-id">User ID</FieldLabel>
            <Input id="organization-member-id" value={userId} onChange={(event) => setUserId(event.target.value)} required autoFocus />
            <FieldDescription>They find it under Settings → Profile.</FieldDescription>
          </Field>
        )}
        <RoleSelect value={role} onChange={setRole} />
        {save.error && (
          <p role="alert" className="text-destructive">
            {save.error.message}
          </p>
        )}
        <FormActions close={close} pending={save.isPending} label="Add person" />
      </form>
    </OrganizationDialog>
  );
}

/// Searches the deployment's users who are not yet members, by name or email.
function UserPicker({ memberIds, value, onChange }: { memberIds: string[]; value: string; onChange: (userId: string) => void }) {
  const users = useDeploymentUsers();
  const members = new Set(memberIds);
  const candidates = (users.data ?? []).filter((user) => !members.has(user.id) && !user.banned);
  const byId = new Map(candidates.map((user) => [user.id, user]));
  const label = (id: string) => { const user = byId.get(id); return user ? `${user.name} (${user.email})` : id; };
  return (
    <Field>
      <FieldLabel htmlFor="organization-member-user">Person</FieldLabel>
      <Combobox<string> items={candidates.map((user) => user.id)} value={value || null}
        itemToStringLabel={label}
        filter={(id, query) => label(id).toLowerCase().includes(query.toLowerCase())}
        onValueChange={(id) => onChange(id ?? "")}>
        <ComboboxInput id="organization-member-user" placeholder={users.isLoading ? "Loading users…" : "Search by name or email"} autoFocus />
        <ComboboxContent>
          <ComboboxEmpty>No matching users who are not already members.</ComboboxEmpty>
          <ComboboxList>{(id: string) => {
            const user = byId.get(id)!;
            return (
              <ComboboxItem key={id} value={id}>
                <span className="min-w-0">
                  <span className="block truncate">{user.name}</span>
                  <span className="block truncate text-xs text-muted-foreground">{user.email}</span>
                </span>
              </ComboboxItem>
            );
          }}</ComboboxList>
        </ComboboxContent>
      </Combobox>
      {users.error && <p role="alert" className="text-sm text-destructive">Users could not be loaded: {users.error.message}</p>}
    </Field>
  );
}

/// Creates a service user with an organization role; its keys are created from its details.
export function NewServiceUserDialog({
  organizationId,
  close,
  saved,
}: {
  organizationId: string;
  close: () => void;
  saved: () => Promise<void>;
}) {
  const [name, setName] = useState("");
  const [role, setRole] = useState<OrganizationRole>("member");
  const save = useMutation({
    mutationFn: () =>
      platformApi("platform/organizations/service-users/create", { organizationId, name: name.trim(), role }),
    onSuccess: async () => {
      await saved();
      close();
    },
  });
  return (
    <OrganizationDialog
      title="New service user"
      description="An identity for an integration, independent of who creates it. Start with Member, grant it the universes it needs, then create its API keys from its details."
      close={close}
    >
      <form
        className="grid gap-4"
        onSubmit={(event) => {
          event.preventDefault();
          save.mutate();
        }}
      >
        <Field>
          <FieldLabel htmlFor="serviceUser-name">Name</FieldLabel>
          <Input id="serviceUser-name" value={name} onChange={(event) => setName(event.target.value)} required autoFocus />
        </Field>
        <RoleSelect value={role} onChange={setRole} />
        {save.error && (
          <p role="alert" className="text-destructive">
            {save.error.message}
          </p>
        )}
        <FormActions close={close} pending={save.isPending} label="Create service user" />
      </form>
    </OrganizationDialog>
  );
}
