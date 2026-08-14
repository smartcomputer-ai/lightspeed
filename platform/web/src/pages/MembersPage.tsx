import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Plus, Trash2 } from "lucide-react";
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
  TableBody,
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
import { canManage, useActiveUniverse } from "@/lib/universes";

export function MembersPage({ admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();

  if (isLoading) {
    return <LoadingNote />;
  }
  if (!universe) {
    return <UniverseNotFound slug={slug} />;
  }

  return <MemberList universeId={universe.id} writable={canManage(universe, admin)} />;
}

function MemberList({ universeId, writable }: { universeId: string; writable: boolean }) {
  const queryClient = useQueryClient();
  const [createOpen, setCreateOpen] = useState(false);
  const members = useQuery({
    queryKey: ["members", universeId],
    queryFn: () => api<Member[]>("GET", `/api/v1/universes/${universeId}/members`),
  });
  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: ["members", universeId] });

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
      {members.error && <p className="text-sm text-destructive">{members.error.message}</p>}
      {members.data && (
        <div className="mb-6 overflow-x-auto rounded-xl border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead>Email</TableHead>
                <TableHead>Role</TableHead>
                {writable && <TableHead className="w-0" />}
              </TableRow>
            </TableHeader>
            <TableBody>
              {members.data.map((member) => (
                <TableRow key={member.id}>
                  <TableCell>{member.name}</TableCell>
                  <TableCell>{member.email}</TableCell>
                  <TableCell>{member.role}</TableCell>
                  {writable && (
                    <TableCell>
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
                              They lose access to this universe immediately.
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
                    </TableCell>
                  )}
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
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
    </>
  );
}

function AddMemberDialog({
  universeId,
  memberUserIds,
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
  const [role, setRole] = useState("member");
  const [error, setError] = useState<string | null>(null);

  const users = useQuery({
    queryKey: ["users"],
    queryFn: () => api<PlatformUser[]>("GET", "/api/v1/users"),
  });
  const candidates = (users.data ?? []).filter(
    (user) => !memberUserIds.includes(user.id),
  );

  const add = useMutation({
    mutationFn: () =>
      api("POST", `/api/v1/universes/${universeId}/members`, { userId, role }),
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
            Give an existing account access to this universe.
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
                    const user = users.data?.find((u) => u.id === value);
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
                <SelectItem value="member">member</SelectItem>
                <SelectItem value="admin">admin</SelectItem>
                <SelectItem value="owner">owner</SelectItem>
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
