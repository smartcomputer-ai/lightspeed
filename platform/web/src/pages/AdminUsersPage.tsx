import { ReadError } from "@/components/read-error";
import { useEffect, useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Pencil, Plus } from "lucide-react";
import { authClient, useLoginConfig, type SessionUser } from "@/auth";
import { api } from "@/api";
import { Badge } from "@/components/ui/badge";
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Table,
  TableBody,
  TableCard,
  TableCell,
  TableActionsCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { LoadingNote, PageHeader } from "@/components/page";

interface UserRow extends SessionUser {
  createdAt?: string | Date;
}

export function AdminUsersPage({ currentUser }: { currentUser: SessionUser }) {
  const config = useLoginConfig();
  const [showAudit, setShowAudit] = useState(false);
  const audit = useQuery({ queryKey: ["admin", "audit"], enabled: showAudit, queryFn: () => api<Array<{
    id: string; createdAt: string; action: string; actorId: string | null; targetId: string | null; outcome: string;
  }>>("GET", "/api/v1/admin/audit") });
  const users = useQuery({
    queryKey: ["admin", "users"],
    queryFn: async () => {
      const result = await authClient.admin.listUsers({
        query: { limit: 200, sortBy: "createdAt" },
      });
      if (result.error) {
        throw new Error(result.error.message ?? "failed to load users");
      }
      return result.data.users as UserRow[];
    },
  });

  const [createOpen, setCreateOpen] = useState(false);
  const [editing, setEditing] = useState<UserRow | null>(null);

  return (
    <>
      <PageHeader
        title="Users"
        description={config.data?.sso ? "Company users appear after their first admitted sign-in. Manage universe roles on Members." : "Platform accounts. Signup is closed — accounts are created here."}
        actions={
          config.data && config.data.password !== "off" && <Button onClick={() => setCreateOpen(true)}>
            <Plus data-icon="inline-start" />
            {config.data.sso ? "Create emergency admin" : "Create user"}
          </Button>
        }
      />
      <CreateUserDialog open={createOpen} onOpenChange={setCreateOpen} sso={config.data?.sso ?? false} />
      <EditUserDialog
        user={editing}
        currentUserId={currentUser.id}
        passwordMode={config.data?.password}
        onOpenChange={(open) => {
          if (!open) setEditing(null);
        }}
      />
      {users.isLoading && <LoadingNote />}
      {users.error && <ReadError error={users.error} loading={!users.data} />}
      {users.data && (
        <TableCard className="mb-8">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead>Email</TableHead>
                <TableHead>Role</TableHead>
                <TableHead>Access</TableHead>
                <TableHead>Last company check</TableHead>
                <TableHead>Created</TableHead>
                <TableHead className="w-0" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {users.data.map((user) => (
                <TableRow key={user.id}>
                  <TableCell>{user.name}</TableCell>
                  <TableCell>{user.email}</TableCell>
                  <TableCell>
                    {user.role?.split(",").includes("admin") ? (
                      <Badge variant="secondary">admin</Badge>
                    ) : (
                      <span className="text-muted-foreground">user</span>
                    )}
                  </TableCell>
                  <TableCell className="text-muted-foreground">
                    {user.banned ? "Suspended" : user.identitySource === "company" ? user.companyAdmitted ? "Company account" : "Company access absent" : user.emergencyAdmin ? "Emergency admin" : "Local account"}
                  </TableCell>
                  <TableCell className="text-muted-foreground">
                    {user.providerCheckedAt ? new Date(user.providerCheckedAt).toLocaleString() : "—"}
                  </TableCell>
                  <TableCell className="text-muted-foreground">
                    {user.createdAt
                      ? new Date(user.createdAt).toLocaleDateString()
                      : "—"}
                  </TableCell>
                  <TableActionsCell>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={`Edit ${user.email}`}
                      onClick={() => setEditing(user)}
                    >
                      <Pencil />
                    </Button>
                  </TableActionsCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </TableCard>
      )}
      <details onToggle={(event) => setShowAudit(event.currentTarget.open)}>
        <summary className="cursor-pointer font-medium">Recent access changes</summary>
        <p className="my-3 text-sm text-muted-foreground">The latest 100 recorded access events. Records survive user and session deletion.</p>
        {audit.error && <ReadError error={audit.error} loading={!audit.data} />}
        {audit.isFetching && <LoadingNote />}
        {audit.data && <TableCard><Table>
          <TableHeader><TableRow><TableHead>When</TableHead><TableHead>Action</TableHead><TableHead>Actor</TableHead><TableHead>Target</TableHead><TableHead>Outcome</TableHead></TableRow></TableHeader>
          <TableBody>{audit.data.map((event) => <TableRow key={event.id}>
            <TableCell>{new Date(event.createdAt).toLocaleString()}</TableCell><TableCell>{auditAction(event.action)}</TableCell>
            <TableCell>{users.data?.find((user) => user.id === event.actorId)?.email ?? event.actorId ?? "System"}</TableCell>
            <TableCell>{users.data?.find((user) => user.id === event.targetId)?.email ?? event.targetId ?? "—"}</TableCell><TableCell>{event.outcome}</TableCell>
          </TableRow>)}</TableBody>
        </Table></TableCard>}
      </details>
    </>
  );
}

function EditUserDialog({
  user,
  currentUserId,
  passwordMode,
  onOpenChange,
}: {
  user: UserRow | null;
  currentUserId: string;
  passwordMode?: "local" | "break-glass" | "off";
  onOpenChange: (open: boolean) => void;
}) {
  const queryClient = useQueryClient();
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [role, setRole] = useState("user");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [error, setError] = useState<string | null>(null);

  const open = user !== null;
  const isCurrentUser = user?.id === currentUserId;
  const company = user?.identitySource === "company";
  const passwordAllowed = !company && passwordMode !== "off" && (passwordMode === "local" || user?.emergencyAdmin === true);

  useEffect(() => {
    if (!user) return;
    setName(user.name);
    setEmail(user.email);
    setRole(user.role?.split(",").includes("admin") ? "admin" : "user");
    setPassword("");
    setConfirm("");
    setError(null);
  }, [user]);

  const reset = () => {
    setName("");
    setEmail("");
    setRole("user");
    setPassword("");
    setConfirm("");
    setError(null);
  };

  const close = () => {
    onOpenChange(false);
    reset();
  };

  const accessChange = useMutation({
    mutationFn: async (action: "suspend" | "reinstate" | "revoke") => {
      if (!user) return;
      const result = action === "revoke" ? await authClient.admin.revokeUserSessions({ userId: user.id })
        : action === "reinstate" ? await authClient.admin.unbanUser({ userId: user.id })
        : await authClient.admin.banUser({ userId: user.id });
      if (result.error) throw new Error(result.error.message ?? "Access change failed");
    },
    onSuccess: () => { void queryClient.invalidateQueries({ queryKey: ["admin"] }); close(); },
    onError: (err) => setError(err.message),
  });

  const edit = useMutation({
    mutationFn: async () => {
      if (!user) return;

      const data: Record<string, unknown> = {};
      const nextName = name.trim();
      const nextEmail = email.trim().toLowerCase();
      const currentRole = user.role?.split(",").includes("admin") ? "admin" : "user";
      if (nextName !== user.name) data.name = nextName;
      if (nextEmail !== user.email.toLowerCase()) {
        data.email = nextEmail;
        // Platform accounts are provisioned and maintained by a trusted admin.
        data.emailVerified = true;
      }
      if (!isCurrentUser && role !== currentRole) data.role = role;

      if (Object.keys(data).length > 0) {
        const result = await authClient.admin.updateUser({ userId: user.id, data });
        if (result.error) {
          throw new Error(result.error.message ?? "failed to update user");
        }
      }

      if (password) {
        const result = await authClient.admin.setUserPassword({
          userId: user.id,
          newPassword: password,
        });
        if (result.error) {
          throw new Error(result.error.message ?? "failed to set password");
        }

      }
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["admin"] });
      const signedOutSelf = isCurrentUser && Boolean(password);
      const refreshedSelf = isCurrentUser && hasProfileChanges;
      close();
      if (signedOutSelf) {
        window.location.assign(`${import.meta.env.BASE_URL}login`);
      } else if (refreshedSelf) {
        window.location.reload();
      }
    },
    onError: (err) => setError(err.message),
  });

  const currentRole = user?.role?.split(",").includes("admin") ? "admin" : "user";
  const hasProfileChanges = Boolean(
    user &&
      (name.trim() !== user.name ||
        email.trim().toLowerCase() !== user.email.toLowerCase() ||
        (!isCurrentUser && role !== currentRole)),
  );
  const hasChanges = hasProfileChanges || Boolean(password);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    setError(null);
    if (password !== confirm) {
      setError("New passwords do not match.");
      return;
    }
    edit.mutate();
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) close();
      }}
    >
      <DialogContent className="max-h-[min(92dvh,800px)] overflow-y-auto sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Edit user</DialogTitle>
          <DialogDescription>
            {company ? `Identity and platform role for ${user?.email} are managed by the company. Universe roles are managed on Members.` : `Update the platform account for ${user?.email}. A password reset signs the user out of every session.`}
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-5">
          <div className="grid gap-4">
            <Field>
              <FieldLabel htmlFor="edit-user-name">Name</FieldLabel>
              <Input
                id="edit-user-name"
                value={name}
                disabled={company}
                onChange={(event) => setName(event.target.value)}
                required
                autoFocus
              />
            </Field>
            <Field>
              <FieldLabel htmlFor="edit-user-email">Email</FieldLabel>
              <Input
                id="edit-user-email"
                type="email"
                value={email}
                disabled={company}
                onChange={(event) => setEmail(event.target.value)}
                required
              />
              <FieldDescription>
                Admin-managed addresses are treated as verified sign-in addresses.
              </FieldDescription>
            </Field>
            <Field>
              <FieldLabel htmlFor="edit-user-role">Platform role</FieldLabel>
              <Select
                value={role}
                onValueChange={(value) => setRole(value as string)}
                disabled={isCurrentUser || company || user?.emergencyAdmin === true}
              >
                <SelectTrigger id="edit-user-role" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="user">user</SelectItem>
                  <SelectItem value="admin">admin</SelectItem>
                </SelectContent>
              </Select>
              {isCurrentUser && (
                <FieldDescription>
                  Another platform admin must change your role, preventing accidental
                  self-lockout.
                </FieldDescription>
              )}
            </Field>
          </div>

          {passwordAllowed && <div className="grid gap-4 border-t pt-5">
            <div>
              <p className="font-medium">Reset password</p>
              <p className="text-sm text-muted-foreground">
                Leave these fields empty to keep the current password.
                {isCurrentUser ? " You will be signed out if you reset your own password." : ""}
              </p>
            </div>
            <Field>
              <FieldLabel htmlFor="edit-user-password">New password</FieldLabel>
              <Input
                id="edit-user-password"
                type="password"
                value={password}
                onChange={(event) => setPassword(event.target.value)}
                minLength={8}
                autoComplete="new-password"
              />
            </Field>
            <Field>
              <FieldLabel htmlFor="edit-user-confirm">Repeat new password</FieldLabel>
              <Input
                id="edit-user-confirm"
                type="password"
                value={confirm}
                onChange={(event) => setConfirm(event.target.value)}
                minLength={8}
                autoComplete="new-password"
                required={Boolean(password)}
              />
            </Field>
          </div>}

          <div className="grid gap-2 border-t pt-5">
            <p className="text-sm text-muted-foreground">Suspension blocks Platform access and signs out every device. Core API keys must be revoked separately.</p>
            {!isCurrentUser && <Button type="button" variant="outline" disabled={accessChange.isPending} onClick={() => accessChange.mutate(user?.banned ? "reinstate" : "suspend")}>
              {user?.banned ? "Reinstate access" : "Suspend access"}
            </Button>}
            <Button type="button" variant="outline" disabled={accessChange.isPending} onClick={() => accessChange.mutate("revoke")}>Sign out all sessions</Button>
          </div>

          {error && <p className="text-sm text-destructive">{error}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={close}>
              Cancel
            </Button>
            <Button
              type="submit"
              disabled={edit.isPending || !hasChanges || !name.trim() || !email.trim()}
            >
              {edit.isPending ? "Saving…" : "Save changes"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
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
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{sso ? "Create emergency admin" : "Create user"}</DialogTitle>
          <DialogDescription>
            {sso ? "Creates a separate local administrator for emergency access. Use an email distinct from company sign-in accounts and store the password securely." : "Creates a platform account. Share the password out of band — users can change it under Account."}
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
                <FieldLabel htmlFor="user-role">Role</FieldLabel>
                <Select value={sso ? "admin" : role} disabled={sso} onValueChange={(value) => setRole(value as string)}>
                  <SelectTrigger id="user-role" className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="user">user</SelectItem>
                    <SelectItem value="admin">admin</SelectItem>
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

function auditAction(action: string): string {
  const labels: Record<string, string> = {
    "company.access": "Company access updated", "company.sign_in": "Company sign-in",
    "emergency.sign_in": "Emergency sign-in", "password.sign_in": "Password sign-in",
    "user.suspend": "User suspended", "user.reinstate": "User reinstated",
    "session.revoke_all": "All sessions signed out", "member.add": "Universe member added",
    "member.role": "Universe role changed", "member.remove": "Universe member removed",
    "key.create": "API key created", "key.revoke": "API key revoked",
    "create_user": "User created", "update_user": "User updated", "set_user_password": "Password reset",
    "emergency.designate": "Emergency admin designated", "emergency.create": "Emergency admin created",
  };
  return labels[action] ?? action;
}
