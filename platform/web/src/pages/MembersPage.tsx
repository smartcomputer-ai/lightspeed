import { PrivateContentAccess } from "@/components/access/private-content-access";
import { useActionPermissions } from "@/lib/permissions";
import { ReadError } from "@/components/read-error";
import { useState, type FormEvent } from "react";
import { Link } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Pencil, Plus, Trash2 } from "lucide-react";
import { api, type Member } from "@/api";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { FieldDescription, FieldLabel } from "@/components/ui/field";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Table,
  TableActionsCell,
  TableBody,
  TableCard,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import {
  LoadingNote,
  PageHeader,
  UniverseNotFound,
} from "@/components/page";
import { useActiveUniverse } from "@/lib/universes";

export function MembersPage({ admin: _admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();
  const permissions = useActionPermissions(universe?.id);

  if (isLoading || permissions.isLoading) {
    return <LoadingNote />;
  }
  if (permissions.error) {
    return <ReadError error={permissions.error} loading prefix="Permissions unavailable" />;
  }
  if (!universe || !permissions.can("manage_access")) {
    return <UniverseNotFound slug={slug} />;
  }

  return <MemberList universeId={universe.id} slug={slug} writable={permissions.can("manage_access")} />;
}

function MemberList({ universeId, slug, writable }: { universeId: string; slug: string | undefined; writable: boolean }) {
  const queryClient = useQueryClient();
  const [createOpen, setCreateOpen] = useState(false);
  const [editing, setEditing] = useState<Member | null>(null);
  const members = useQuery({
    queryKey: ["members", universeId],
    queryFn: () => api<Member[]>("GET", `/api/v1/universes/${universeId}/members`),
  });
  const invalidate = () => Promise.all([
    queryClient.invalidateQueries({ queryKey: ["members", universeId] }),
    queryClient.invalidateQueries({ queryKey: ["universes"] }),
    queryClient.invalidateQueries({ queryKey: ["me"] }),
    queryClient.invalidateQueries({ queryKey: ["action-permissions"] }),
    ...["session", "sessions", "bot", "bots", "bot-state", "access-policy"].map((key) => queryClient.invalidateQueries({ predicate: (query) => query.queryKey[0] === key && query.queryKey.includes(universeId) })),
  ]);

  const remove = useMutation({
    mutationFn: (memberId: string) =>
      api("DELETE", `/api/v1/universes/${universeId}/members/${memberId}`),
    onSuccess: () => void invalidate(),
  });

  return (
    <>
      <PageHeader
        title="Members"
        description="Who can see and manage this universe."
        actions={
          writable ? (
            <Button onClick={() => setCreateOpen(true)}>
              <Plus data-icon="inline-start" />
              Add member
            </Button>
          ) : undefined
        }
      />
      {members.isLoading && <LoadingNote />}
      {remove.error && <p className="mb-3 text-sm text-destructive">{remove.error.message}</p>}
      {members.error && <ReadError error={members.error} loading={!members.data} />}
      {members.data && (
        <TableCard className="mb-6">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead>Email</TableHead>
                <TableHead>Role</TableHead>
                {writable && (
                  <TableHead className="w-0" />
                )}
              </TableRow>
            </TableHeader>
            <TableBody>
              {members.data.map((member) => (
                <TableRow key={member.id}>
                  <TableCell>{member.name}</TableCell>
                  <TableCell>{member.email}</TableCell>
                  {member.system ? (
                    <TableCell>
                      Executor
                      <span className="mt-1 block text-xs text-muted-foreground">
                        Every session and bot that runs as the Default agent identity uses its access.{" "}
                        <Link className="underline underline-offset-2" to={`/u/${slug}/settings/general`}>Execution settings</Link>
                      </span>
                    </TableCell>
                  ) : (
                    <TableCell>{member.role}{member.readPrivateContent && <span className="mt-1 block text-xs text-muted-foreground">Private-content access</span>}</TableCell>
                  )}
                  {writable && member.system && <TableCell />}
                  {writable && !member.system && (
                    <TableActionsCell>
                      <Button
                        variant="ghost"
                        size="icon-sm"
                        aria-label={`Edit role for ${member.name || member.email}`}
                        onClick={() => setEditing(member)}
                      >
                        <Pencil />
                      </Button>
                      <AlertDialog>
                        <AlertDialogTrigger
                          render={
                            <Button
                              variant="ghost"
                              size="icon-sm"
                              className="text-destructive"
                            />
                          }
                        >
                          <Trash2 />
                        </AlertDialogTrigger>
                        <AlertDialogContent>
                          <AlertDialogHeader>
                            <AlertDialogTitle>Remove {member.email}?</AlertDialogTitle>
                            <AlertDialogDescription>
                              This role assignment is removed immediately. Other direct or group assignments still apply.
                            </AlertDialogDescription>
                          </AlertDialogHeader>
                          <AlertDialogFooter>
                            <AlertDialogCancel>Cancel</AlertDialogCancel>
                            <AlertDialogAction
                              className="bg-destructive text-white hover:bg-destructive/90"
                              onClick={() => remove.mutate(member.id)}
                            >
                              Remove
                            </AlertDialogAction>
                          </AlertDialogFooter>
                        </AlertDialogContent>
                      </AlertDialog>
                    </TableActionsCell>
                  )}
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </TableCard>
      )}
      {writable && (
        <AddMemberDialog
          universeId={universeId}
          memberUserIds={(members.data ?? []).map((m) => m.userId)}
          open={createOpen}
          onOpenChange={setCreateOpen}
          onDone={invalidate}
        />
      )}
      {writable && editing && (
        <EditMemberRoleDialog
          key={editing.id}
          universeId={universeId}
          member={editing}
          onClose={() => setEditing(null)}
          onDone={invalidate}
        />
      )}
    </>
  );
}

function EditMemberRoleDialog({ universeId, member, onClose, onDone }: {
  universeId: string;
  member: Member;
  onClose: () => void;
  onDone: () => Promise<unknown>;
}) {
  const [role, setRole] = useState(member.role);
  const edit = useMutation({
    mutationFn: () => api("PATCH", `/api/v1/universes/${universeId}/members/${member.id}`, { role }),
    onSuccess: async () => {
      onClose();
      await onDone();
    },
    onError: () => { void onDone(); },
  });

  return (
    <Dialog open onOpenChange={(open) => { if (!open) onClose(); }}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Edit universe role</DialogTitle>
          <DialogDescription>
            Change the role assigned to {member.name || member.email} in this universe.
            Other direct or group assignments still apply. At least one active administrator must remain.
          </DialogDescription>
        </DialogHeader>
        <form className="grid gap-3" onSubmit={(event) => { event.preventDefault(); edit.mutate(); }}>
          <FieldLabel htmlFor="edit-member-role">Role</FieldLabel>
          <Select value={role} onValueChange={(value) => setRole(value as string)} disabled={edit.isPending}>
            <SelectTrigger id="edit-member-role"><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="viewer">viewer</SelectItem>
              <SelectItem value="contributor">contributor</SelectItem>
              <SelectItem value="operator">operator</SelectItem>
              <SelectItem value="admin">admin</SelectItem>
            </SelectContent>
          </Select>
          <PrivateContentAccess universeId={universeId} member={member} onDone={onDone} />
          {edit.error && <p role="alert" className="text-sm text-destructive">{edit.error.message}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>Cancel</Button>
            <Button type="submit" disabled={edit.isPending || role === member.role}>
              {edit.isPending ? "Saving…" : "Save role"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function AddMemberDialog({
  universeId,
  open,
  onOpenChange,
  onDone,
}: {
  universeId: string;
  memberUserIds: string[];
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onDone: () => void;
}) {
  const [userId, setUserId] = useState("");
  const [role, setRole] = useState("contributor");
  const [error, setError] = useState<string | null>(null);

  const users = useQuery({
    queryKey: ["users"],
    queryFn: () => api<PlatformUser[]>("GET", "/api/v1/users"),
  });
  const groups = useQuery({ queryKey: ["universe-groups", universeId],
    queryFn: () => api<Array<{ id: string; displayName: string }>>("GET", `/api/v1/universes/${universeId}/groups`), enabled: open,
  });
  const candidates = [...(users.data ?? []), ...(groups.data ?? []).map((g) => ({ id: `group:${g.id}`, name: g.displayName, email: "Group" }))];

  const add = useMutation({
    mutationFn: () =>
      api("POST", `/api/v1/universes/${universeId}/members`, { ...(userId.startsWith("group:") ? { groupId: userId.slice(6) } : { userId }), role }),
    onSuccess: () => {
      setUserId("");
      setError(null);
      onOpenChange(false);
      onDone();
    },
    onError: (err) => setError(err.message),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!userId) {
      setError("pick a user to add");
      return;
    }
    add.mutate();
  };

  const userLabel = (user: PlatformUser) =>
    user.name ? `${user.name} (${user.email})` : user.email;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Add member</DialogTitle>
          <DialogDescription>
            Give an account or deployment group access to this universe.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-3">
          <FieldLabel htmlFor="member-user">Account</FieldLabel>
          <div className="grid gap-3 sm:grid-cols-[1fr_auto]">
            <Select value={userId} onValueChange={(value) => setUserId((value as string) ?? "")}>
              <SelectTrigger id="member-user" className="w-full">
                <SelectValue>
                  {(value: string) => {
                    if (!value) {
                      return candidates.length === 0
                        ? "No accounts left to add"
                        : "Pick a user…";
                    }
                    const user = candidates.find((u) => u.id === value);
                    return user ? userLabel(user) : value;
                  }}
                </SelectValue>
              </SelectTrigger>
              <SelectContent>
                {candidates.map((user) => (
                  <SelectItem key={user.id} value={user.id}>
                    {userLabel(user)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Select value={role} onValueChange={(value) => setRole(value as string)}>
              <SelectTrigger aria-label="Role" className="w-full sm:w-32">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="viewer">viewer</SelectItem>
                <SelectItem value="contributor">contributor</SelectItem>
                <SelectItem value="operator">operator</SelectItem>
                <SelectItem value="admin">admin</SelectItem>
              </SelectContent>
            </Select>
          </div>
          {error && <p className="text-sm text-destructive">{error}</p>}
          <FieldDescription>
            Accounts are created by platform admins under Admin → Users.
          </FieldDescription>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={add.isPending || !userId}>
              Add member
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

interface PlatformUser {
  id: string;
  name: string | null;
  email: string;
}
