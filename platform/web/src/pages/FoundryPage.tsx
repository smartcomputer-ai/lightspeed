import { useState, type FormEvent, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ExternalLink, Plus, Settings2, Webhook } from "lucide-react";
import { NavLink, useNavigate, useParams } from "react-router-dom";
import {
  api,
  type Environment,
  type FoundryPack,
  type FoundryPackState,
  type FoundryRelease,
  type ProfileSummary,
} from "@/api";
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
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { LoadingNote, UniverseNotFound } from "@/components/page";
import { canManage, useActiveUniverse } from "@/lib/universes";
import { cn } from "@/lib/utils";
import { SessionDetail } from "./SessionsPage";

const DEFAULT_PROFILE = "foundry-manager";
const AUTO_ENVIRONMENT = "__auto__";
const NO_RUNTIME = "__none__";

export function FoundryPage({ admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();
  const { packId } = useParams<{ packId?: string }>();

  if (isLoading) return <LoadingNote />;
  if (!universe) {
    return (
      <div className="p-6">
        <UniverseNotFound slug={slug} />
      </div>
    );
  }

  const manage = canManage(universe, admin);
  return (
    <div className="flex min-h-0 min-w-0 flex-1">
      <aside
        className={cn(
          "w-full shrink-0 flex-col border-r md:flex md:w-80",
          packId ? "hidden" : "flex",
        )}
      >
        <FoundryPane
          universeId={universe.id}
          slug={slug!}
          activeId={packId}
          manage={manage}
        />
      </aside>
      <section className={cn("min-w-0 flex-1 flex-col", packId ? "flex" : "hidden md:flex")}>
        {packId ? (
          <PackWorkspace
            key={packId}
            universeId={universe.id}
            slug={slug!}
            packId={packId}
            manage={manage}
          />
        ) : (
          <div className="flex flex-1 items-center justify-center p-6 text-sm text-muted-foreground">
            Select a pack{manage ? ", or register one." : "."}
          </div>
        )}
      </section>
    </div>
  );
}

function FoundryPane({
  universeId,
  slug,
  activeId,
  manage,
}: {
  universeId: string;
  slug: string;
  activeId: string | undefined;
  manage: boolean;
}) {
  const packs = useQuery({
    queryKey: ["foundry-packs", universeId],
    queryFn: () =>
      api<{ packs: FoundryPack[] }>("GET", `/api/v1/universes/${universeId}/foundry-packs`),
  });
  const [createOpen, setCreateOpen] = useState(false);

  return (
    <>
      <div className="flex h-12 shrink-0 items-center gap-2 border-b px-4">
        <h1 className="text-sm font-semibold">Foundry</h1>
        {packs.data && <span className="text-xs text-muted-foreground">{packs.data.packs.length}</span>}
        {manage && (
          <Button
            variant="ghost"
            size="icon-sm"
            className="ml-auto"
            onClick={() => setCreateOpen(true)}
            aria-label="Register pack"
          >
            <Plus />
          </Button>
        )}
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {packs.isLoading && <p className="p-4 text-sm text-muted-foreground">Loading…</p>}
        {packs.error && <p className="p-4 text-sm text-destructive">{packs.error.message}</p>}
        {packs.data?.packs.length === 0 && (
          <p className="p-4 text-sm text-muted-foreground">
            No packs yet{manage ? " — register one to create its manager." : "."}
          </p>
        )}
        <ul>
          {packs.data?.packs.map((pack) => (
            <li key={pack.id}>
              <NavLink
                to={`/u/${slug}/foundry/${pack.id}`}
                className={cn(
                  "flex flex-col gap-0.5 border-b px-4 py-2.5 text-sm hover:bg-muted/50",
                  pack.id === activeId && "bg-muted",
                )}
              >
                <span className="flex items-center gap-2">
                  <span className="truncate font-medium">{pack.name}</span>
                  {!pack.enabled && <span className="ml-auto text-xs text-destructive">Disabled</span>}
                </span>
                <span className="truncate text-xs text-muted-foreground">{repositoryLabel(pack.repoUrl)}</span>
              </NavLink>
            </li>
          ))}
        </ul>
      </div>
      {manage && (
        <RegisterPackDialog
          universeId={universeId}
          slug={slug}
          open={createOpen}
          onOpenChange={setCreateOpen}
        />
      )}
    </>
  );
}

function RegisterPackDialog({
  universeId,
  slug,
  open,
  onOpenChange,
}: {
  universeId: string;
  slug: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const [name, setName] = useState("");
  const [repoUrl, setRepoUrl] = useState("");
  const [error, setError] = useState<string | null>(null);
  const register = useMutation({
    mutationFn: () =>
      api<{ pack: FoundryPack }>("POST", `/api/v1/universes/${universeId}/foundry-packs`, {
        name: name.trim(),
        repoUrl: repoUrl.trim(),
        managerProfileId: DEFAULT_PROFILE,
      }),
    onSuccess: async ({ pack }) => {
      await queryClient.invalidateQueries({ queryKey: ["foundry-packs", universeId] });
      setName("");
      setRepoUrl("");
      setError(null);
      onOpenChange(false);
      navigate(`/u/${slug}/foundry/${pack.id}`);
    },
    onError: (err) => setError(err.message),
  });

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Register pack</DialogTitle>
          <DialogDescription>
            Create a persistent manager session and event address for this software system.
          </DialogDescription>
        </DialogHeader>
        <form
          onSubmit={(event) => {
            event.preventDefault();
            setError(null);
            register.mutate();
          }}
          className="grid gap-4"
        >
          <Field>
            <FieldLabel htmlFor="pack-name">Pack name</FieldLabel>
            <Input id="pack-name" value={name} onChange={(event) => setName(event.target.value)} autoFocus />
            <FieldDescription>Lowercase letters, numbers, and dashes.</FieldDescription>
          </Field>
          <Field>
            <FieldLabel htmlFor="pack-repository">Repository URL</FieldLabel>
            <Input
              id="pack-repository"
              type="url"
              placeholder="https://github.com/acme/my-pack.git"
              value={repoUrl}
              onChange={(event) => setRepoUrl(event.target.value)}
            />
            <FieldDescription>The repository should document its contract in FOUNDRY.md.</FieldDescription>
          </Field>
          <p className="text-xs text-muted-foreground">
            The Fleet-capable Foundry Manager profile is created automatically. You can switch profiles and environments after registration.
          </p>
          {error && <p className="text-sm text-destructive">{error}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>Cancel</Button>
            <Button type="submit" disabled={register.isPending || !name.trim() || !repoUrl.trim()}>
              {register.isPending ? "Registering…" : "Register"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function PackWorkspace({
  universeId,
  slug,
  packId,
  manage,
}: {
  universeId: string;
  slug: string;
  packId: string;
  manage: boolean;
}) {
  const pack = useQuery({
    queryKey: ["foundry-pack", packId],
    queryFn: () => api<{ pack: FoundryPack }>("GET", `/api/v1/foundry/packs/${packId}`),
  });
  const state = useQuery({
    queryKey: ["foundry-state", packId],
    queryFn: () => api<{ state: FoundryPackState }>("GET", `/api/v1/foundry/packs/${packId}/state`),
    refetchInterval: 3_000,
    retry: true,
  });
  const releases = useQuery({
    queryKey: ["foundry-releases", packId],
    queryFn: () => api<{ releases: FoundryRelease[] }>("GET", `/api/v1/foundry/packs/${packId}/releases`),
    refetchInterval: 10_000,
  });

  if (pack.isLoading) return <DetailNote>Loading…</DetailNote>;
  if (pack.error || !pack.data) return <DetailNote destructive>{pack.error?.message ?? "Pack not found"}</DetailNote>;
  const current = pack.data.pack;

  return (
    <div className="flex min-h-0 flex-1">
      <div className="flex min-w-0 flex-1 flex-col">
        <CompactPackBar
          universeId={universeId}
          pack={current}
          state={state.data?.state}
          manage={manage}
        />
        {state.data?.state.sessionReady ? (
          <SessionDetail
            universeId={universeId}
            slug={slug}
            sessionId={state.data.state.sessionId}
            backTo={`/u/${slug}/foundry`}
          />
        ) : (
          <DetailNote destructive={Boolean(state.error)}>
            {state.error ? `Manager unavailable: ${state.error.message}` : "Starting manager…"}
          </DetailNote>
        )}
      </div>
      <PackControlPanel
        universeId={universeId}
        pack={current}
        state={state.data?.state}
        releases={releases.data?.releases ?? []}
        manage={manage}
      />
    </div>
  );
}

function CompactPackBar({
  universeId,
  pack,
  state,
  manage,
}: {
  universeId: string;
  pack: FoundryPack;
  state?: FoundryPackState;
  manage: boolean;
}) {
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [eventOpen, setEventOpen] = useState(false);
  return (
    <div className="flex h-12 shrink-0 items-center gap-2 border-b px-4 xl:hidden">
      <span className="min-w-0 truncate text-sm font-semibold">{pack.name}</span>
      <StatusBadge status={state?.controllerStatus} />
      {manage && (
        <div className="ml-auto flex items-center gap-1">
          <Button variant="ghost" size="icon-sm" onClick={() => setEventOpen(true)} aria-label="Send event"><Webhook /></Button>
          <Button variant="ghost" size="icon-sm" onClick={() => setSettingsOpen(true)} aria-label="Pack settings"><Settings2 /></Button>
        </div>
      )}
      {manage && (
        <>
          <PackSettingsDialog universeId={universeId} pack={pack} open={settingsOpen} onOpenChange={setSettingsOpen} />
          <ManualEventDialog packId={pack.id} open={eventOpen} onOpenChange={setEventOpen} />
        </>
      )}
    </div>
  );
}

function PackControlPanel({
  universeId,
  pack,
  state,
  releases,
  manage,
}: {
  universeId: string;
  pack: FoundryPack;
  state?: FoundryPackState;
  releases: FoundryRelease[];
  manage: boolean;
}) {
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [eventOpen, setEventOpen] = useState(false);
  return (
    <aside className="hidden w-80 shrink-0 overflow-y-auto border-l xl:block">
      <div className="flex h-12 items-center gap-2 border-b px-4">
        <span className="truncate text-sm font-semibold">{pack.name}</span>
        <StatusBadge status={state?.controllerStatus} />
        {manage && (
          <Button variant="ghost" size="icon-sm" className="ml-auto" onClick={() => setSettingsOpen(true)} aria-label="Pack settings">
            <Settings2 />
          </Button>
        )}
      </div>
      <div className="grid gap-6 p-4 text-sm">
        <section className="grid gap-2">
          <PanelHeading title="System" />
          <a className="flex min-w-0 items-center gap-1.5 hover:underline" href={pack.repoUrl} target="_blank" rel="noreferrer">
            <span className="truncate">{repositoryLabel(pack.repoUrl)}</span>
            <ExternalLink className="size-3.5 shrink-0" />
          </a>
          <KeyValue label="Profile" value={pack.managerProfileId} />
          <KeyValue label="Environment" value={state?.environmentId ?? pack.environmentId ?? "Resolving…"} />
          <KeyValue label="Runtime" value={pack.runtimeTarget ? `${pack.runtimeTarget.kind}: ${pack.runtimeTarget.name}` : "Not registered"} />
          {state?.lastError && <p className="rounded-md bg-destructive/10 p-2 text-xs text-destructive">{state.lastError}</p>}
        </section>

        <section className="grid gap-2">
          <div className="flex items-center gap-2">
            <PanelHeading title="Event inbox" />
            {manage && (
              <Button variant="outline" size="xs" className="ml-auto" onClick={() => setEventOpen(true)}>
                <Webhook data-icon="inline-start" /> Send event
              </Button>
            )}
          </div>
          <KeyValue label="Pending" value={String(state?.pendingEventCount ?? 0)} />
          {state?.activeEvent && <EventRow id={state.activeEvent.id} status="active" />}
          {state?.recentEvents.slice().reverse().slice(0, 8).map((event) => (
            <EventRow key={event.id} id={event.id} status={event.status} summary={event.summary ?? event.failure} />
          ))}
          {state && !state.activeEvent && state.recentEvents.length === 0 && (
            <p className="text-xs text-muted-foreground">No events delivered yet.</p>
          )}
        </section>

        <section className="grid gap-2">
          <PanelHeading title="Releases" />
          {releases.slice(0, 6).map((release) => (
            <div key={release.id} className="rounded-md border p-2 text-xs">
              <div className="flex items-center gap-2">
                <code className="truncate">{release.artifactDigest}</code>
                <Badge variant={release.outcome === "failed" ? "destructive" : "outline"}>{release.outcome}</Badge>
              </div>
              <p className="mt-1 truncate text-muted-foreground">{release.target} · {release.sourceCommit.slice(0, 10)}</p>
            </div>
          ))}
          {releases.length === 0 && <p className="text-xs text-muted-foreground">No recorded releases.</p>}
        </section>
      </div>
      {manage && (
        <>
          <PackSettingsDialog universeId={universeId} pack={pack} open={settingsOpen} onOpenChange={setSettingsOpen} />
          <ManualEventDialog packId={pack.id} open={eventOpen} onOpenChange={setEventOpen} />
        </>
      )}
    </aside>
  );
}

function PackSettingsDialog({ universeId, pack, open, onOpenChange }: { universeId: string; pack: FoundryPack; open: boolean; onOpenChange: (open: boolean) => void }) {
  const queryClient = useQueryClient();
  const profiles = useQuery({
    queryKey: ["profiles", universeId],
    queryFn: () => api<ProfileSummary[]>("GET", `/api/v1/universes/${universeId}/profiles`),
    enabled: open,
  });
  const environments = useQuery({
    queryKey: ["environments", universeId],
    queryFn: () => api<Environment[]>("GET", `/api/v1/universes/${universeId}/environments`),
    enabled: open,
  });
  const [profileId, setProfileId] = useState(pack.managerProfileId);
  const [environmentId, setEnvironmentId] = useState(pack.environmentId ?? AUTO_ENVIRONMENT);
  const [runtimeKind, setRuntimeKind] = useState<string>(pack.runtimeTarget?.kind ?? NO_RUNTIME);
  const [runtimeName, setRuntimeName] = useState(pack.runtimeTarget?.name ?? "");
  const [error, setError] = useState<string | null>(null);
  const save = useMutation({
    mutationFn: () => api<{ pack: FoundryPack }>("PATCH", `/api/v1/foundry/packs/${pack.id}`, {
      managerProfileId: profileId,
      environmentId: environmentId === AUTO_ENVIRONMENT ? null : environmentId,
      runtimeTarget: runtimeKind === NO_RUNTIME ? null : { kind: runtimeKind, name: runtimeName.trim() },
    }),
    onSuccess: async ({ pack: updated }) => {
      queryClient.setQueryData(["foundry-pack", pack.id], { pack: updated });
      await queryClient.invalidateQueries({ queryKey: ["foundry-state", pack.id] });
      onOpenChange(false);
    },
    onError: (err) => setError(err.message),
  });

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Pack configuration</DialogTitle>
          <DialogDescription>Changes are applied to the manager at its next idle boundary.</DialogDescription>
        </DialogHeader>
        <div className="grid gap-4">
          <Field>
            <FieldLabel>Manager profile</FieldLabel>
            <Select value={profileId} onValueChange={(value) => value && setProfileId(value)}>
              <SelectTrigger><SelectValue /></SelectTrigger>
              <SelectContent>
                {!profiles.data?.some((profile) => profile.profileId === DEFAULT_PROFILE) && <SelectItem value={DEFAULT_PROFILE}>Foundry Manager</SelectItem>}
                {profiles.data?.map((profile) => <SelectItem key={profile.profileId} value={profile.profileId}>{profile.displayName ?? profile.profileId}</SelectItem>)}
              </SelectContent>
            </Select>
            <FieldDescription>The profile must grant Fleet and durable environment jobs.</FieldDescription>
          </Field>
          <Field>
            <FieldLabel>Development / operations environment</FieldLabel>
            <Select value={environmentId} onValueChange={(value) => value && setEnvironmentId(value)}>
              <SelectTrigger><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value={AUTO_ENVIRONMENT}>Resolve from pack tags</SelectItem>
                {environments.data?.map((environment) => <SelectItem key={environment.environmentId} value={environment.environmentId}>{environment.environmentId}</SelectItem>)}
              </SelectContent>
            </Select>
          </Field>
          <Field>
            <FieldLabel>Runtime target</FieldLabel>
            <Select value={runtimeKind} onValueChange={(value) => value && setRuntimeKind(value)}>
              <SelectTrigger><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value={NO_RUNTIME}>Not registered</SelectItem>
                <SelectItem value="docker">Docker</SelectItem>
                <SelectItem value="kubernetes">Kubernetes</SelectItem>
                <SelectItem value="ssh">SSH / VM</SelectItem>
                <SelectItem value="external">External</SelectItem>
              </SelectContent>
            </Select>
          </Field>
          {runtimeKind !== NO_RUNTIME && (
            <Field>
              <FieldLabel htmlFor="runtime-name">Target name</FieldLabel>
              <Input id="runtime-name" value={runtimeName} onChange={(event) => setRuntimeName(event.target.value)} placeholder="packs-prod" />
            </Field>
          )}
          {error && <p className="text-sm text-destructive">{error}</p>}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>Cancel</Button>
          <Button onClick={() => save.mutate()} disabled={save.isPending || !profileId || (runtimeKind !== NO_RUNTIME && !runtimeName.trim())}>
            {save.isPending ? "Saving…" : "Save"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function ManualEventDialog({ packId, open, onOpenChange }: { packId: string; open: boolean; onOpenChange: (open: boolean) => void }) {
  const queryClient = useQueryClient();
  const [kind, setKind] = useState("operator.requested");
  const [summary, setSummary] = useState("");
  const [data, setData] = useState("");
  const [error, setError] = useState<string | null>(null);
  const send = useMutation({
    mutationFn: () => {
      let parsed: unknown = undefined;
      if (data.trim()) parsed = JSON.parse(data);
      return api("POST", `/api/v1/foundry/packs/${packId}/events`, { kind: kind.trim(), summary: summary.trim(), ...(parsed === undefined ? {} : { data: parsed }) });
    },
    onSuccess: async () => {
      setSummary("");
      setData("");
      setError(null);
      onOpenChange(false);
      await queryClient.invalidateQueries({ queryKey: ["foundry-state", packId] });
    },
    onError: (err) => setError(err instanceof Error ? err.message : String(err)),
  });
  const submit = (event: FormEvent) => {
    event.preventDefault();
    setError(null);
    send.mutate();
  };
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Send Foundry event</DialogTitle>
          <DialogDescription>Queue a durable request for the manager. This does not itself authorize production changes.</DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <Field><FieldLabel htmlFor="event-kind">Kind</FieldLabel><Input id="event-kind" value={kind} onChange={(event) => setKind(event.target.value)} /></Field>
          <Field><FieldLabel htmlFor="event-summary">Summary</FieldLabel><Textarea id="event-summary" value={summary} onChange={(event) => setSummary(event.target.value)} rows={4} autoFocus /></Field>
          <Field><FieldLabel htmlFor="event-data">Data (optional JSON)</FieldLabel><Textarea id="event-data" value={data} onChange={(event) => setData(event.target.value)} rows={5} className="font-mono text-xs" /></Field>
          {error && <p className="text-sm text-destructive">{error}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>Cancel</Button>
            <Button type="submit" disabled={send.isPending || !kind.trim() || !summary.trim()}>{send.isPending ? "Sending…" : "Send event"}</Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function StatusBadge({ status }: { status?: FoundryPackState["controllerStatus"] }) {
  if (!status) return <Badge variant="outline">starting</Badge>;
  if (status === "degraded") return <Badge variant="destructive">degraded</Badge>;
  if (status === "idle") return <Badge variant="secondary">idle</Badge>;
  return <Badge variant="outline">{status.replaceAll("_", " ")}</Badge>;
}

function EventRow({ id, status, summary }: { id: string; status: string; summary?: string }) {
  return (
    <div className="rounded-md border p-2 text-xs">
      <div className="flex items-center gap-2"><code className="min-w-0 flex-1 truncate">{id}</code><Badge variant={status === "run_failed" || status === "blocked" ? "destructive" : "outline"}>{status}</Badge></div>
      {summary && <p className="mt-1 line-clamp-2 text-muted-foreground">{summary}</p>}
    </div>
  );
}

function PanelHeading({ title }: { title: string }) {
  return <h2 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">{title}</h2>;
}

function KeyValue({ label, value }: { label: string; value: string }) {
  return <div className="grid grid-cols-[5rem_minmax(0,1fr)] gap-2 text-xs"><span className="text-muted-foreground">{label}</span><span className="truncate">{value}</span></div>;
}

function DetailNote({ children, destructive = false }: { children: ReactNode; destructive?: boolean }) {
  return <div className={cn("flex flex-1 items-center justify-center p-6 text-sm text-muted-foreground", destructive && "text-destructive")}>{children}</div>;
}

function repositoryLabel(repoUrl: string): string {
  try {
    const url = new URL(repoUrl);
    return `${url.host}${url.pathname}`.replace(/\.git$/, "").replace(/\/$/, "");
  } catch {
    return repoUrl.replace(/^https?:\/\//, "").replace(/\.git$/, "");
  }
}
