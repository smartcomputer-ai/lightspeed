import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  api,
  connectorAccountHealth,
  type ChannelConnectorHealth,
  type ChannelConnectorStatus,
  type ChannelsStatus,
  type DeploymentChannelAccountListResponse,
} from "@/api";
import { Badge } from "@/components/ui/badge";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  Table,
  TableBody,
  TableCard,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  TableTitleCell,
} from "@/components/ui/table";
import { LoadingNote, PageHeader } from "@/components/page";
import { useUniverses } from "@/lib/universes";

/** Deployment-wide diagnostics. Account configuration belongs to each universe. */
export function AdminChannelsPage() {
  const accounts = useQuery({
    queryKey: ["admin-channel-accounts"],
    queryFn: () => api<DeploymentChannelAccountListResponse>("GET", "/api/v1/channel-accounts"),
  });
  const universes = useUniverses();
  const status = useQuery({
    queryKey: ["channels-status"],
    queryFn: () => api<ChannelsStatus>("GET", "/api/v1/status/channels"),
    refetchInterval: 10_000,
  });
  const universeByCoreId = useMemo(
    () => new Map((universes.data ?? []).map((universe) => [universe.lightspeedUniverseId, universe])),
    [universes.data],
  );
  const healthRows: Array<{
    connector: ChannelConnectorStatus;
    health: ChannelConnectorHealth | null;
  }> = [];
  for (const connector of status.data?.connectors ?? []) {
    const rows = connectorAccountHealth(connector);
    if (rows.length === 0) {
      healthRows.push({ connector, health: null });
    } else {
      healthRows.push(...rows.map((health) => ({ connector, health })));
    }
  }

  return (
    <>
      <PageHeader
        title="Channels"
        description="Deployment-wide connector health and account inventory. Universe owners manage their connections from Channels."
      />
      <div className="grid gap-6">
        <Card>
          <CardHeader>
            <CardTitle>Connector health</CardTitle>
            <CardDescription>
              Runtime diagnostics from each configured connector host, refreshed every ten seconds.
            </CardDescription>
          </CardHeader>
          <CardContent>
            {status.isLoading && <LoadingNote />}
            {status.error && <p className="text-sm text-destructive">{status.error.message}</p>}
            {status.data && healthRows.length === 0 && (
              <p className="text-sm text-muted-foreground">No connector hosts are configured.</p>
            )}
            {status.data && healthRows.length > 0 && (
              <TableCard>
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Account</TableHead>
                      <TableHead>Universe</TableHead>
                      <TableHead>State</TableHead>
                      <TableHead>Ingress</TableHead>
                      <TableHead>Activities</TableHead>
                      <TableHead>Last error</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {healthRows.map(({ connector, health }, index) => (
                      <TableRow key={`${connector.url}/${health?.accountId ?? index}`}>
                        <TableCell className="font-medium">
                          {health ? `${health.provider} / ${health.accountId}` : connector.url}
                        </TableCell>
                        <TableCell className="text-muted-foreground">
                          {health?.universeId
                            ? universeByCoreId.get(health.universeId)?.name ?? health.universeId
                            : "—"}
                        </TableCell>
                        <TableCell>
                          <Badge variant={health?.state === "ready" ? "secondary" : "destructive"}>
                            {health?.state ?? (connector.reachable ? "no accounts" : "unreachable")}
                          </Badge>
                        </TableCell>
                        <TableCell>{yesNo(health?.ingressConnected)}</TableCell>
                        <TableCell>{yesNo(health?.activityWorkerReady)}</TableCell>
                        <TableCell className="max-w-sm text-sm text-muted-foreground">
                          {health?.lastError ?? connector.error ?? health?.detail ?? "—"}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </TableCard>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Account inventory</CardTitle>
            <CardDescription>
              Read-only deployment view. Open the owning universe to connect, enable, or disable an account.
            </CardDescription>
          </CardHeader>
          <CardContent>
            {accounts.isLoading && <LoadingNote />}
            {accounts.error && <p className="text-sm text-destructive">{accounts.error.message}</p>}
            {accounts.data && (
              <TableCard>
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Account</TableHead>
                      <TableHead>Provider</TableHead>
                      <TableHead>Universe</TableHead>
                      <TableHead>Status</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {(accounts.data.accounts ?? []).map((account) => (
                      <TableRow key={`${account.universeId}/${account.accountId}`}>
                        <TableTitleCell title={account.displayName} subtitle={account.accountId} />
                        <TableCell>
                          {account.provider}
                          <span className="block text-xs text-muted-foreground">{account.providerAccountId}</span>
                        </TableCell>
                        <TableCell className="text-muted-foreground">
                          {universeByCoreId.get(account.universeId)?.name ?? account.universeId}
                        </TableCell>
                        <TableCell>
                          <Badge variant={(account.enabled ?? true) ? "secondary" : "outline"}>
                            {(account.enabled ?? true) ? "enabled" : "disabled"}
                          </Badge>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </TableCard>
            )}
          </CardContent>
        </Card>
      </div>
    </>
  );
}

function yesNo(value: boolean | undefined): string {
  return value === undefined ? "—" : value ? "yes" : "no";
}
