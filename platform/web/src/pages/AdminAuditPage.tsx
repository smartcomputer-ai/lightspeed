import { useState } from "react";
import { keepPreviousData, useQuery, useQueryClient } from "@tanstack/react-query";
import type { AuditPage } from "@lightspeed-ai/platform-shared";
import { api } from "@/api";
import { authClient } from "@/auth";
import { LoadingNote, PageHeader } from "@/components/page";
import { ReadError } from "@/components/read-error";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Table, TableBody, TableCard, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";

export function AdminAuditPage() {
  const queryClient = useQueryClient();
  const [pageSize, setPageSize] = useState(50);
  const [cursors, setCursors] = useState<(string | undefined)[]>([undefined]);
  const cursor = cursors.at(-1);
  const audit = useQuery({
    queryKey: ["admin", "audit", pageSize, cursor],
    queryFn: () => {
      const params = new URLSearchParams({ limit: String(pageSize) });
      if (cursor) params.set("cursor", cursor);
      return api<AuditPage>("GET", `/api/ui/admin/audit?${params}`);
    },
    placeholderData: keepPreviousData,
    // New events belong on the first page; refresh explicitly to restart there.
    staleTime: Infinity,
    refetchOnWindowFocus: false,
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
    <PageHeader title="Audit log" description="Recorded access changes and permanent session deletions, newest first. Records survive user and session deletion."
      actions={<Button variant="outline" size="sm" disabled={audit.isFetching} onClick={() => {
        setCursors([undefined]);
        void queryClient.invalidateQueries({ queryKey: ["admin", "audit"] });
      }}>Refresh</Button>}
    />
    {audit.error && <ReadError error={audit.error} loading={!audit.data} />}
    {audit.isFetching && <LoadingNote />}
    {audit.data && <TableCard><Table>
      <TableHeader><TableRow>
        <TableHead>When</TableHead><TableHead>Action</TableHead><TableHead>Actor</TableHead><TableHead>Target</TableHead><TableHead>Outcome</TableHead>
      </TableRow></TableHeader>
      <TableBody>
        {audit.data.events.map((event) => <TableRow key={event.id}>
          <TableCell>{new Date(event.createdAt).toLocaleString()}</TableCell>
          <TableCell>{auditAction(event.action)}</TableCell>
          <TableCell>{label(event.actorId, "System")}</TableCell>
          <TableCell>{label(event.targetId, "—")}</TableCell>
          <TableCell>{event.outcome}</TableCell>
        </TableRow>)}
        {audit.data.events.length === 0 && <TableRow><TableCell colSpan={5} className="text-muted-foreground">No audit events recorded.</TableCell></TableRow>}
      </TableBody>
    </Table></TableCard>}
    <nav aria-label="Audit log pagination" className="mt-4 flex flex-wrap items-center justify-between gap-3">
      <div className="flex items-center gap-2 text-sm text-muted-foreground">
        <label htmlFor="audit-page-size">Rows per page</label>
        <Select value={pageSize} onValueChange={(value) => {
          if (value !== null) { setPageSize(value); setCursors([undefined]); }
        }}>
          <SelectTrigger id="audit-page-size" size="sm" className="w-20 text-foreground"><SelectValue /></SelectTrigger>
          <SelectContent>{[25, 50, 100].map((size) => <SelectItem key={size} value={size}>{size}</SelectItem>)}</SelectContent>
        </Select>
      </div>
      <div className="flex items-center gap-2">
        <span className="mr-2 text-sm text-muted-foreground" aria-live="polite">Page {cursors.length}</span>
        <Button variant="outline" size="sm" disabled={cursors.length === 1 || audit.isFetching}
          onClick={() => setCursors((previous) => previous.slice(0, -1))}>Previous</Button>
        <Button variant="outline" size="sm" disabled={audit.isFetching || audit.isPlaceholderData || !audit.data?.nextCursor}
          onClick={() => {
            const nextCursor = audit.data?.nextCursor;
            if (nextCursor) setCursors((previous) => [...previous, nextCursor]);
          }}>Next</Button>
      </div>
    </nav>
  </>;
}

function auditAction(action: string): string {
  const labels: Record<string, string> = {
    "universe.delete": "Universe deletion requested",
    "session.purge": "Session permanently deleted",
    "company.access": "Company access updated", "company.sign_in": "Company sign-in",
    "emergency.sign_in": "Emergency sign-in", "password.sign_in": "Password sign-in",
    "user.suspend": "User suspended", "user.reinstate": "User reinstated",
    "session.revoke_all": "All sessions signed out", "member.add": "Universe member added",
    "member.role": "Universe role changed", "member.remove": "Universe member removed",
    "create_user": "User created", "update_user": "User updated", "set_user_password": "Password reset",
    "emergency.designate": "Emergency admin designated", "emergency.create": "Emergency admin created",
  };
  return labels[action] ?? action;
}
