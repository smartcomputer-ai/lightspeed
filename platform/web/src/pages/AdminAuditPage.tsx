import { useQuery } from "@tanstack/react-query";
import { api } from "@/api";
import { authClient } from "@/auth";
import { LoadingNote, PageHeader } from "@/components/page";
import { ReadError } from "@/components/read-error";
import { Table, TableBody, TableCard, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";

interface AuditEvent {
  id: string;
  createdAt: string;
  action: string;
  actorId: string | null;
  targetId: string | null;
  outcome: string;
}

export function AdminAuditPage() {
  const audit = useQuery({
    queryKey: ["admin", "audit"],
    queryFn: () => api<AuditEvent[]>("GET", "/api/v1/admin/audit"),
  });
  const users = useQuery({
    queryKey: ["admin", "users"],
    queryFn: async () => {
      const result = await authClient.admin.listUsers({ query: { limit: 200, sortBy: "createdAt" } });
      if (result.error) throw new Error(result.error.message ?? "failed to load users");
      return result.data.users;
    },
  });
  const label = (id: string | null, fallback: string) => users.data?.find((user) => user.id === id)?.email ?? id ?? fallback;

  return <>
    <PageHeader title="Audit log" description="The latest 100 recorded access changes and permanent session deletions. Records survive user and session deletion." />
    {audit.error && <ReadError error={audit.error} loading={!audit.data} />}
    {audit.isFetching && <LoadingNote />}
    {audit.data && <TableCard><Table>
      <TableHeader><TableRow>
        <TableHead>When</TableHead><TableHead>Action</TableHead><TableHead>Actor</TableHead><TableHead>Target</TableHead><TableHead>Outcome</TableHead>
      </TableRow></TableHeader>
      <TableBody>
        {audit.data.map((event) => <TableRow key={event.id}>
          <TableCell>{new Date(event.createdAt).toLocaleString()}</TableCell>
          <TableCell>{auditAction(event.action)}</TableCell>
          <TableCell>{label(event.actorId, "System")}</TableCell>
          <TableCell>{label(event.targetId, "—")}</TableCell>
          <TableCell>{event.outcome}</TableCell>
        </TableRow>)}
        {audit.data.length === 0 && <TableRow><TableCell colSpan={5} className="text-muted-foreground">No audit events recorded.</TableCell></TableRow>}
      </TableBody>
    </Table></TableCard>}
  </>;
}

function auditAction(action: string): string {
  const labels: Record<string, string> = {
    "session.purge": "Session permanently deleted",
    "company.access": "Company access updated", "company.sign_in": "Company sign-in",
    "emergency.sign_in": "Emergency sign-in", "password.sign_in": "Password sign-in",
    "user.suspend": "User suspended", "user.reinstate": "User reinstated",
    "session.revoke_all": "All sessions signed out", "member.add": "Universe member added",
    "member.role": "Universe role changed", "member.remove": "Universe member removed",
    "key.create": "API key created", "key.revoke": "API key revoked", "key.rotate": "API key rotated",
    "create_user": "User created", "update_user": "User updated", "set_user_password": "Password reset",
    "emergency.designate": "Emergency admin designated", "emergency.create": "Emergency admin created",
  };
  return labels[action] ?? action;
}
