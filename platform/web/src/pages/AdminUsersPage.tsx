import { ReadError } from "@/components/read-error";
import { useRef, useState, type FormEvent } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { AddIcon } from "@/icons";
import { authClient, useLoginConfig, type SessionUser } from "@/auth";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { SaveButton } from "@/components/ui/save-button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { TableCell } from "@/components/ui/table";
import { ConfirmDangerButton } from "@/components/confirm-danger-button";
import { ResourceActions, ResourceDetail, ResourceDetailsDialog, ResourceDetailsGrid } from "@/components/resource-details-dialog";
import { ResourceIdentityCell, ResourceRow, ResourceStatus, ResourceStatusBadge, ResourceTable, ResourceValue } from "@/components/resource-table";
import type { ResourceStatusInfo } from "@/lib/resource-status";
import { useDeploymentUsers, type DeploymentUser } from "@/lib/deployment-users";
import { LoadingNote, PageHeader } from "@/components/page";

type UserRow = DeploymentUser;

const isPlatformAdmin = (user: UserRow) => user.role?.split(",").includes("admin") === true;

function signIn(user: UserRow): string {
  return user.identitySource === "company" ? "Company" : user.emergencyAdmin ? "Emergency admin" : "Local";
}

function userStatus(user: UserRow): ResourceStatusInfo {
  if (user.banned) return { label: "Suspended", tone: "neutral" };
  if (user.identitySource === "company" && !user.companyAdmitted) return { label: "Company access absent", tone: "warning" };
  return { label: "Active", tone: "positive" };
}

/// Everyone who can sign in to the deployment; each row opens the user.
export function AdminUsersPage({ currentUser }: { currentUser: SessionUser }) {
  const config = useLoginConfig();
  const users = useDeploymentUsers();

  const [createOpen, setCreateOpen] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const triggerRef = useRef<HTMLElement | null>(null);
  const selected = users.data?.find((user) => user.id === selectedId) ?? null;

  return (
    <>
      <PageHeader
        title="Users"
        description={config.data?.sso
          ? "Company users appear after their first admitted sign-in. Add them to organizations under Admin → organization → Members."
          : "People who can sign in to this deployment. Add them to organizations under Admin → organization → Members."}
        actions={
          config.data && config.data.password !== "off" && <Button onClick={() => setCreateOpen(true)}>
            <AddIcon data-icon="inline-start" />
            {config.data.sso ? "Create emergency admin" : "New user"}
          </Button>
        }
      />
      <CreateUserDialog open={createOpen} onOpenChange={setCreateOpen} sso={config.data?.sso ?? false} />
      {users.isLoading && <LoadingNote />}
      {users.error && <ReadError error={users.error} loading={!users.data} />}
      {!!users.data?.length && (
        <ResourceTable label="Users" columns={["User", "Sign-in", "Platform role", "Status"]}>
          {users.data.map((user) => (
            <ResourceRow key={user.id} name={user.email} onOpen={(trigger) => { triggerRef.current = trigger; setSelectedId(user.id); }}>
              <ResourceIdentityCell name={user.name} subtitle={user.email} />
              <TableCell className="hidden sm:table-cell">
                <ResourceValue description={user.identitySource === "company" && user.providerCheckedAt ? `Checked ${new Date(user.providerCheckedAt).toLocaleDateString()}` : undefined}>{signIn(user)}</ResourceValue>
              </TableCell>
              <TableCell className="hidden sm:table-cell"><ResourceValue>{isPlatformAdmin(user) ? "Platform Admin" : "User"}</ResourceValue></TableCell>
              <TableCell><ResourceStatusBadge status={userStatus(user)} /></TableCell>
            </ResourceRow>
          ))}
        </ResourceTable>
      )}
      <ResourceDetailsDialog open={selected !== null} onOpenChange={(open) => { if (!open) setSelectedId(null); }} finalFocus={triggerRef}
        title={selected?.name ?? "User"} description={selected?.email ?? ""}>
        {selected && <UserDetails key={selected.id} user={selected} currentUserId={currentUser.id} passwordMode={config.data?.password} />}
      </ResourceDetailsDialog>
    </>
  );
}

function UserDetails({ user, currentUserId, passwordMode }: {
  user: UserRow;
  currentUserId: string;
  passwordMode?: "local" | "break-glass" | "off";
}) {
  const queryClient = useQueryClient();
  const isCurrentUser = user.id === currentUserId;
  const company = user.identitySource === "company";
  const passwordAllowed = !company && passwordMode !== "off" && (passwordMode === "local" || user.emergencyAdmin === true);
  const currentRole = isPlatformAdmin(user) ? "admin" : "user";
  const [name, setName] = useState(user.name);
  const [email, setEmail] = useState(user.email);
  const [role, setRole] = useState(currentRole);
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [passwordError, setPasswordError] = useState<string | null>(null);
  const refresh = () => queryClient.invalidateQueries({ queryKey: ["admin"] });

  const profileChanges = name.trim() !== user.name || email.trim().toLowerCase() !== user.email.toLowerCase() || (!isCurrentUser && role !== currentRole);
  const saveProfile = useMutation({
    mutationFn: async () => {
      const data: Record<string, unknown> = {};
      if (name.trim() !== user.name) data.name = name.trim();
      if (email.trim().toLowerCase() !== user.email.toLowerCase()) {
        data.email = email.trim().toLowerCase();
        // Local users are provisioned and maintained by a trusted admin.
        data.emailVerified = true;
      }
      if (!isCurrentUser && role !== currentRole) data.role = role;
      const result = await authClient.admin.updateUser({ userId: user.id, data });
      if (result.error) throw new Error(result.error.message ?? "failed to update user");
    },
    onSuccess: () => {
      void refresh();
      if (isCurrentUser) window.location.reload();
    },
  });
  const setUserPassword = useMutation({
    mutationFn: async () => {
      const result = await authClient.admin.setUserPassword({ userId: user.id, newPassword: password });
      if (result.error) throw new Error(result.error.message ?? "failed to set password");
    },
    onSuccess: () => {
      setPassword("");
      setConfirm("");
      if (isCurrentUser) window.location.assign(`${import.meta.env.BASE_URL}login`);
    },
  });
  const accessChange = useMutation({
    mutationFn: async (action: "suspend" | "reinstate" | "revoke") => {
      const result = action === "revoke" ? await authClient.admin.revokeUserSessions({ userId: user.id })
        : action === "reinstate" ? await authClient.admin.unbanUser({ userId: user.id })
        : await authClient.admin.banUser({ userId: user.id });
      if (result.error) throw new Error(result.error.message ?? "Access change failed");
    },
    onSuccess: () => void refresh(),
  });

  const submitProfile = (event: FormEvent) => { event.preventDefault(); if (profileChanges && name.trim() && email.trim()) saveProfile.mutate(); };
  const submitPassword = (event: FormEvent) => {
    event.preventDefault();
    setPasswordError(null);
    if (password !== confirm) { setPasswordError("New passwords do not match."); return; }
    setUserPassword.mutate();
  };

  return (
    <>
      <ResourceDetailsGrid>
        <ResourceDetail label="User ID" mono>{user.id}</ResourceDetail>
        <ResourceDetail label="Status"><ResourceStatus status={userStatus(user)} /></ResourceDetail>
        <ResourceDetail label="Sign-in">{signIn(user)}</ResourceDetail>
        <ResourceDetail label="Created">{user.createdAt ? new Date(user.createdAt).toLocaleDateString() : "—"}</ResourceDetail>
        {company && <ResourceDetail label="Last company check">{user.providerCheckedAt ? new Date(user.providerCheckedAt).toLocaleString() : "—"}</ResourceDetail>}
      </ResourceDetailsGrid>
      <p className="text-sm text-muted-foreground">
        {company
          ? "Their identity and Platform role are managed by the company. Add them to organizations under Admin → organization → Members."
          : "Add them to organizations under Admin → organization → Members."}
      </p>
      <form onSubmit={submitProfile} className="grid gap-4">
        <div className="grid gap-4 sm:grid-cols-2">
          <Field>
            <FieldLabel htmlFor="edit-user-name">Name</FieldLabel>
            <Input id="edit-user-name" value={name} disabled={company} onChange={(event) => setName(event.target.value)} required />
          </Field>
          <Field>
            <FieldLabel htmlFor="edit-user-email">Email</FieldLabel>
            <Input id="edit-user-email" type="email" value={email} disabled={company} onChange={(event) => setEmail(event.target.value)} required />
          </Field>
          <Field>
            <FieldLabel htmlFor="edit-user-role">Platform role</FieldLabel>
            <Select value={role} onValueChange={(value) => setRole(value as string)} disabled={isCurrentUser || company || user.emergencyAdmin === true}>
              <SelectTrigger id="edit-user-role" className="w-full"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="user">User</SelectItem>
                <SelectItem value="admin">Platform Admin</SelectItem>
              </SelectContent>
            </Select>
            {isCurrentUser && <FieldDescription>Another Platform Admin must change your role, so you cannot lock yourself out.</FieldDescription>}
          </Field>
        </div>
        {!company && (
          <div className="flex items-center gap-3">
            <SaveButton type="submit" dirty={profileChanges && !!name.trim() && !!email.trim()} saving={saveProfile.isPending} error={saveProfile.isError} />
            {saveProfile.error && <p role="alert" className="text-sm text-destructive">{saveProfile.error.message}</p>}
          </div>
        )}
      </form>
      {passwordAllowed && (
        <form onSubmit={submitPassword} className="grid gap-4 border-t pt-5">
          <div>
            <p className="text-sm font-medium">Reset password</p>
            <p className="text-sm text-muted-foreground">Signs them out of every session.{isCurrentUser ? " That includes you." : ""}</p>
          </div>
          <div className="grid gap-4 sm:grid-cols-2">
            <Field>
              <FieldLabel htmlFor="edit-user-password">New password</FieldLabel>
              <Input id="edit-user-password" type="password" value={password} onChange={(event) => setPassword(event.target.value)} minLength={8} autoComplete="new-password" required />
            </Field>
            <Field>
              <FieldLabel htmlFor="edit-user-confirm">Repeat new password</FieldLabel>
              <Input id="edit-user-confirm" type="password" value={confirm} onChange={(event) => setConfirm(event.target.value)} minLength={8} autoComplete="new-password" required />
            </Field>
          </div>
          <div className="flex items-center gap-3">
            <Button type="submit" variant="outline" disabled={setUserPassword.isPending || !password}>{setUserPassword.isPending ? "Setting…" : "Set password"}</Button>
            {(passwordError ?? setUserPassword.error?.message) && <p role="alert" className="text-sm text-destructive">{passwordError ?? setUserPassword.error?.message}</p>}
          </div>
        </form>
      )}
      <ResourceActions>
        {!isCurrentUser && (user.banned ? (
          <Button variant="outline" size="sm" disabled={accessChange.isPending} onClick={() => accessChange.mutate("reinstate")}>Reinstate access</Button>
        ) : (
          <ConfirmDangerButton label="Suspend access" title={`Suspend ${user.name}?`} pending={accessChange.isPending}
            description="They lose Platform access and are signed out of every device. Their data and memberships stay."
            onConfirm={() => accessChange.mutate("suspend")} />
        ))}
        <Button variant="outline" size="sm" disabled={accessChange.isPending} onClick={() => accessChange.mutate("revoke")}>Sign out all sessions</Button>
        {accessChange.error && <p role="alert" className="text-sm text-destructive">{accessChange.error.message}</p>}
      </ResourceActions>
    </>
  );
}

function CreateUserDialog({
  open,
  onOpenChange,
  sso,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  sso: boolean;
}) {
  const queryClient = useQueryClient();
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [role, setRole] = useState("user");
  const [error, setError] = useState<string | null>(null);

  const reset = () => {
    setName("");
    setEmail("");
    setPassword("");
    setRole("user");
    setError(null);
  };

  const create = useMutation({
    mutationFn: async () => {
      const result = await authClient.admin.createUser({
        name,
        email,
        password,
        role: sso ? "admin" : role as "user" | "admin",
        ...(sso ? { data: { emergencyAdmin: true } } : {}),
      });
      if (result.error) {
        throw new Error(result.error.message ?? "failed to create user");
      }
      return result.data;
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["admin"] });
      onOpenChange(false);
      reset();
    },
    onError: (err) => setError(err.message),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    setError(null);
    create.mutate();
  };

  return (
    <Dialog open={open} onOpenChange={(next) => { onOpenChange(next); if (!next) reset(); }}>
      <DialogContent className="max-h-[min(92dvh,800px)] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{sso ? "Create emergency admin" : "Create user"}</DialogTitle>
          <DialogDescription>
            {sso ? "Creates a separate local administrator for emergency access. Use an email distinct from company sign-in accounts and store the password securely." : "Creates a local user. Share the password out of band — users can change it under Your profile → Sign-in."}
          </DialogDescription>
        </DialogHeader>
          <form onSubmit={submit} className="grid gap-4">
            <div className="grid gap-4">
              <Field>
                <FieldLabel htmlFor="user-name">Name</FieldLabel>
                <Input
                  id="user-name"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  required
                  autoFocus
                />
              </Field>
              <Field>
                <FieldLabel htmlFor="user-email">Email</FieldLabel>
                <Input
                  id="user-email"
                  type="email"
                  value={email}
                  onChange={(e) => setEmail(e.target.value)}
                  required
                />
              </Field>
              <Field>
                <FieldLabel htmlFor="user-password">Password</FieldLabel>
                <Input
                  id="user-password"
                  type="password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  minLength={8}
                  required
                />
              </Field>
              <Field>
                <FieldLabel htmlFor="user-role">Platform role</FieldLabel>
                <Select value={sso ? "admin" : role} disabled={sso} onValueChange={(value) => setRole(value as string)}>
                  <SelectTrigger id="user-role" className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="user">User</SelectItem>
                    <SelectItem value="admin">Platform Admin</SelectItem>
                  </SelectContent>
                </Select>
              </Field>
            </div>
            {error && <p className="text-sm text-destructive">{error}</p>}
            <DialogFooter>
              <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
                Cancel
              </Button>
              <Button type="submit" disabled={create.isPending}>
                {create.isPending ? "Creating…" : "Create user"}
              </Button>
            </DialogFooter>
          </form>
      </DialogContent>
    </Dialog>
  );
}
