import { api } from "@/api";
import { ReadError } from "@/components/read-error";
import { useEffect, useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Pencil, Plus } from "lucide-react";
import { authClient, type SessionUser } from "@/auth";
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

interface UserRow {
  id: string;
  name: string;
  email: string;
  role?: string | null;
  directRole?: string;
  status?: "active" | "disabled";
  createdAt?: string | Date;
}

export function AdminUsersPage({ currentUser }: { currentUser: SessionUser }) {
  const users = useQuery({
    queryKey: ["admin", "users"],
    queryFn: async () => {
      return (await api<{ users: UserRow[] }>("GET", "/api/v1/admin/users")).users;
    },
  });

  const [createOpen, setCreateOpen] = useState(false);
  const [editing, setEditing] = useState<UserRow | null>(null);

  return (
    <>
      <PageHeader
        title="Users"
        description="Platform accounts. Signup is closed — accounts are created here."
        actions={
          <Button onClick={() => setCreateOpen(true)}>
            <Plus data-icon="inline-start" />
            Create user
          </Button>
        }
      />
      <CreateUserDialog open={createOpen} onOpenChange={setCreateOpen} />
      <EditUserDialog
        user={editing}
        currentUserId={currentUser.id}
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
    </>
  );
}

function EditUserDialog({
  user,
  currentUserId,
  onOpenChange,
}: {
  user: UserRow | null;
  currentUserId: string;
  onOpenChange: (open: boolean) => void;
}) {
  const queryClient = useQueryClient();
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [role, setRole] = useState("user");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [status, setStatus] = useState<"active" | "disabled">("active");
  const [error, setError] = useState<string | null>(null);

  const open = user !== null;
  const isCurrentUser = user?.id === currentUserId;

  useEffect(() => {
    if (!user) return;
    setStatus(user.status ?? "active");
    setName(user.name);
    setEmail(user.email);
    setRole(user.directRole ?? (user.role === "admin" ? "admin" : "user"));
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

  const edit = useMutation({
    mutationFn: async () => {
      if (!user) return;

      const data: Record<string, unknown> = {};
      const nextName = name.trim();
      const nextEmail = email.trim().toLowerCase();
      const currentRole = user.directRole ?? (user.role === "admin" ? "admin" : "user");
      if (nextName !== user.name) data.name = nextName;
      if (nextEmail !== user.email.toLowerCase()) {
        data.email = nextEmail;
        // Platform accounts are provisioned and maintained by a trusted admin.
        data.emailVerified = true;
      }
      if (!isCurrentUser && role !== currentRole) data.role = role;
      if (status !== (user.status ?? "active")) data.status = status;

      if (Object.keys(data).length > 0) {
        await api("PATCH", `/api/v1/admin/users/${user.id}`, data);
      }

      if (password) {
        await api("POST", `/api/v1/admin/users/${user.id}/password`, { newPassword: password });
      }
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["admin", "users"] });
      void queryClient.invalidateQueries({ queryKey: ["me"] });
      void queryClient.invalidateQueries({ queryKey: ["universes"] });
      const signedOutSelf = isCurrentUser && Boolean(password);
      const refreshedSelf = isCurrentUser && hasProfileChanges;
      close();
      if (signedOutSelf) {
        window.location.assign("/login");
      } else if (refreshedSelf) {
        window.location.reload();
      }
    },
    onError: (err) => setError(err.message),
  });

  const currentRole = user?.directRole ?? (user?.role === "admin" ? "admin" : "user");
  const hasProfileChanges = Boolean(
    user &&
      (name.trim() !== user.name ||
        email.trim().toLowerCase() !== user.email.toLowerCase() ||
        (!isCurrentUser && role !== currentRole)),
  );
  const hasChanges = hasProfileChanges || Boolean(password) || status !== (user?.status ?? "active");

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
            Update the platform account for {user?.email}. A password reset signs the
            user out of every session.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-5">
          <div className="grid gap-4">
            <Field>
              <FieldLabel htmlFor="edit-user-name">Name</FieldLabel>
              <Input
                id="edit-user-name"
                value={name}
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
                onChange={(event) => setEmail(event.target.value)}
                required
              />
              <FieldDescription>
                Admin-managed addresses are treated as verified sign-in addresses.
              </FieldDescription>
            </Field>
            <Field>
              <FieldLabel htmlFor="edit-user-role">Direct deployment role</FieldLabel>
              <Select
                value={role}
                onValueChange={(value) => setRole(value as string)}
                disabled={isCurrentUser}
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
                  Direct roles are separate from roles inherited through groups.
                </FieldDescription>
              )}
            </Field>
          </div>

          <div className="grid gap-4 border-t pt-5">
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
          </div>

          {error && <p className="text-sm text-destructive">{error}</p>}
          {user && <label className="flex gap-2 text-sm"><input type="checkbox" checked={status === "active"} onChange={(e) => setStatus(e.target.checked ? "active" : "disabled")} />Account active (core identity)</label>}
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
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
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
      return api("POST", "/api/v1/admin/users", { name, email, password, role });
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["admin", "users"] });
      void queryClient.invalidateQueries({ queryKey: ["me"] });
      void queryClient.invalidateQueries({ queryKey: ["universes"] });
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
          <DialogTitle>Create user</DialogTitle>
          <DialogDescription>
            Creates a platform account. Share the password out of band — users can
            change it under Account.
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
                <Select value={role} onValueChange={(value) => setRole(value as string)}>
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
