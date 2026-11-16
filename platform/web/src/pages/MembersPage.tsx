import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { UNIVERSE_ROLES, type PrincipalRef, type UniverseRole } from "@lightspeed-ai/platform-shared";
import { platformApi } from "@/lib/platform-api";
import { useActiveUniverse } from "@/lib/universes";
import { useActionPermissions } from "@/lib/permissions";
import { Button } from "@/components/ui/button";
import { PageHeader, LoadingNote, UniverseNotFound } from "@/components/page";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow, TableCard } from "@/components/ui/table";

export function MembersPage({ admin: _admin }: { admin: boolean }) {
  const { universe, handle, isLoading } = useActiveUniverse();
  const permissions = useActionPermissions(universe?.id);
  if (isLoading) return <LoadingNote />;
  if (!universe) return <UniverseNotFound handle={handle} />;
  if (!permissions.can("manageAccess")) return <><PageHeader title="Members" /><p>Only a universe Owner can view or manage access.</p></>;
  return <Members universe={universe.lightspeedUniverseId} accountId={universe.accountId} writable={permissions.can("manageAccess")} />;
}

function Members({ universe, accountId, writable }: { universe: string; accountId?: string; writable: boolean }) {
  const client = useQueryClient();
  const [principal, setPrincipal] = useState("");
  const [role, setRole] = useState<UniverseRole>("viewer");
  const grants = useQuery({ queryKey: ["grants", universe], queryFn: () => platformApi("platform/universes/grants/list", { universe }) });
  const members = useQuery({ queryKey: ["account-members", accountId], enabled: writable && !!accountId,
    queryFn: () => platformApi("platform/accounts/members/list", { accountId: accountId! }) });
  const applications = useQuery({ queryKey: ["applications", accountId], enabled: writable && !!accountId,
    queryFn: () => platformApi("platform/accounts/applications/list", { accountId: accountId! }) });
  const refresh = async () => {
    await client.invalidateQueries({ queryKey: ["grants", universe] });
    await client.invalidateQueries({ queryKey: ["universes"] });
  };
  const put = useMutation({ mutationFn: (input: { principal: PrincipalRef; role: UniverseRole }) => platformApi("platform/universes/grants/put", { universe, ...input }), onSuccess: refresh });
  const remove = useMutation({ mutationFn: (grantId: string) => platformApi("platform/universes/grants/delete", { universe, grantId }), onSuccess: refresh });
  const choices = [
    ...(members.data?.members ?? []).map(m => ({ value: `user:${m.userId}`, label: m.name })),
    ...(applications.data?.applications ?? []).filter(a => a.enabled).map(a => ({ value: `application:${a.id}`, label: `${a.name} (application)` })),
  ];
  return <>
    <PageHeader title="Members" description="Explicit user and application access. Account admins also have access to every universe in their account." />
    {[grants.error, members.error, applications.error, put.error, remove.error].filter(Boolean).map((e, i) => <p role="alert" className="text-destructive" key={i}>{e?.message}</p>)}
    {writable && <form className="mb-6 flex flex-wrap gap-3" onSubmit={event => {
      event.preventDefault();
      const split = principal.indexOf(":");
      if (split < 0) return;
      put.mutate({ principal: { kind: principal.slice(0, split) as PrincipalRef["kind"], id: principal.slice(split + 1) }, role });
    }}>
      <select aria-label="Account principal" className="rounded-md border bg-background p-2" value={principal} onChange={e => setPrincipal(e.target.value)} required>
        <option value="">Select an account member or application</option>
        {choices.map(c => <option key={c.value} value={c.value}>{c.label}</option>)}
      </select>
      <select aria-label="Universe role" className="rounded-md border bg-background p-2" value={role} onChange={e => setRole(e.target.value as UniverseRole)}>{UNIVERSE_ROLES.map(r => <option key={r}>{r}</option>)}</select>
      <Button type="submit" disabled={!principal || put.isPending}>Grant access</Button>
    </form>}
    <TableCard><Table><TableHeader><TableRow><TableHead>Principal</TableHead><TableHead>Type</TableHead><TableHead>Role</TableHead><TableHead /></TableRow></TableHeader><TableBody>
      {grants.data?.grants.map(g => <TableRow key={g.id}><TableCell>{g.name}</TableCell><TableCell>{g.principal.kind}</TableCell><TableCell>
        {writable ? <select aria-label={`Role for ${g.name}`} value={g.role} disabled={put.isPending} onChange={e => put.mutate({ principal: g.principal, role: e.target.value as UniverseRole })}>{UNIVERSE_ROLES.map(r => <option key={r}>{r}</option>)}</select> : g.role}
      </TableCell><TableCell>{writable && <Button variant="ghost" disabled={remove.isPending} onClick={() => { if (window.confirm(`Remove ${g.name} from this universe?`)) remove.mutate(g.id); }}>Remove</Button>}</TableCell></TableRow>)}
    </TableBody></Table></TableCard>
    <p className="mt-3 text-sm text-muted-foreground">Keep at least one explicit Owner. Owners manage universe access; Account Admins manage account membership.</p>
  </>;
}
