import { useMemo, useState, type FormEvent } from "react";
import { Link } from "react-router-dom";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Plus, RadioTower, Unplug } from "lucide-react";
import {
  api,
  type ChannelAccountInput,
  type ChannelAccountListResponse,
  type ChannelAccountView,
  type ChannelConnectorHealth,
  type ChannelPairingListResponse,
  type ChannelPairingView,
  type UniverseChannelStatus,
} from "@/api";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
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
  TableActionsCell,
  TableBody,
  TableCard,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
  TableTitleCell,
} from "@/components/ui/table";
import { CenteredNote, LoadingNote, PageHeader, UniverseNotFound } from "@/components/page";
import { canManage, useActiveUniverse } from "@/lib/universes";

export function ChannelsPage({ admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();
  if (isLoading) return <LoadingNote />;
  if (!universe || !canManage(universe, admin)) return <UniverseNotFound slug={slug} />;
  return <Channels universeId={universe.id} slug={universe.slug} />;
}

function Channels({ universeId, slug }: { universeId: string; slug: string }) {
  const queryClient = useQueryClient();
  const [connectOpen, setConnectOpen] = useState(false);
  const accounts = useQuery({
    queryKey: ["channel-accounts", universeId],
    queryFn: () => api<ChannelAccountListResponse>(
      "GET",
      `/api/v1/universes/${universeId}/channel-accounts`,
    ),
  });
  const pairings = useQuery({
    queryKey: ["channel-pairings", universeId],
    queryFn: () => api<ChannelPairingListResponse>(
      "GET",
      `/api/v1/universes/${universeId}/channel-pairings`,
    ),
  });
  const status = useQuery({
    queryKey: ["channel-status", universeId],
    queryFn: () => api<UniverseChannelStatus>(
      "GET",
      `/api/v1/universes/${universeId}/channel-status`,
    ),
    refetchInterval: 10_000,
  });
  const healthByAccount = useMemo(() => new Map(
    (status.data?.accounts ?? [])
      .map((health) => [health.accountId, health]),
  ), [status.data]);
  const accountRows = accounts.data?.accounts ?? [];
  const pairingRows = pairings.data?.pairings ?? [];

  const toggle = useMutation({
    mutationFn: (account: ChannelAccountView) =>
      api("PUT", `/api/v1/universes/${universeId}/channel-accounts/${account.accountId}`, {
        account: { ...accountInputOf(account), enabled: !(account.enabled ?? true) },
        expectedRevision: account.revision,
      }),
    onSuccess: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["channel-accounts", universeId] }),
        queryClient.invalidateQueries({ queryKey: ["channel-status", universeId] }),
      ]);
    },
  });
  const unpair = useMutation({
    mutationFn: (pairing: ChannelPairingView) => api(
      "DELETE",
      `/api/v1/universes/${universeId}/channel-pairings/${encodeURIComponent(pairing.accountId)}/${encodeURIComponent(pairing.chatId)}`,
    ),
    onSuccess: () => queryClient.invalidateQueries({
      queryKey: ["channel-pairings", universeId],
    }),
  });

  return (
    <>
      <PageHeader
        title="Channels"
        description="Connect messaging accounts to this universe, then route conversations from your bots."
        actions={
          <Button onClick={() => setConnectOpen(true)}>
            <Plus data-icon="inline-start" />
            Connect channel
          </Button>
        }
      />
      <ConnectChannelDialog
        universeId={universeId}
        open={connectOpen}
        onOpenChange={setConnectOpen}
      />
      {(accounts.isLoading || pairings.isLoading) && <LoadingNote />}
      {accounts.error && <p className="mb-4 text-sm text-destructive">{accounts.error.message}</p>}
      {status.error && (
        <p className="mb-4 text-sm text-destructive">
          Connector health unavailable: {status.error.message}
        </p>
      )}
      {toggle.error && <p className="mb-4 text-sm text-destructive">{toggle.error.message}</p>}

      {accounts.data && accountRows.length === 0 ? (
        <CenteredNote>
          <RadioTower className="mx-auto size-6" />
          <span className="font-medium text-foreground">No messaging accounts connected</span>
          <span>Connect a Telegram bot or WhatsApp number without managing credentials separately.</span>
          <Button className="mx-auto mt-2" onClick={() => setConnectOpen(true)}>
            Connect channel
          </Button>
        </CenteredNote>
      ) : (
        <div className="grid gap-6">
          <Card>
            <CardHeader>
              <CardTitle>Messaging accounts</CardTitle>
              <CardDescription>
                Credentials are encrypted automatically. Add a chat trigger on a bot to start routing messages.
              </CardDescription>
            </CardHeader>
            <CardContent>
              {accounts.data && (
                <TableCard>
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead>Account</TableHead>
                        <TableHead>Provider</TableHead>
                        <TableHead>Connection</TableHead>
                        <TableHead>Conversations</TableHead>
                        <TableHead className="w-0" />
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {accountRows.map((account) => (
                        <TableRow key={account.accountId}>
                          <TableTitleCell title={account.displayName} subtitle={account.accountId} />
                          <TableCell>
                            <span className="capitalize">{account.provider}</span>
                            <span className="block text-xs text-muted-foreground">
                              {providerAccountLabel(account)}
                            </span>
                          </TableCell>
                          <TableCell>
                            <AccountStatusBadge
                              account={account}
                              health={healthByAccount.get(account.accountId)}
                            />
                          </TableCell>
                          <TableCell className="text-muted-foreground">
                            {pairingRows.filter((pairing) => pairing.accountId === account.accountId).length}
                          </TableCell>
                          <TableActionsCell>
                            <Button
                              variant="ghost"
                              size="sm"
                              disabled={toggle.isPending}
                              onClick={() => toggle.mutate(account)}
                            >
                              {(account.enabled ?? true) ? "Disable" : "Enable"}
                            </Button>
                          </TableActionsCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                </TableCard>
              )}
            </CardContent>
          </Card>

          <Card>
            <CardHeader className="flex flex-row items-start justify-between gap-4">
              <div className="grid gap-1.5">
                <CardTitle>Connected conversations</CardTitle>
                <CardDescription>
                  Pairings bind one provider conversation to one bot trigger. Unpairing makes that conversation connect again.
                </CardDescription>
              </div>
              <Button
                variant="outline"
                size="sm"
                nativeButton={false} render={<Link to={`/u/${slug}/bots`} />}
              >
                Configure bots
              </Button>
            </CardHeader>
            <CardContent>
              {pairings.error && <p className="text-sm text-destructive">{pairings.error.message}</p>}
              {pairings.data && pairingRows.length === 0 && (
                <p className="text-sm text-muted-foreground">
                  No conversations have connected yet.
                </p>
              )}
              {pairings.data && pairingRows.length > 0 && (
                <TableCard>
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead>Account</TableHead>
                        <TableHead>Bot trigger</TableHead>
                        <TableHead>Conversation</TableHead>
                        <TableHead>Connected</TableHead>
                        <TableHead className="w-0" />
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {pairingRows.map((pairing) => (
                        <TableRow key={`${pairing.accountId}/${pairing.chatId}`}>
                          <TableCell>{accountName(accountRows, pairing.accountId)}</TableCell>
                          <TableCell>
                            {pairing.botId}
                            <span className="block text-xs text-muted-foreground">{pairing.triggerId}</span>
                          </TableCell>
                          <TableCell className="font-mono text-xs">{pairing.chatId}</TableCell>
                          <TableCell className="text-muted-foreground">
                            {new Date(pairing.pairedAtMs).toLocaleString()}
                          </TableCell>
                          <TableActionsCell>
                            <AlertDialog>
                              <AlertDialogTrigger
                                render={
                                  <Button
                                    variant="ghost"
                                    size="icon-sm"
                                    className="text-destructive"
                                    aria-label={`Unpair conversation ${pairing.chatId}`}
                                  />
                                }
                              >
                                <Unplug />
                              </AlertDialogTrigger>
                              <AlertDialogContent>
                                <AlertDialogHeader>
                                  <AlertDialogTitle>Unpair this conversation?</AlertDialogTitle>
                                  <AlertDialogDescription>
                                    It stops routing to {pairing.botId}. The conversation must connect or present a pairing code again.
                                  </AlertDialogDescription>
                                </AlertDialogHeader>
                                <AlertDialogFooter>
                                  <AlertDialogCancel>Cancel</AlertDialogCancel>
                                  <AlertDialogAction
                                    className="bg-destructive text-white hover:bg-destructive/90"
                                    onClick={() => unpair.mutate(pairing)}
                                  >
                                    Unpair
                                  </AlertDialogAction>
                                </AlertDialogFooter>
                              </AlertDialogContent>
                            </AlertDialog>
                          </TableActionsCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                </TableCard>
              )}
            </CardContent>
          </Card>
        </div>
      )}
    </>
  );
}

function ConnectChannelDialog({
  universeId,
  open,
  onOpenChange,
}: {
  universeId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const queryClient = useQueryClient();
  const [provider, setProvider] = useState<"telegram" | "whatsapp">("telegram");
  const [token, setToken] = useState("");
  const [phoneNumber, setPhoneNumber] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [printQr, setPrintQr] = useState(true);

  const connect = useMutation({
    mutationFn: () => api(
      "POST",
      `/api/v1/universes/${universeId}/channel-accounts/connect`,
      provider === "telegram"
        ? {
            provider,
            token: token.trim(),
            ...(displayName.trim() ? { displayName: displayName.trim() } : {}),
          }
        : {
            provider,
            phoneNumber: phoneNumber.trim(),
            printQr,
            ...(displayName.trim() ? { displayName: displayName.trim() } : {}),
          },
    ),
    onSuccess: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["channel-accounts", universeId] }),
        queryClient.invalidateQueries({ queryKey: ["channel-status", universeId] }),
        queryClient.invalidateQueries({ queryKey: ["secrets", universeId] }),
      ]);
      setToken("");
      setPhoneNumber("");
      setDisplayName("");
      onOpenChange(false);
    },
  });
  const reset = () => {
    setProvider("telegram");
    setToken("");
    setPhoneNumber("");
    setDisplayName("");
    setPrintQr(true);
    connect.reset();
  };
  const submit = (event: FormEvent) => {
    event.preventDefault();
    connect.mutate();
  };
  const incomplete = provider === "telegram" ? !token.trim() : !phoneNumber.trim();

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        onOpenChange(next);
        if (!next) reset();
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Connect a channel</DialogTitle>
          <DialogDescription>
            Add the provider credential here. Lightspeed creates and manages the encrypted access grant automatically.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <Field>
            <FieldLabel>Provider</FieldLabel>
            <Select
              value={provider}
              onValueChange={(value) => value && setProvider(value as typeof provider)}
            >
              <SelectTrigger className="w-full" aria-label="Channel provider">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="telegram">Telegram</SelectItem>
                <SelectItem value="whatsapp">WhatsApp</SelectItem>
              </SelectContent>
            </Select>
          </Field>

          {provider === "telegram" ? (
            <Field>
              <FieldLabel htmlFor="telegram-bot-token">Bot token</FieldLabel>
              <Input
                id="telegram-bot-token"
                type="password"
                value={token}
                onChange={(event) => setToken(event.target.value)}
                autoComplete="new-password"
                spellCheck={false}
                className="font-mono"
                placeholder="Paste the token from BotFather"
                autoFocus
              />
              <FieldDescription>
                Lightspeed checks the token with Telegram and fills in the bot username for you. The token is encrypted and never shown again.
              </FieldDescription>
            </Field>
          ) : (
            <>
              <Field>
                <FieldLabel htmlFor="whatsapp-phone">Phone number</FieldLabel>
                <Input
                  id="whatsapp-phone"
                  value={phoneNumber}
                  onChange={(event) => setPhoneNumber(event.target.value)}
                  placeholder="+41 79 123 45 67"
                  autoFocus
                />
                <FieldDescription>
                  After connecting, scan the QR code printed by the connector host.
                </FieldDescription>
              </Field>
              <label className="flex items-start gap-3 rounded-lg border bg-muted/15 p-3">
                <Checkbox
                  checked={printQr}
                  onCheckedChange={(checked) => setPrintQr(checked === true)}
                />
                <span className="grid gap-1">
                  <span className="text-sm font-medium">Print pairing QR code</span>
                  <span className="text-xs text-muted-foreground">
                    Show the QR code in the connector process output until this account is paired.
                  </span>
                </span>
              </label>
            </>
          )}

          <Field>
            <FieldLabel htmlFor="channel-display-name">Display name (optional)</FieldLabel>
            <Input
              id="channel-display-name"
              value={displayName}
              onChange={(event) => setDisplayName(event.target.value)}
              placeholder={provider === "telegram" ? "Defaults to the Telegram bot name" : "Support WhatsApp"}
            />
          </Field>
          {connect.error && <p className="text-sm text-destructive">{connect.error.message}</p>}
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => {
                onOpenChange(false);
                reset();
              }}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={connect.isPending || incomplete}>
              {connect.isPending
                ? "Connecting…"
                : provider === "telegram"
                  ? "Connect Telegram"
                  : "Connect WhatsApp"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function accountInputOf(account: ChannelAccountView): ChannelAccountInput {
  return {
    accountId: account.accountId,
    provider: account.provider,
    providerAccountId: account.providerAccountId,
    displayName: account.displayName,
    credentialGrantId: account.credentialGrantId ?? null,
    enabled: account.enabled ?? true,
    settings: account.settings ?? {},
  };
}

function providerAccountLabel(account: ChannelAccountView): string {
  return account.provider === "telegram"
    ? `@${account.providerAccountId.replace(/^@/, "")}`
    : account.providerAccountId;
}

function accountName(accounts: ChannelAccountView[], accountId: string): string {
  return accounts.find((account) => account.accountId === accountId)?.displayName ?? accountId;
}

export function channelAccountConnectionState(
  account: Pick<ChannelAccountView, "enabled">,
  health: Pick<ChannelConnectorHealth, "state" | "detail" | "lastError"> | undefined,
): { label: string; detail?: string; healthy: boolean } {
  if (account.enabled === false) return { label: "Disabled", healthy: false };
  if (!health) return { label: "Waiting for connector", healthy: false };
  if (health.state === "ready") return { label: "Connected", healthy: true };
  const label = health.state === "starting"
    ? "Connecting"
    : health.state === "disconnected"
      ? "Disconnected"
      : health.state === "failed"
        ? "Failed"
        : health.state[0]!.toUpperCase() + health.state.slice(1);
  return { label, detail: health.lastError ?? health.detail, healthy: false };
}

function AccountStatusBadge({
  account,
  health,
}: {
  account: ChannelAccountView;
  health: ChannelConnectorHealth | undefined;
}) {
  const status = channelAccountConnectionState(account, health);
  return (
    <div className="grid gap-1">
      <Badge variant={status.healthy ? "secondary" : status.label === "Failed" ? "destructive" : "outline"}>
        {status.label}
      </Badge>
      {status.detail && (
        <span className="max-w-64 truncate text-xs text-muted-foreground" title={status.detail}>
          {status.detail}
        </span>
      )}
    </div>
  );
}
