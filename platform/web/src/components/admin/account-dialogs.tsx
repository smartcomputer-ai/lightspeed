import { useState, type ReactNode } from "react";
import { useMutation } from "@tanstack/react-query";
import type { AccountRole, PlatformOperationResult } from "@lightspeed-ai/platform-shared";
import { platformApi } from "@/lib/platform-api";
import { Button } from "@/components/ui/button";
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

type Account = PlatformOperationResult<"platform/accounts/read">["account"];
type Member = PlatformOperationResult<"platform/accounts/members/list">["members"][number];
type Application = PlatformOperationResult<"platform/accounts/applications/list">["applications"][number];
const roles: AccountRole[] = ["member", "creator", "admin"];
const roleLabels: Record<AccountRole, string> = { member: "Member", creator: "Creator", admin: "Admin" };

function RoleSelect({ value, onChange }: { value: AccountRole; onChange: (role: AccountRole) => void }) {
  return (
    <Field>
      <FieldLabel htmlFor="account-role">Account role</FieldLabel>
      <select
        id="account-role"
        className="min-w-0 rounded-md border bg-background p-2"
        value={value}
        onChange={(event) => onChange(event.target.value as AccountRole)}
      >
        {roles.map((role) => (
          <option key={role} value={role}>
            {roleLabels[role]}
          </option>
        ))}
      </select>
      <FieldDescription>
        Members need universe access. Creators can create universes. Admins manage the account and all its
        universes.
      </FieldDescription>
    </Field>
  );
}

function AccountDialog({
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

export function CreateAccountDialog({
  close,
  saved,
}: {
  close: () => void;
  saved: (accountId: string) => Promise<void>;
}) {
  const [name, setName] = useState("");
  const [slug, setSlug] = useState("");
  const create = useMutation({
    mutationFn: () =>
      platformApi("platform/accounts/create", {
        name: name.trim(),
        slug,
        kind: "shared",
      }),
    onSuccess: (result) => saved(result.account.id),
  });
  return (
    <AccountDialog
      title="New account"
      description="Create a shared account for a team or business unit. You become its first Account Admin."
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
          <FieldLabel htmlFor="new-account-name">Name</FieldLabel>
          <Input
            id="new-account-name"
            aria-label="New account name"
            value={name}
            onChange={(event) => setName(event.target.value)}
            required
            autoFocus
          />
        </Field>
        <Field>
          <FieldLabel htmlFor="new-account-slug">Handle</FieldLabel>
          <Input
            id="new-account-slug"
            aria-label="New account slug"
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
        <FormActions close={close} pending={create.isPending} label="Create account" />
      </form>
    </AccountDialog>
  );
}

export function MemberDialog({
  accountId,
  member,
  close,
  saved,
}: {
  accountId: string;
  member?: Member;
  close: () => void;
  saved: () => Promise<void>;
}) {
  const [userId, setUserId] = useState(member?.userId ?? "");
  const [role, setRole] = useState<AccountRole>(member?.role ?? "member");
  const save = useMutation({
    mutationFn: () =>
      platformApi("platform/accounts/members/put", {
        accountId,
        userId: userId.trim(),
        role,
      }),
    onSuccess: async () => {
      await saved();
      close();
    },
  });
  return (
    <AccountDialog
      title={member ? `Edit ${member.name}'s account role` : "Add account member"}
      description="Add existing users to this account. Universe roles are managed separately in Admin → Universes."
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
            <FieldLabel htmlFor="account-member-id">User ID</FieldLabel>
            <Input
              id="account-member-id"
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
    </AccountDialog>
  );
}

export function ApplicationDialog({
  accountId,
  application,
  close,
  saved,
}: {
  accountId: string;
  application?: Application;
  close: () => void;
  saved: () => Promise<void>;
}) {
  const [name, setName] = useState(application?.name ?? "");
  const [role, setRole] = useState<AccountRole>(application?.role ?? "member");
  const save = useMutation({
    mutationFn: () =>
      application
        ? platformApi("platform/accounts/applications/update", {
            accountId,
            applicationId: application.id,
            name: name.trim(),
            role,
          })
        : platformApi("platform/accounts/applications/create", {
            accountId,
            name: name.trim(),
            role,
          }),
    onSuccess: async () => {
      await saved();
      close();
    },
  });
  return (
    <AccountDialog
      title={application ? "Edit application" : "New application"}
      description="An application authenticates independently of its creator. Start with Member and grant access to the universes it needs."
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
          <FieldLabel htmlFor="application-name">Application name</FieldLabel>
          <Input
            id="application-name"
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
          label={application ? "Save changes" : "Create application"}
        />
      </form>
    </AccountDialog>
  );
}

export function PolicyDialog({
  account,
  close,
  saved,
}: {
  account: Account;
  close: () => void;
  saved: () => Promise<void>;
}) {
  const [quota, setQuota] = useState(String(account.universeQuota));
  const [defaultProtected, setDefaultProtected] = useState(account.defaultProtected);
  const [requireProtected, setRequireProtected] = useState(account.requireProtected);
  const save = useMutation({
    mutationFn: () =>
      platformApi("platform/accounts/update", {
        accountId: account.id,
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
    <AccountDialog
      title="Edit account policy"
      description={`Policy for ${account.name}. Requiring protection applies to every universe in this account.`}
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
          <FieldLabel htmlFor="account-quota">Universe quota</FieldLabel>
          <Input
            id="account-quota"
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
    </AccountDialog>
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
    <AccountDialog title={title} description={description} close={close}>
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
    </AccountDialog>
  );
}
