import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { AccessChange, AccessDirectory } from "@lightspeed-ai/agent-client";
import { universeRoleSchema } from "@lightspeed/platform-shared";
import { api, type Universe } from "@/api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { PageHeader } from "@/components/page";

type AccessRole = AccessDirectory["roles"][number]["role"];

export function AdminGroupsPage() {
  const cache = useQueryClient();
  const directory = useQuery({ queryKey: ["identity"], queryFn: () => api<AccessDirectory>("GET", "/api/v1/admin/identity") });
  const universes = useQuery({ queryKey: ["universes"], queryFn: () => api<Universe[]>("GET", "/api/v1/universes") });
  const [name, setName] = useState("");
  const [groupId, setGroupId] = useState("");
  const [principalId, setPrincipalId] = useState("");
  const [universeId, setUniverseId] = useState("");
  const [role, setRole] = useState<AccessRole>("viewer");
  const change = useMutation({ mutationFn: (body: AccessChange) => api("POST", "/api/v1/admin/identity", body), onSuccess: () => {
    void cache.invalidateQueries({ queryKey: ["identity"] });
    void cache.invalidateQueries({ queryKey: ["me"] });
    void cache.invalidateQueries({ queryKey: ["universes"] });
    void cache.invalidateQueries({ queryKey: ["admin", "users"] });
  } });
  const data = directory.data;
  const group = data?.groups.find((g) => g.id === groupId);
  const selectClass = "h-9 rounded-md border bg-background px-3 text-sm";
  return <>
    <PageHeader title="Groups" description="Deployment groups bind people and service principals to roles. Universe permissions are assigned explicitly." />
    {(directory.error || change.error) && <p role="alert" className="mb-4 text-destructive">{(directory.error || change.error)?.message}</p>}
    <form className="mb-6 flex gap-2" onSubmit={(e) => { e.preventDefault(); change.mutate({ operation: "create_group", id: crypto.randomUUID(), displayName: name }); }}>
      <Input aria-label="New group name" value={name} onChange={(e) => setName(e.target.value)} placeholder="New group name" maxLength={120} />
      <Button disabled={!name.trim() || change.isPending}>Create group</Button>
    </form>
    <label className="grid gap-2">Group
      <select className={selectClass} value={groupId} onChange={(e) => setGroupId(e.target.value)}>
        <option value="">Choose group…</option>
        {data?.groups.map((g) => <option key={g.id} value={g.id}>{g.displayName}</option>)}
      </select>
    </label>
    {group && data && <div className="mt-6 grid gap-6">
      <section className="grid gap-3">
        <h2 className="font-medium">Members</h2>
        {data.memberships.filter((m) => m.groupId === groupId).map((m) => <div key={m.principalId} className="flex items-center justify-between gap-3">
          <span>{data.principals.find((p) => p.id === m.principalId)?.displayName ?? m.principalId}</span>
          <Button variant="outline" disabled={change.isPending} onClick={() => change.mutate({ operation: "remove_membership", membership: m })}>Remove</Button>
        </div>)}
        <div className="flex gap-2">
          <select aria-label="Principal" className={selectClass} value={principalId} onChange={(e) => setPrincipalId(e.target.value)}>
            <option value="">Choose principal…</option>
            {data.principals.filter((p) => p.status === "active").map((p) => <option key={p.id} value={p.id}>{p.displayName} ({p.kind})</option>)}
          </select>
          <Button disabled={!principalId || change.isPending} onClick={() => change.mutate({ operation: "put_membership", membership: { groupId, principalId } })}>Add member</Button>
        </div>
      </section>
      <section className="grid gap-3">
        <h2 className="font-medium">Roles</h2>
        {data.roles.filter((r) => r.subject.kind === "group" && r.subject.id === groupId).map((r) => <div key={`${JSON.stringify(r.scope)}:${r.role}`} className="flex items-center justify-between gap-3">
          <span>{r.scope.kind === "deployment" ? "Deployment" : universes.data?.find((u) => r.scope.kind === "universe" && u.lightspeedUniverseId === r.scope.universeId)?.name ?? r.scope.universeId}: {r.role}</span>
          <Button variant="outline" disabled={change.isPending} onClick={() => change.mutate({ operation: "revoke_role", assignment: r })}>Remove role</Button>
        </div>)}
        <div className="flex flex-wrap gap-2">
          <select aria-label="Role scope" className={selectClass} value={universeId} onChange={(e) => setUniverseId(e.target.value)}>
            <option value="">Deployment administration</option>
            {universes.data?.map((u) => <option key={u.id} value={u.lightspeedUniverseId}>{u.name}</option>)}
          </select>
          {universeId && <select aria-label="Universe role" className={selectClass} value={role} onChange={(e) => setRole(e.target.value as AccessRole)}>
            {universeRoleSchema.options.map((r) => <option key={r}>{r}</option>)}
          </select>}
          <Button disabled={change.isPending} onClick={() => change.mutate({ operation: "assign_role", assignment: {
            subject: { kind: "group", id: groupId }, scope: universeId ? { kind: "universe", universeId } : { kind: "deployment" }, role: universeId ? role : "deployment_admin",
          } })}>Assign role</Button>
        </div>
      </section>
    </div>}
  </>;
}
