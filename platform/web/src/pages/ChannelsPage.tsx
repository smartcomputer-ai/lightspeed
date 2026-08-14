import { useState, type FormEvent } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { bindingCreateSchema } from "@lightspeed/platform-shared";
import { Check, Copy, Plus, Power, RotateCw, Trash2 } from "lucide-react";
import { api, type Binding, type ChannelAccount, type ProfileSummary } from "@/api";
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
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Field, FieldLabel } from "@/components/ui/field";
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
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import {
  LoadingNote,
  PageHeader,
  SectionHeader,
  UniverseNotFound,
} from "@/components/page";
import { canManage, useActiveUniverse } from "@/lib/universes";

export function ChannelsPage({ admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();

  if (isLoading) {
    return <LoadingNote />;
  }
  if (!universe) {
    return <UniverseNotFound slug={slug} />;
  }

  const writable = canManage(universe, admin);

  return <RoutingRules universeId={universe.id} writable={writable} />;
}

function RoutingRules({
  universeId,
  writable,
}: {
  universeId: string;
  writable: boolean;
}) {
  const queryClient = useQueryClient();
  const [createOpen, setCreateOpen] = useState(false);
  const bindings = useQuery({
    queryKey: ["bindings", universeId],
    queryFn: () => api<Binding[]>("GET", `/api/v1/universes/${universeId}/bindings`),
  });
  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: ["bindings", universeId] });

  const rotate = useMutation({
    mutationFn: (bindingId: string) =>
      api("POST", `/api/v1/bindings/${bindingId}/rotate-pairing`),
    onSuccess: invalidate,
  });
  const toggle = useMutation({
    mutationFn: (binding: Binding) =>
      api("PATCH", `/api/v1/bindings/${binding.id}`, { enabled: !binding.enabled }),
    onSuccess: invalidate,
  });
  const remove = useMutation({
    mutationFn: (bindingId: string) => api("DELETE", `/api/v1/bindings/${bindingId}`),
    onSuccess: invalidate,
  });

  const rows = (bindings.data ?? []).slice().sort((a, b) => a.priority - b.priority);

  return (
    <>
      <PageHeader
        title="Channels"
        description="Connect Telegram and WhatsApp conversations to this universe."
        actions={
          writable ? (
            <Button onClick={() => setCreateOpen(true)}>
              <Plus data-icon="inline-start" />
              Add routing rule
            </Button>
          ) : undefined
        }
      />
      <section>
        <SectionHeader
          title="Routing rules"
          description="Which conversations reach this universe, and under which profile."
        />
        {bindings.isLoading && <LoadingNote />}
        {bindings.error && (
          <p className="text-sm text-destructive">{bindings.error.message}</p>
        )}
        {bindings.data && rows.length === 0 && (
          <p className="mb-4 text-sm text-muted-foreground">
            No routing rules — this universe is not reachable from any chat.
          </p>
        )}
        {rows.length > 0 && (
          <div className="mb-6 overflow-hidden rounded-xl border">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead className="whitespace-normal">Rule</TableHead>
                  <TableHead className="whitespace-normal">Account</TableHead>
                  <TableHead>Scope</TableHead>
                  <TableHead className="whitespace-normal">Profile</TableHead>
                  <TableHead>Pairing code</TableHead>
                  <TableHead>Status</TableHead>
                  {writable && (
                    <TableHead className="sticky right-0 z-20 w-0 border-l bg-background shadow-[-8px_0_12px_-12px_rgba(0,0,0,0.45)]">
                      <span className="sr-only">Actions</span>
                    </TableHead>
                  )}
                </TableRow>
              </TableHeader>
              <TableBody>
                {rows.map((binding) => (
                  <TableRow key={binding.id} className="group">
                    <TableCell className="max-w-40 whitespace-normal break-all font-mono text-xs">
                      {binding.name}
                    </TableCell>
                    <TableCell className="max-w-52 whitespace-normal">
                      <span className="block font-medium">
                        {binding.channelAccount.provider}
                      </span>
                      <span className="block break-words text-muted-foreground">
                        {binding.channelAccount.displayName}
                      </span>
                      <span className="block break-all font-mono text-xs text-muted-foreground">
                        {binding.channelAccount.accountId}
                      </span>
                    </TableCell>
                    <TableCell>{binding.matchScope ?? "any"}</TableCell>
                    <TableCell className="max-w-40 whitespace-normal break-all">
                      {binding.profileId ?? "default"}
                    </TableCell>
                    <TableCell>
                      <PairingCode code={binding.pairingCode} />
                    </TableCell>
                    <TableCell>
                      <Badge variant={binding.enabled ? "secondary" : "outline"}>
                        {binding.enabled ? "enabled" : "disabled"}
                      </Badge>
                    </TableCell>
                    {writable && (
                      <TableCell className="sticky right-0 z-10 whitespace-nowrap border-l bg-background shadow-[-8px_0_12px_-12px_rgba(0,0,0,0.45)] group-hover:bg-muted/50">
                        <Tooltip>
                          <TooltipTrigger
                            render={
                              <Button
                                variant="ghost"
                                size="icon-sm"
                                onClick={() => rotate.mutate(binding.id)}
                              />
                            }
                          >
                            <RotateCw />
                          </TooltipTrigger>
                          <TooltipContent>Mint a fresh pairing code</TooltipContent>
                        </Tooltip>
                        <Tooltip>
                          <TooltipTrigger
                            render={
                              <Button
                                variant="ghost"
                                size="icon-sm"
                                onClick={() => toggle.mutate(binding)}
                              />
                            }
                          >
                            <Power />
                          </TooltipTrigger>
                          <TooltipContent>
                            {binding.enabled ? "Disable" : "Enable"}
                          </TooltipContent>
                        </Tooltip>
                        <DeleteRuleButton
                          name={binding.name}
                          onConfirm={() => remove.mutate(binding.id)}
                        />
                      </TableCell>
                    )}
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        )}
      </section>
      {writable && (
        <AddRuleDialog
          universeId={universeId}
          open={createOpen}
          onOpenChange={setCreateOpen}
          onDone={invalidate}
        />
      )}
    </>
  );
}

function PairingCode({ code }: { code: string | null }) {
  const [copied, setCopied] = useState(false);
  if (!code) {
    return <span className="text-muted-foreground">open</span>;
  }
  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <button
            type="button"
            className="inline-flex items-center gap-1.5 rounded-md bg-muted px-2 py-0.5 font-mono text-xs hover:bg-accent"
            onClick={() => {
              void navigator.clipboard.writeText(code);
              setCopied(true);
              setTimeout(() => setCopied(false), 1500);
            }}
          />
        }
      >
        {code}
        {copied ? <Check className="size-3" /> : <Copy className="size-3" />}
      </TooltipTrigger>
      <TooltipContent>Send this code in a chat to pair it</TooltipContent>
    </Tooltip>
  );
}

function DeleteRuleButton({
  name,
  onConfirm,
}: {
  name: string;
  onConfirm: () => void;
}) {
  return (
    <AlertDialog>
      <AlertDialogTrigger
        render={<Button variant="ghost" size="icon-sm" className="text-destructive" />}
      >
        <Trash2 />
      </AlertDialogTrigger>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>Delete rule {name}?</AlertDialogTitle>
          <AlertDialogDescription>
            Chats paired through this rule stop reaching the universe.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction
            className="bg-destructive text-white hover:bg-destructive/90"
            onClick={onConfirm}
          >
            Delete
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

function AddRuleDialog({
  universeId,
  open,
  onOpenChange,
  onDone,
}: {
  universeId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onDone: () => void;
}) {
  const [name, setName] = useState("");
  const [channelAccountId, setChannelAccountId] = useState("");
  const [scope, setScope] = useState("any");
  const [profile, setProfile] = useState("");
  const [error, setError] = useState<string | null>(null);

  // Only rendered for owners/admins, so the gated profiles endpoint is fine.
  const profiles = useQuery({
    queryKey: ["profiles", universeId],
    queryFn: () => api<ProfileSummary[]>("GET", `/api/v1/universes/${universeId}/profiles`),
  });
  const accounts = useQuery({
    queryKey: ["channel-accounts"],
    queryFn: () => api<ChannelAccount[]>("GET", "/api/v1/channel-accounts"),
  });

  const add = useMutation({
    mutationFn: (input: unknown) =>
      api("POST", `/api/v1/universes/${universeId}/bindings`, input),
    onSuccess: () => {
      setName("");
      setChannelAccountId("");
      setProfile("");
      setError(null);
      onOpenChange(false);
      onDone();
    },
    onError: (err) => setError(err.message),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    setError(null);
    // Same schema the API enforces — one source of validation truth.
    const parsed = bindingCreateSchema.safeParse({
      name,
      channelAccountId,
      matchScope: scope === "any" ? null : scope,
      profileId: profile || null,
      sessionKey: `${name}-${new Date().toISOString().slice(0, 10).replaceAll("-", "")}`,
    });
    if (!parsed.success) {
      const issue = parsed.error.issues[0];
      setError(issue ? `${issue.path.join(".")}: ${issue.message}` : "invalid input");
      return;
    }
    add.mutate(parsed.data);
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Add routing rule</DialogTitle>
          <DialogDescription>
            Choose which conversations reach this universe and which profile handles them.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <div className="grid gap-4 sm:grid-cols-2">
            <Field>
              <FieldLabel htmlFor="rule-name">Rule name</FieldLabel>
              <Input
                id="rule-name"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="my-group"
                required
              />
            </Field>
            <Field>
              <FieldLabel htmlFor="rule-account">Channel account</FieldLabel>
              <Select
                value={channelAccountId}
                onValueChange={(value) => setChannelAccountId(value as string)}
              >
                <SelectTrigger id="rule-account" className="w-full">
                  <SelectValue placeholder="Select an account" />
                </SelectTrigger>
                <SelectContent>
                  {(accounts.data ?? []).filter((account) => account.enabled).map((account) => (
                    <SelectItem key={account.id} value={account.id}>
                      {account.provider} · {account.displayName}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
            <Field>
              <FieldLabel htmlFor="rule-scope">Scope</FieldLabel>
              <Select
                value={scope}
                onValueChange={(value) => setScope(value as string)}
              >
                <SelectTrigger id="rule-scope" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="any">any</SelectItem>
                  <SelectItem value="direct">direct</SelectItem>
                  <SelectItem value="group">group</SelectItem>
                </SelectContent>
              </Select>
            </Field>
            <Field>
              <FieldLabel htmlFor="rule-profile">Profile</FieldLabel>
              <Select
                value={profile}
                onValueChange={(value) => setProfile((value as string) ?? "")}
              >
                <SelectTrigger id="rule-profile" className="w-full">
                  <SelectValue>
                    {(value: string) =>
                      value
                        ? (profiles.data?.find((p) => p.profileId === value)
                            ?.displayName ?? value)
                        : "Default (no profile)"
                    }
                  </SelectValue>
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="">Default (no profile)</SelectItem>
                  {(profiles.data ?? []).map((p) => (
                    <SelectItem key={p.profileId} value={p.profileId}>
                      {p.displayName ?? p.profileId}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
          </div>
          {error && <p className="text-sm text-destructive">{error}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={add.isPending}>
              Add rule
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
