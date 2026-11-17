import { useState, type ReactNode } from "react";
import { useMutation } from "@tanstack/react-query";
import type { OrganizationRole, PlatformOperationResult } from "@lightspeed-ai/platform-shared";
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

type Organization = PlatformOperationResult<"platform/organizations/read">["organization"];
type Member = PlatformOperationResult<"platform/organizations/members/list">["members"][number];
type ServiceUser = PlatformOperationResult<"platform/organizations/service-users/list">["serviceUsers"][number];
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
        Members need universe access. Creators can create universes. Admins manage the organization and all its
        universes.
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

export function MemberDialog({
  organizationId,
  member,
  close,
  saved,
}: {
  organizationId: string;
  member?: Member;
  close: () => void;
  saved: () => Promise<void>;
}) {
  const [userId, setUserId] = useState(member?.userId ?? "");
  const [role, setRole] = useState<OrganizationRole>(member?.role ?? "member");
  const save = useMutation({
    mutationFn: () =>
      platformApi("platform/organizations/members/put", {
        organizationId,
        userId: userId.trim(),
        role,
      }),
    onSuccess: async () => {
      await saved();
      close();
    },
  });
  return (
    <OrganizationDialog
      title={member ? `Edit ${member.name}'s organization role` : "Add organization member"}
      description="Add existing users to this organization. Universe roles are managed separately in Admin → organization → Universes."
      close={close}
    >
      <form
        className="grid gap-4"
        onSubmit={(event) => {
          event.preventDefault();
          save.mutate();
        }}
      >
        {!member && (
          <Field>
            <FieldLabel htmlFor="organization-member-id">User ID</FieldLabel>
            <Input
              id="organization-member-id"
              value={userId}
              onChange={(event) => setUserId(event.target.value)}
              required
              autoFocus
            />
            <FieldDescription>
              Find the person's ID under Your profile, or Admin → Users → Edit user.
            </FieldDescription>
          </Field>
        )}
        <RoleSelect value={role} onChange={setRole} />
        {save.error && (
          <p role="alert" className="text-destructive">
            {save.error.message}
          </p>
        )}
        <FormActions close={close} pending={save.isPending} label={member ? "Save changes" : "Add member"} />
      </form>
    </OrganizationDialog>
  );
}

export function ServiceUserDialog({
  organizationId,
  serviceUser,
  close,
  saved,
}: {
  organizationId: string;
  serviceUser?: ServiceUser;
  close: () => void;
  saved: () => Promise<void>;
}) {
  const [name, setName] = useState(serviceUser?.name ?? "");
  const [role, setRole] = useState<OrganizationRole>(serviceUser?.role ?? "member");
  const save = useMutation({
    mutationFn: () =>
      serviceUser
        ? platformApi("platform/organizations/service-users/update", {
            organizationId,
            serviceUserId: serviceUser.id,
            name: name.trim(),
            role,
          })
        : platformApi("platform/organizations/service-users/create", {
            organizationId,
            name: name.trim(),
            role,
          }),
    onSuccess: async () => {
      await saved();
      close();
    },
  });
  return (
    <OrganizationDialog
      title={serviceUser ? "Edit service user" : "New service user"}
      description="A service user authenticates independently of its creator. Start with Member and grant access to the universes it needs."
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
          <FieldLabel htmlFor="serviceUser-name">Service user name</FieldLabel>
          <Input
            id="serviceUser-name"
            value={name}
            onChange={(event) => setName(event.target.value)}
            required
            autoFocus
          />
        </Field>
        <RoleSelect value={role} onChange={setRole} />
        {save.error && (
          <p role="alert" className="text-destructive">
            {save.error.message}
          </p>
        )}
        <FormActions
          close={close}
          pending={save.isPending}
          label={serviceUser ? "Save changes" : "Create service user"}
        />
      </form>
    </OrganizationDialog>
  );
}

export function PolicyDialog({
  organization,
  close,
  saved,
}: {
  organization: Organization;
  close: () => void;
  saved: () => Promise<void>;
}) {
  const [quota, setQuota] = useState(String(organization.universeQuota));
  const [defaultProtected, setDefaultProtected] = useState(organization.defaultProtected);
  const [requireProtected, setRequireProtected] = useState(organization.requireProtected);
  const save = useMutation({
    mutationFn: () =>
      platformApi("platform/organizations/update", {
        organizationId: organization.id,
        universeQuota: Number(quota),
        defaultProtected,
        requireProtected,
      }),
    onSuccess: async () => {
      await saved();
      close();
    },
  });
  return (
    <OrganizationDialog
      title="Edit organization policy"
      description={`Policy for ${organization.name}. Requiring protection applies to every universe in this organization.`}
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
          <FieldLabel htmlFor="organization-quota">Universe quota</FieldLabel>
          <Input
            id="organization-quota"
            type="number"
            value={quota}
            onChange={(event) => setQuota(event.target.value)}
            min={0}
            max={100000}
            step={1}
            required
          />
        </Field>
        <label className="flex gap-2">
          <input
            type="checkbox"
            checked={defaultProtected}
            onChange={(event) => setDefaultProtected(event.target.checked)}
          />
          Protect new universes by default
        </label>
        <label className="flex gap-2">
          <input
            type="checkbox"
            checked={requireProtected}
            onChange={(event) => setRequireProtected(event.target.checked)}
          />
          Require universe protection
        </label>
        {save.error && (
          <p role="alert" className="text-destructive">
            {save.error.message}
          </p>
        )}
        <FormActions close={close} pending={save.isPending} label="Save policy" />
      </form>
    </OrganizationDialog>
  );
}

export function ConfirmDialog({
  title,
  description,
  label,
  close,
  action,
}: {
  title: string;
  description: string;
  label: string;
  close: () => void;
  action: () => Promise<void>;
}) {
  const save = useMutation({ mutationFn: action, onSuccess: close });
  return (
    <OrganizationDialog title={title} description={description} close={close}>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          save.mutate();
        }}
        className="grid gap-4"
      >
        {save.error && (
          <p role="alert" className="text-destructive">
            {save.error.message}
          </p>
        )}
        <FormActions close={close} pending={save.isPending} label={label} />
      </form>
    </OrganizationDialog>
  );
}
