import { useState, type FormEvent } from "react";
import { useQuery } from "@tanstack/react-query";
import { useSearchParams } from "react-router-dom";
import type { SessionUser } from "@/auth";
import { useDocumentTitle } from "@/lib/document-title";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Checkbox } from "@/components/ui/checkbox";

type DeviceReview = { user_code: string; status: "pending" | "approved" | "denied"; client_id?: string; scope?: string };
async function deviceRequest(path: string, body?: unknown) {
  const response = await fetch(`/api/auth${path}`, { credentials: "same-origin", redirect: "error",
    ...(body ? { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) } : {}) });
  const value = await response.json() as { error_description?: string };
  if (!response.ok) throw new Error(value.error_description ?? "This authorization request is unavailable. Start login again from your CLI.");
  return value;
}

export function DevicePage({ user }: { user: SessionUser }) {
  useDocumentTitle(["Authorize CLI"]);
  const [search, setSearch] = useSearchParams();
  const code = (search.get("user_code") ?? "").replace(/[^a-zA-Z0-9]/g, "").toUpperCase();
  const [draft, setDraft] = useState(code);
  const [confirmed, setConfirmed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [outcome, setOutcome] = useState<"approved" | "denied" | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const review = useQuery({ queryKey: ["device-review", user.id, code], enabled: code.length === 8,
    queryFn: async () => await deviceRequest(`/device?${new URLSearchParams({ user_code: code })}`) as DeviceReview,
    retry: false, staleTime: Infinity, refetchOnWindowFocus: false });
  const owned = review.data?.client_id === "lightspeed-cli";
  const status = outcome ?? review.data?.status;
  const submit = (event: FormEvent) => { event.preventDefault(); setConfirmed(false); setOutcome(null); setFailure(null); setSearch({ user_code: draft }); };
  const decide = async (decision: "approve" | "deny") => {
    if (!owned || status !== "pending" || (decision === "approve" && !confirmed)) return;
    setBusy(true); setFailure(null);
    try { await deviceRequest(`/device/${decision}`, { userCode: code }); setOutcome(decision === "approve" ? "approved" : "denied"); }
    catch (error) { setFailure(error instanceof Error ? error.message : "Authorization failed"); }
    finally { setBusy(false); }
  };
  return <div className="flex min-h-svh items-center justify-center p-4"><Card className="w-full max-w-lg">
    <CardHeader><CardTitle>Authorize Lightspeed CLI</CardTitle><CardDescription>Signed in as {user.name} ({user.email})</CardDescription></CardHeader>
    <CardContent className="grid gap-5">
      {!code && <form onSubmit={submit} className="grid gap-3"><Field><FieldLabel htmlFor="device-code">Code shown in your terminal</FieldLabel>
        <Input id="device-code" value={draft} onChange={event => setDraft(event.target.value)} autoComplete="off" maxLength={20} required /></Field><Button type="submit">Review request</Button></form>}
      {code && <><p className="text-center font-mono text-2xl tracking-widest" aria-label="Device code">{code}</p>
        {code.length !== 8 && <p role="alert">The code must contain eight letters or digits.</p>}
        {review.isFetching && <p>Loading request…</p>}
        {review.error && <p role="alert">{review.error.message}</p>}
        {review.data && !owned && <p role="alert">This code belongs to another account or an unsupported client. Start login again from your CLI.</p>}
        {owned && status === "pending" && <>
          <p><strong>Client: Lightspeed CLI</strong> (lightspeed-cli)</p>
          <p>This signs the CLI in as you, with your current account and universe permissions. It can also manage your API keys.</p>
          <p>Approve only a login you started on a device you control. Do not approve codes sent to you by someone else.</p>
          <label className="flex items-start gap-3"><Checkbox checked={confirmed} onCheckedChange={checked => setConfirmed(Boolean(checked))} />The code above matches the code in my terminal.</label>
          <div className="flex gap-3"><Button disabled={busy || !confirmed} onClick={() => void decide("approve")}>Approve CLI</Button>
            <Button variant="outline" disabled={busy} onClick={() => void decide("deny")}>Deny</Button></div>
        </>}
        {owned && status === "approved" && <p role="status">Approved. Return to your terminal to finish signing in.</p>}
        {owned && status === "denied" && <p role="status">Denied. No CLI session was issued.</p>}
        {failure && <p role="alert">{failure}</p>}
        <Button variant="link" disabled={busy} onClick={() => { setSearch({}); setOutcome(null); setConfirmed(false); setFailure(null); }}>Enter a different code</Button>
      </>}
    </CardContent>
  </Card></div>;
}
