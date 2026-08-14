import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { authClient } from "@/auth";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
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
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { LoadingNote, PageHeader, SectionHeader } from "@/components/page";

interface UserRow {
  id: string;
  name: string;
  email: string;
  role?: string | null;
  createdAt?: string | Date;
}

export function AdminUsersPage() {
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

  return (
    <>
      <PageHeader
        title="Users"
        description="Platform accounts. Signup is closed — accounts are created here."
      />
      {users.isLoading && <LoadingNote />}
      {users.error && <p className="text-sm text-destructive">{users.error.message}</p>}
      {users.data && (
        <div className="mb-8 overflow-x-auto rounded-xl border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead>Email</TableHead>
                <TableHead>Role</TableHead>
                <TableHead>Created</TableHead>
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
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      )}
      <CreateUserForm />
    </>
  );
}

function CreateUserForm() {
  const queryClient = useQueryClient();
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [role, setRole] = useState("user");
  const [created, setCreated] = useState<string | null>(null);

  const create = useMutation({
    mutationFn: async () => {
      const result = await authClient.admin.createUser({
        name,
        email,
        password,
        role: role as "user" | "admin",
      });
      if (result.error) {
        throw new Error(result.error.message ?? "failed to create user");
      }
      return result.data;
    },
    onSuccess: () => {
      setCreated(email);
      setName("");
      setEmail("");
      setPassword("");
      void queryClient.invalidateQueries({ queryKey: ["admin", "users"] });
    },
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    setCreated(null);
    create.mutate();
  };

  return (
    <section>
      <SectionHeader title="Create user" />
      <Card>
        <CardContent>
          <form onSubmit={submit} className="grid gap-4">
            <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
              <Field>
                <FieldLabel htmlFor="user-name">Name</FieldLabel>
                <Input
                  id="user-name"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  required
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
            {create.error && (
              <p className="text-sm text-destructive">{create.error.message}</p>
            )}
            {created && (
              <p className="text-sm text-muted-foreground">Created {created}.</p>
            )}
            <div>
              <Button type="submit" disabled={create.isPending}>
                {create.isPending ? "Creating…" : "Create"}
              </Button>
            </div>
            <FieldDescription>
              Share the password out of band — users can change it under Account.
            </FieldDescription>
          </form>
        </CardContent>
      </Card>
    </section>
  );
}
