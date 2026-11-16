import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { AccountRole, PlatformOperationResult } from "@lightspeed-ai/platform-shared";
import { platformApi, useAccounts } from "@/lib/platform-api";
import { usePlatformAdmin } from "@/lib/permissions";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Field, FieldLabel } from "@/components/ui/field";
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from "@/components/ui/card";
import { PageHeader } from "@/components/page";
import { ApiKeysCard } from "@/components/api-keys-card";

type Account = PlatformOperationResult<"platform/accounts/read">["account"];
const roles: AccountRole[] = ["member", "creator", "admin"];
function RoleSelect({ value, onChange, disabled }: { value: AccountRole; onChange: (role: AccountRole) => void; disabled?: boolean }) {
  return <select aria-label="Account role" className="rounded-md border bg-background p-2" disabled={disabled} value={value} onChange={e => onChange(e.target.value as AccountRole)}>{roles.map(role => <option key={role}>{role}</option>)}</select>;
}
export function AccountsPage() {
  const client = useQueryClient();
  const accounts = useAccounts();
  const admin = usePlatformAdmin();
  const [selected, setSelected] = useState("");
  const [name, setName] = useState("");
  const [slug, setSlug] = useState("");
  const account = accounts.data?.accounts.find(a => a.id === selected) ?? accounts.data?.accounts[0];
  const create = useMutation({ mutationFn: () => platformApi("platform/accounts/create", { name, slug, kind: "shared" }), onSuccess: async data => { setSelected(data.account.id); setName(""); setSlug(""); await client.invalidateQueries({ queryKey: ["accounts"] }); } });
  return <>
    <PageHeader title="Accounts" description="Accounts group universes, members, and applications. Members see only assigned universes; Creators can create them; Admins manage the whole account." />
    <div className="grid gap-6">
      {accounts.error && <p role="alert">{accounts.error.message}</p>}
      <Field><FieldLabel htmlFor="active-account">Account</FieldLabel><select id="active-account" className="max-w-xl rounded-md border bg-background p-2" value={account?.id ?? ""} onChange={e => setSelected(e.target.value)}>{accounts.data?.accounts.map(a => <option key={a.id} value={a.id}>{a.name} · {a.kind} · {a.role ?? "platform admin"}</option>)}</select></Field>
      {account && <AccountDetails key={account.id} account={account} writable={admin || account.role === "admin"} />}
      {(accounts.data?.canCreate ?? admin) && <Card><CardHeader><CardTitle>New shared account</CardTitle></CardHeader><CardContent><form className="grid max-w-xl gap-3" onSubmit={e => { e.preventDefault(); create.mutate(); }}><Input aria-label="New account name" placeholder="Account name" required value={name} onChange={e => setName(e.target.value)} /><Input aria-label="New account slug" placeholder="account-handle" required pattern="[a-z0-9]+(?:-[a-z0-9]+)*" value={slug} onChange={e => setSlug(e.target.value)} /><Button type="submit" className="w-fit" disabled={create.isPending}>Create account</Button>{create.error && <p role="alert">{create.error.message}</p>}</form></CardContent></Card>}
    </div>
  </>;
}
function AccountDetails({ account, writable }: { account: Account; writable: boolean }) {
  const client = useQueryClient();
  const [userId, setUserId] = useState("");
  const [role, setRole] = useState<AccountRole>("member");
  const [appName, setAppName] = useState("");
  const [appRole, setAppRole] = useState<AccountRole>("member");
  const [selectedApplication, setSelectedApplication] = useState<string | null>(null);
  const accountId = account.id;
  const members = useQuery({ queryKey: ["account-members", accountId], enabled: writable, queryFn: () => platformApi("platform/accounts/members/list", { accountId }) });
  const apps = useQuery({ queryKey: ["applications", accountId], enabled: writable, queryFn: () => platformApi("platform/accounts/applications/list", { accountId }) });
  const refresh = async () => { await Promise.all([client.invalidateQueries({ queryKey: ["accounts"] }), client.invalidateQueries({ queryKey: ["account-members", accountId] }), client.invalidateQueries({ queryKey: ["applications", accountId] }), client.invalidateQueries({ queryKey: ["universes"] })]); };
  const memberPut = useMutation({ mutationFn: (input: { userId: string; role: AccountRole }) => platformApi("platform/accounts/members/put", { accountId, ...input }), onSuccess: refresh });
  const memberRemove = useMutation({ mutationFn: (userId: string) => platformApi("platform/accounts/members/delete", { accountId, userId }), onSuccess: refresh });
  const appCreate = useMutation({ mutationFn: () => platformApi("platform/accounts/applications/create", { accountId, name: appName, role: appRole }), onSuccess: async () => { setAppName(""); await refresh(); } });
  const appUpdate = useMutation({ mutationFn: (input: { applicationId: string; enabled?: boolean; role?: AccountRole }) => platformApi("platform/accounts/applications/update", { accountId, ...input }), onSuccess: refresh });
  const update = useMutation({ mutationFn: (input: { universeQuota?: number; defaultProtected?: boolean; requireProtected?: boolean }) => platformApi("platform/accounts/update", { accountId, ...input }), onSuccess: refresh });
  return <>
    <Card><CardHeader><CardTitle>Account policy</CardTitle><CardDescription>ID: {accountId}. Account Admins can see and administer every universe in this account.</CardDescription></CardHeader><CardContent className="grid max-w-xl gap-3">
      <Field><FieldLabel>Universe quota</FieldLabel><Input aria-label="Universe quota" type="number" min={0} max={100000} defaultValue={account.universeQuota} key={account.universeQuota} disabled={!writable || update.isPending} onBlur={e => { const quota = Number(e.target.value); if (writable && Number.isInteger(quota) && quota >= 0 && quota <= 100000 && e.target.value !== "" && quota !== account.universeQuota) update.mutate({ universeQuota: quota }); }} /></Field>
      <label className="flex gap-2"><input type="checkbox" checked={account.defaultProtected} disabled={!writable || update.isPending} onChange={e => update.mutate({ defaultProtected: e.target.checked })} />Protect new universes by default</label>
      <label className="flex gap-2"><input type="checkbox" checked={account.requireProtected} disabled={!writable || update.isPending} onChange={e => update.mutate({ requireProtected: e.target.checked })} />Require universe protection</label>
    </CardContent></Card>
    {writable && <Card><CardHeader><CardTitle>Account members</CardTitle><CardDescription>Universe Owners grant access to existing account members. Adding users to this account is an Account Admin action.</CardDescription></CardHeader><CardContent className="grid gap-3">
      {members.data?.members.map(m => <div className="flex flex-wrap items-center gap-3" key={m.id}><span className="flex-1">{m.name} <span className="text-sm text-muted-foreground">{m.email}</span></span><RoleSelect value={m.role} disabled={!writable || memberPut.isPending} onChange={role => memberPut.mutate({ userId: m.userId, role })} />{writable && <Button variant="ghost" disabled={memberRemove.isPending} onClick={() => { if (window.confirm(`Remove ${m.name} from this account and disable their universe access?`)) memberRemove.mutate(m.userId); }}>Remove</Button>}</div>)}
      {writable && <form className="flex flex-wrap gap-3 border-t pt-3" onSubmit={e => { e.preventDefault(); memberPut.mutate({ userId, role }); }}><Input className="max-w-sm" aria-label="User ID" placeholder="Existing user ID" required value={userId} onChange={e => setUserId(e.target.value)} /><RoleSelect value={role} onChange={setRole} /><Button type="submit" disabled={memberPut.isPending}>Add member</Button></form>}
    </CardContent></Card>}
    {writable && <Card><CardHeader><CardTitle>Applications</CardTitle><CardDescription>Applications have their own permissions and keys. Disabling an application disables all its API access without deleting its universes.</CardDescription></CardHeader><CardContent className="grid gap-3">
      {apps.data?.applications.map(a => <div className="flex flex-wrap items-center gap-3" key={a.id}><span className="flex-1">{a.name}{!a.enabled && " (disabled)"}</span><RoleSelect value={a.role} disabled={!writable || appUpdate.isPending} onChange={role => appUpdate.mutate({ applicationId: a.id, role })} />{writable && <><Button variant="outline" onClick={() => setSelectedApplication(selectedApplication === a.id ? null : a.id)}>API keys</Button><Button variant="ghost" disabled={appUpdate.isPending} onClick={() => appUpdate.mutate({ applicationId: a.id, enabled: !a.enabled })}>{a.enabled ? "Disable" : "Enable"}</Button></>}</div>)}
      {writable && <form className="flex flex-wrap gap-3 border-t pt-3" onSubmit={e => { e.preventDefault(); appCreate.mutate(); }}><Input className="max-w-sm" aria-label="Application name" placeholder="Application name" value={appName} onChange={e => setAppName(e.target.value)} required /><RoleSelect value={appRole} onChange={setAppRole} /><Button type="submit" disabled={appCreate.isPending}>Create application</Button></form>}
    </CardContent></Card>}
    {!writable && <p className="text-sm text-muted-foreground">An Account Admin manages account members and applications. Universe Owners manage access from the universe’s Members page.</p>}
    {[members.error, apps.error, memberPut.error, memberRemove.error, appCreate.error, appUpdate.error, update.error].filter(Boolean).map((e, i) => <p role="alert" className="text-destructive" key={i}>{e?.message}</p>)}
    {writable && selectedApplication && <ApiKeysCard key={selectedApplication} application={{ accountId, applicationId: selectedApplication }} />}
  </>;
}
