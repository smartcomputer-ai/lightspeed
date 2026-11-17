import { useState, type ReactNode } from "react";
import { useMutation } from "@tanstack/react-query";
import type { OrganizationRole } from "@lightspeed-ai/platform-shared";
import { platformApi } from "@/lib/platform-api";
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

/// Adds an existing user, by user ID, with an organization role.
export function AddPersonDialog({
  organizationId,
  close,
  saved,
}: {
  organizationId: string;
  close: () => void;
  saved: () => Promise<void>;
}) {
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
      description="Add an existing user to this organization. Give universe roles under a universe's Settings → Members."
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
          <FieldLabel htmlFor="organization-member-id">User ID</FieldLabel>
          <Input id="organization-member-id" value={userId} onChange={(event) => setUserId(event.target.value)} required autoFocus />
          <FieldDescription>They find it under Your profile; Platform Admins see it under Platform → Users.</FieldDescription>
        </Field>
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
