import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type ChannelAccount, type ChannelsStatus } from "@/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { LoadingNote, PageHeader } from "@/components/page";

export function AdminChannelsPage() {
  const queryClient = useQueryClient();
  const accounts = useQuery({
    queryKey: ["channel-accounts"],
    queryFn: () => api<ChannelAccount[]>("GET", "/api/v1/channel-accounts"),
  });
  const status = useQuery({
    queryKey: ["channels-status"],
    queryFn: () => api<ChannelsStatus>("GET", "/api/v1/status/channels"),
    refetchInterval: 10_000,
  });
  const toggle = useMutation({
    mutationFn: ({ id, enabled }: { id: string; enabled: boolean }) =>
      api<ChannelAccount>("PATCH", `/api/v1/channel-accounts/${id}`, { enabled }),
    onSuccess: () =>
      void queryClient.invalidateQueries({ queryKey: ["channel-accounts"] }),
  });

  return (
    <>
      <PageHeader
        title="Channels"
        description="Provider accounts and live connector state for this deployment."
      />
      <div className="grid gap-6">
        <Card>
          <CardHeader>
            <CardTitle>Connectors</CardTitle>
            <CardDescription>
              Refreshes every ten seconds from each connector's private health endpoint.
            </CardDescription>
          </CardHeader>
          <CardContent>
            {status.isLoading && <LoadingNote />}
            {status.error && <p className="text-sm text-destructive">{status.error.message}</p>}
            {status.data && (
              <div className="overflow-x-auto rounded-xl border">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Connector</TableHead>
                      <TableHead>State</TableHead>
                      <TableHead>Ingress</TableHead>
                      <TableHead>Activities</TableHead>
                      <TableHead>Last change</TableHead>
                      <TableHead>Last error</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {status.data.connectors.map((connector) => {
                      const health = connector.health;
                      return (
                        <TableRow key={connector.url}>
                          <TableCell className="font-medium">
                            {health ? `${health.provider} / ${health.accountId}` : connector.url}
                          </TableCell>
                          <TableCell>
                            <Badge variant={health?.state === "ready" ? "secondary" : "destructive"}>
                              {health?.state ?? "unreachable"}
                            </Badge>
                          </TableCell>
                          <TableCell>{yesNo(health?.ingressConnected)}</TableCell>
                          <TableCell>{yesNo(health?.activityWorkerReady)}</TableCell>
                          <TableCell className="text-muted-foreground">
                            {health ? new Date(health.changedAtMs).toLocaleString() : "—"}
                          </TableCell>
                          <TableCell className="max-w-sm text-sm text-muted-foreground">
                            {health?.lastError ?? connector.error ?? health?.detail ?? "—"}
                          </TableCell>
                        </TableRow>
                      );
                    })}
                  </TableBody>
                </Table>
              </div>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Channel accounts</CardTitle>
            <CardDescription>
              Bindings target one stable provider account. Secret values stay outside Postgres.
            </CardDescription>
          </CardHeader>
          <CardContent className="grid gap-5">
            {accounts.isLoading && <LoadingNote />}
            {accounts.error && <p className="text-sm text-destructive">{accounts.error.message}</p>}
            {accounts.data && (
              <div className="overflow-x-auto rounded-xl border">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Name</TableHead>
                      <TableHead>Provider</TableHead>
                      <TableHead>Account id</TableHead>
                      <TableHead>Status</TableHead>
                      <TableHead className="w-0" />
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {accounts.data.map((account) => (
                      <TableRow key={account.id}>
                        <TableCell className="font-medium">{account.displayName}</TableCell>
                        <TableCell>{account.provider}</TableCell>
                        <TableCell className="font-mono text-xs">{account.accountId}</TableCell>
                        <TableCell>
                          <Badge variant={account.enabled ? "secondary" : "outline"}>
                            {account.enabled ? "enabled" : "disabled"}
                          </Badge>
                        </TableCell>
                        <TableCell>
                          <Button
                            variant="ghost"
                            size="sm"
                            disabled={toggle.isPending}
                            onClick={() => toggle.mutate({ id: account.id, enabled: !account.enabled })}
                          >
                            {account.enabled ? "Disable" : "Enable"}
                          </Button>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </div>
            )}
            <CreateAccountForm />
          </CardContent>
        </Card>
      </div>
    </>
  );
}

function CreateAccountForm() {
  const queryClient = useQueryClient();
  const [provider, setProvider] = useState<"telegram" | "whatsapp">("telegram");
  const [accountId, setAccountId] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [credentialRef, setCredentialRef] = useState("");
  const [stateRef, setStateRef] = useState("");
  const create = useMutation({
    mutationFn: () =>
      api<ChannelAccount>("POST", "/api/v1/channel-accounts", {
        provider,
        accountId: accountId.trim(),
        displayName: displayName.trim(),
        credentialRef: credentialRef.trim() || null,
        stateRef: stateRef.trim() || null,
        settings: {},
      }),
    onSuccess: async () => {
      setAccountId("");
      setDisplayName("");
      setCredentialRef("");
      setStateRef("");
      await queryClient.invalidateQueries({ queryKey: ["channel-accounts"] });
    },
  });
  const submit = (event: FormEvent) => {
    event.preventDefault();
    create.mutate();
  };

  return (
    <form onSubmit={submit} className="grid gap-3 rounded-xl border p-4 md:grid-cols-2">
      <div className="md:col-span-2">
        <h3 className="text-sm font-semibold">Add account</h3>
      </div>
      <Field>
        <FieldLabel htmlFor="channel-provider">Provider</FieldLabel>
        <select
          id="channel-provider"
          className="h-9 rounded-md border bg-transparent px-3 text-sm"
          value={provider}
          onChange={(event) => setProvider(event.target.value as typeof provider)}
        >
          <option value="telegram">Telegram</option>
          <option value="whatsapp">WhatsApp</option>
        </select>
      </Field>
      <Field>
        <FieldLabel htmlFor="channel-account-id">Stable account id</FieldLabel>
        <Input id="channel-account-id" value={accountId} onChange={(e) => setAccountId(e.target.value)} required />
      </Field>
      <Field>
        <FieldLabel htmlFor="channel-display-name">Display name</FieldLabel>
        <Input id="channel-display-name" value={displayName} onChange={(e) => setDisplayName(e.target.value)} required />
      </Field>
      <Field>
        <FieldLabel htmlFor="channel-credential-ref">Credential reference</FieldLabel>
        <Input id="channel-credential-ref" value={credentialRef} onChange={(e) => setCredentialRef(e.target.value)} placeholder="optional opaque reference" />
      </Field>
      <Field>
        <FieldLabel htmlFor="channel-state-ref">State reference</FieldLabel>
        <Input id="channel-state-ref" value={stateRef} onChange={(e) => setStateRef(e.target.value)} placeholder="optional opaque reference" />
      </Field>
      <div className="flex items-end md:col-span-2">
        <Button type="submit" disabled={create.isPending || !accountId.trim() || !displayName.trim()}>
          {create.isPending ? "Adding…" : "Add account"}
        </Button>
      </div>
      {create.error && <p className="text-sm text-destructive md:col-span-2">{create.error.message}</p>}
    </form>
  );
}

function yesNo(value: boolean | undefined): string {
  return value === undefined ? "—" : value ? "yes" : "no";
}
