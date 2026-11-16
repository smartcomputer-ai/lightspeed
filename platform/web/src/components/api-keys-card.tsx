import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { platformApi } from "@/lib/platform-api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Field, FieldLabel } from "@/components/ui/field";
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from "@/components/ui/card";

const split = (value: string) => value.split(",").map(v => v.trim()).filter(Boolean);
export function ApiKeysCard({ application }: { application?: { accountId: string; applicationId: string } }) {
  const client = useQueryClient();
  const queryKey = ["api-keys", application?.applicationId ?? "personal"];
  const [name, setName] = useState("");
  const [days, setDays] = useState("");
  const [accounts, setAccounts] = useState("");
  const [universes, setUniverses] = useState("");
  const [operations, setOperations] = useState("");
  const [readOnly, setReadOnly] = useState(false);
  const [allowCreation, setAllowCreation] = useState(false);
  const [secret, setSecret] = useState<string | null>(null);
  const keys = useQuery({ queryKey, queryFn: () => application
    ? platformApi("platform/applications/credentials/list", application)
    : platformApi("platform/credentials/list", {}) });
  const refresh = () => client.invalidateQueries({ queryKey });
  const create = useMutation({ mutationFn: () => {
    const input = { name, ...(days ? { expiresInDays: Number(days) } : {}), restrictions: {
      ...(accounts.trim() ? { accountIds: split(accounts) } : {}),
      ...(universes.trim() ? { universeIds: split(universes) } : {}),
      ...(operations.trim() ? { operations: split(operations) } : {}),
      readOnly, allowUniverseCreation: allowCreation && !universes.trim() && !readOnly,
    } };
    return application ? platformApi("platform/applications/credentials/create", { ...input, ...application }) : platformApi("platform/credentials/create", input);
  }, onSuccess: async result => { setSecret(result.secret); setName(""); await refresh(); } });
  const revoke = useMutation({ mutationFn: (keyId: string) => application
    ? platformApi("platform/applications/credentials/revoke", { ...application, keyId })
    : platformApi("platform/credentials/revoke", { keyId }), onSuccess: refresh });
  return <Card id={application ? undefined : "api-keys"}>
    <CardHeader><CardTitle>{application ? "Application API keys" : "Personal API keys"}</CardTitle><CardDescription>Keys use the principal’s current permissions, narrowed by any restrictions below. Creating a replacement leaves existing keys valid until you revoke them.</CardDescription></CardHeader>
    <CardContent className="grid gap-4">
      {secret && <div className="grid gap-2 rounded-md border p-3" role="status"><p>Copy this key now. It is shown only once.</p><code className="break-all select-all">{secret}</code><Button variant="outline" onClick={() => setSecret(null)}>I have saved it</Button></div>}
      <form className="grid max-w-xl gap-3" onSubmit={e => { e.preventDefault(); create.mutate(); }}>
        <Field><FieldLabel>Name</FieldLabel><Input aria-label="API key name" value={name} onChange={e => setName(e.target.value)} required maxLength={120} /></Field>
        <Field><FieldLabel>Expiry in days</FieldLabel><Input aria-label="API key expiry in days" type="number" min={1} max={3650} value={days} onChange={e => setDays(e.target.value)} placeholder="Deployment default" /></Field>
        <details><summary className="cursor-pointer text-sm">Restrict access</summary><div className="mt-3 grid gap-3">
          <Field><FieldLabel>Account IDs, comma separated</FieldLabel><Input aria-label="Allowed account IDs" value={accounts} onChange={e => setAccounts(e.target.value)} placeholder="All accounts this principal can access" /></Field>
          <Field><FieldLabel>Universe UUIDs, comma separated</FieldLabel><Input aria-label="Allowed universe UUIDs" value={universes} onChange={e => setUniverses(e.target.value)} placeholder="All universes this principal can access" /></Field>
          <Field><FieldLabel>Operation IDs, comma separated</FieldLabel><Input aria-label="Allowed operations" value={operations} onChange={e => setOperations(e.target.value)} placeholder="For example: profiles/read, profiles/list" /></Field>
          <label className="flex gap-2 text-sm"><input type="checkbox" checked={readOnly} onChange={e => setReadOnly(e.target.checked)} />Read only</label>
        </div></details>
        <label className="flex gap-2 text-sm"><input type="checkbox" checked={allowCreation && !universes.trim() && !readOnly} disabled={!!universes.trim() || readOnly} onChange={e => setAllowCreation(e.target.checked)} />Allow universe creation where the principal is a Creator or Admin</label>
        <Button type="submit" className="w-fit" disabled={create.isPending || !!secret || !name.trim()}>Create API key</Button>
      </form>
      {[keys.error, create.error, revoke.error].filter(Boolean).map((e, i) => <p role="alert" className="text-destructive" key={i}>{e?.message}</p>)}
      {keys.data?.credentials.map(key => <div className="flex flex-wrap items-center justify-between gap-3 border-t pt-3" key={key.id}><div><p className="font-medium">{key.name}</p><p className="text-sm text-muted-foreground">{key.start}… · expires {new Date(key.expiresAt).toLocaleDateString()} · {key.enabled ? "active" : "revoked"}</p><p className="text-xs text-muted-foreground">{key.restrictions.readOnly ? "Read only. " : ""}{key.restrictions.accountIds?.length ?? "All"} accounts; {key.restrictions.universeIds?.length ?? "all"} universes; {key.restrictions.operations?.length ?? "all"} operations</p></div><Button variant="outline" disabled={revoke.isPending || !key.enabled} onClick={() => { if (window.confirm(`Revoke “${key.name}”? Clients using it will lose access.`)) revoke.mutate(key.id); }}>Revoke</Button></div>)}
    </CardContent>
  </Card>;
}
