import { useEffect, useMemo, useState, type FormEvent, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { NavLink, useNavigate, useParams } from "react-router-dom";
import { slugify } from "@lightspeed/platform-shared";
import { ChevronRight, Plus, Trash2 } from "lucide-react";
import {
  api,
  type Environment,
  type ProfileDocument,
  type ProfileSummary,
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
import { Button } from "@/components/ui/button";
import { ProfileEnvironmentEditor } from "@/components/session/profile-environment-editor";
import { SessionConfigEditor } from "@/components/session/session-config-editor";
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
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { LoadingNote, UniverseNotFound } from "@/components/page";
import { useSessionConfigEditorOptions } from "@/lib/sessions/editor-options";
import {
  hasSessionFeature,
  resourceFeatureDisableReasons,
  setupResourceFeatureError,
} from "@/lib/sessions/resource-features";
import { canManage, useActiveUniverse } from "@/lib/universes";
import { cn } from "@/lib/utils";

/// Structured profile editor. Pane = profile list; detail = sectioned form
/// with a reusable SessionConfig editor and a permanent Edit-as-JSON tab.
/// Saves keep the profiles/put +
/// expectedRevision flow — a stale editor gets a 409, not a clobber.
export function ProfilesPage({ admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();
  const { profileId } = useParams<{ profileId: string }>();

  if (isLoading) {
    return <LoadingNote />;
  }
  if (!universe || !canManage(universe, admin)) {
    return (
      <div className="p-6">
        <UniverseNotFound slug={slug} />
      </div>
    );
  }

  return (
    <div className="flex min-h-0 flex-1">
      <aside
        className={cn(
          "w-full shrink-0 flex-col border-r md:flex md:w-80",
          profileId ? "hidden" : "flex",
        )}
      >
        <ProfilePane universeId={universe.id} slug={slug!} activeId={profileId} />
      </aside>
      <div
        className={cn("min-w-0 flex-1 flex-col", profileId ? "flex" : "hidden md:flex")}
      >
        {profileId ? (
          <ProfileEditor
            key={profileId}
            universeId={universe.id}
            slug={slug!}
            profileId={profileId}
          />
        ) : (
          <div className="flex flex-1 items-center justify-center p-6 text-sm text-muted-foreground">
            Select a profile, or create one.
          </div>
        )}
      </div>
    </div>
  );
}

function ProfilePane({
  universeId,
  slug,
  activeId,
}: {
  universeId: string;
  slug: string;
  activeId: string | undefined;
}) {
  const profiles = useQuery({
    queryKey: ["profiles", universeId],
    queryFn: () => api<ProfileSummary[]>("GET", `/api/v1/universes/${universeId}/profiles`),
  });
  const [createOpen, setCreateOpen] = useState(false);

  return (
    <>
      <div className="flex h-12 shrink-0 items-center gap-2 border-b px-4">
        <h1 className="text-sm font-semibold">Profiles</h1>
        <Button
          variant="ghost"
          size="icon-sm"
          className="ml-auto"
          onClick={() => setCreateOpen(true)}
          aria-label="New profile"
        >
          <Plus />
        </Button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {profiles.isLoading && <p className="p-4 text-sm text-muted-foreground">Loading…</p>}
        {profiles.error && (
          <p className="p-4 text-sm text-destructive">{profiles.error.message}</p>
        )}
        {profiles.data && profiles.data.length === 0 && (
          <p className="p-4 text-sm text-muted-foreground">
            No profiles yet. Routing rules reference profiles by label — create one to
            give bound chats their configuration.
          </p>
        )}
        <ul>
          {profiles.data?.map((profile) => (
            <li key={profile.profileId}>
              <NavLink
                to={`/u/${slug}/profiles/${profile.profileId}`}
                className={cn(
                  "flex flex-col gap-0.5 border-b px-4 py-2.5 text-sm hover:bg-muted/50",
                  profile.profileId === activeId && "bg-muted",
                )}
              >
                <span className="flex items-center gap-2">
                  <span className="truncate font-medium">
                    {profile.displayName ?? profile.profileId}
                  </span>
                  <span className="ml-auto shrink-0 text-xs text-muted-foreground">
                    r{profile.revision}
                  </span>
                </span>
                <span className="truncate text-xs text-muted-foreground">
                  {profile.description ?? profile.profileId}
                </span>
              </NavLink>
            </li>
          ))}
        </ul>
      </div>
      <NewProfileDialog
        universeId={universeId}
        slug={slug}
        open={createOpen}
        onOpenChange={setCreateOpen}
      />
    </>
  );
}

function ProfileEditor({
  universeId,
  slug,
  profileId,
}: {
  universeId: string;
  slug: string;
  profileId: string;
}) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const doc = useQuery({
    queryKey: ["profile", universeId, profileId],
    queryFn: () =>
      api<ProfileDocument>("GET", `/api/v1/universes/${universeId}/profiles/${profileId}`),
    staleTime: 0,
  });

  const [original, setOriginal] = useState<ProfileDocument | null>(null);
  const [draft, setDraft] = useState<ProfileDocument | null>(null);
  const [tab, setTab] = useState<"form" | "json">("form");
  const [jsonText, setJsonText] = useState("");
  const [configError, setConfigError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Re-sync on every (re)load — including the refetch after a save, which
  // brings the new revision. The active tab is deliberately left alone.
  useEffect(() => {
    if (doc.data) {
      const { createdAtMs, updatedAtMs, ...editable } = doc.data;
      setOriginal(editable as ProfileDocument);
      setDraft(structuredClone(editable) as ProfileDocument);
      setJsonText(JSON.stringify(editable, null, 2));
      setConfigError(null);
      setError(null);
    }
  }, [doc.data]);

  const mutate = (fn: (d: ProfileDocument) => void) => {
    setDraft((prev) => {
      if (!prev) return prev;
      const next = structuredClone(prev);
      fn(next);
      return next;
    });
  };

  const dirty = useMemo(() => {
    if (!draft || !original) return false;
    if (tab === "json") return jsonText !== JSON.stringify(draft, null, 2);
    return JSON.stringify(draft) !== JSON.stringify(original);
  }, [draft, original, tab, jsonText]);

  const save = useMutation({
    mutationFn: (document: ProfileDocument) =>
      api("PUT", `/api/v1/universes/${universeId}/profiles/${profileId}`, document),
    onSuccess: () => {
      setError(null);
      void queryClient.invalidateQueries({ queryKey: ["profiles", universeId] });
      void queryClient.invalidateQueries({ queryKey: ["profile", universeId, profileId] });
    },
    onError: (err) => setError(err.message),
  });

  const remove = useMutation({
    mutationFn: () =>
      api("DELETE", `/api/v1/universes/${universeId}/profiles/${profileId}`),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["profiles", universeId] });
      navigate(`/u/${slug}/profiles`);
    },
    onError: (err) => setError(err.message),
  });

  const submit = () => {
    setError(null);
    let document = draft;
    if (tab === "json") {
      try {
        document = JSON.parse(jsonText) as ProfileDocument;
      } catch (parseError) {
        setError(`invalid JSON: ${(parseError as Error).message}`);
        return;
      }
      if (document.profileId !== profileId) {
        setError("profileId cannot be changed — create a new profile instead");
        return;
      }
      setDraft(document);
    } else if (configError) {
      setError(`Config: ${configError}`);
      return;
    }
    if (document) {
      const resourceError = setupResourceFeatureError(document);
      if (resourceError) {
        setError(resourceError);
        return;
      }
      save.mutate(document);
    }
  };

  const switchTab = (next: "form" | "json") => {
    if (next === tab || !draft) return;
    if (next === "json") {
      setJsonText(JSON.stringify(draft, null, 2));
      setTab("json");
      return;
    }
    // JSON → form: apply the text if it parses; keep the user in JSON
    // with an error if it doesn't.
    try {
      const parsed = JSON.parse(jsonText) as ProfileDocument;
      if (parsed.profileId !== profileId) {
        setError("profileId cannot be changed — create a new profile instead");
        return;
      }
      setDraft(parsed);
      setConfigError(null);
      setError(null);
      setTab("form");
    } catch (parseError) {
      setError(`invalid JSON: ${(parseError as Error).message}`);
    }
  };

  if (doc.isLoading || !draft) {
    return (
      <div className="p-6">
        {doc.error ? (
          <p className="text-sm text-destructive">{doc.error.message}</p>
        ) : (
          <LoadingNote />
        )}
      </div>
    );
  }

  return (
    <>
      <header className="flex h-12 shrink-0 items-center gap-3 border-b px-4">
        <NavLink to={`/u/${slug}/profiles`} className="md:hidden">
          <ChevronRight className="size-4 rotate-180" />
        </NavLink>
        <h1 className="truncate text-sm font-semibold">
          {(draft.displayName as string) || profileId}
        </h1>
        <code className="shrink-0 rounded bg-muted px-1.5 py-0.5 font-mono text-xs text-muted-foreground">
          {profileId}
        </code>
        <span className="shrink-0 text-xs text-muted-foreground">
          r{(draft.revision as number) ?? "?"}
        </span>
        <div className="ml-auto flex shrink-0 items-center gap-2">
          <Tabs value={tab} onValueChange={(v) => switchTab(v as "form" | "json")}>
            <TabsList>
              <TabsTrigger value="form">Form</TabsTrigger>
              <TabsTrigger value="json">JSON</TabsTrigger>
            </TabsList>
          </Tabs>
          <Button size="sm" disabled={!dirty || save.isPending} onClick={submit}>
            {save.isPending ? "Saving…" : dirty ? "Save" : "Saved"}
          </Button>
          <AlertDialog>
            <AlertDialogTrigger
              render={
                <Button variant="ghost" size="icon-sm" className="text-destructive" />
              }
            >
              <Trash2 />
            </AlertDialogTrigger>
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>Delete profile {profileId}?</AlertDialogTitle>
                <AlertDialogDescription>
                  Routing rules referencing this profile id fall back to the default
                  configuration.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>Cancel</AlertDialogCancel>
                <AlertDialogAction
                  className="bg-destructive text-white hover:bg-destructive/90"
                  onClick={() => remove.mutate()}
                >
                  Delete
                </AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
        </div>
      </header>
      {error && <p className="border-b px-4 py-2 text-sm text-destructive">{error}</p>}
      <div className="min-h-0 flex-1 overflow-y-auto">
        {tab === "json" ? (
          <textarea
            className="h-full w-full resize-none bg-transparent p-4 font-mono text-xs outline-none"
            value={jsonText}
            onChange={(e) => setJsonText(e.target.value)}
            spellCheck={false}
          />
        ) : (
          <div className="mx-auto grid w-full max-w-5xl gap-8 px-4 py-6 md:px-8">
            <GeneralSection draft={draft} mutate={mutate} />
            <InstructionsSection draft={draft} mutate={mutate} />
            <ConfigSection
              universeId={universeId}
              draft={draft}
              mutate={mutate}
              onValidityChange={setConfigError}
            />
            <InitialEnvironmentSection universeId={universeId} draft={draft} mutate={mutate} />
          </div>
        )}
      </div>
    </>
  );
}

function Section({
  title,
  description,
  actions,
  children,
}: {
  title: string;
  description?: string;
  actions?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="grid gap-3">
      <div className="flex items-end justify-between gap-3">
        <div className="grid gap-0.5">
          <h2 className="text-sm font-semibold">{title}</h2>
          {description && <p className="text-xs text-muted-foreground">{description}</p>}
        </div>
        {actions}
      </div>
      {children}
    </section>
  );
}

type Mutate = (fn: (d: ProfileDocument) => void) => void;

function GeneralSection({ draft, mutate }: { draft: ProfileDocument; mutate: Mutate }) {
  return (
    <Section title="General">
      <div className="grid gap-4 sm:grid-cols-2">
        <Field>
          <FieldLabel htmlFor="profile-display-name">Display name</FieldLabel>
          <Input
            id="profile-display-name"
            value={(draft.displayName as string) ?? ""}
            onChange={(e) =>
              mutate((d) => {
                if (e.target.value) {
                  d.displayName = e.target.value;
                } else {
                  delete d.displayName;
                }
              })
            }
          />
        </Field>
        <Field>
          <FieldLabel htmlFor="profile-description">Description</FieldLabel>
          <Input
            id="profile-description"
            value={(draft.description as string) ?? ""}
            onChange={(e) =>
              mutate((d) => {
                if (e.target.value) {
                  d.description = e.target.value;
                } else {
                  delete d.description;
                }
              })
            }
          />
        </Field>
      </div>
    </Section>
  );
}

function InstructionsSection({
  draft,
  mutate,
}: {
  draft: ProfileDocument;
  mutate: Mutate;
}) {
  const instructions = draft.instructions as
    | { type: "text"; text: string }
    | { type: "textRef"; blobRef: string }
    | undefined;

  if (instructions?.type === "textRef") {
    return (
      <Section title="Instructions">
        <p className="text-sm text-muted-foreground">
          Stored as a blob reference (<code className="font-mono text-xs">{instructions.blobRef}</code>)
          — edit via the JSON tab.
        </p>
      </Section>
    );
  }

  return (
    <Section title="Instructions" description="System prompt applied to sessions.">
      <textarea
        className="min-h-32 w-full resize-y rounded-lg border border-input bg-transparent p-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 dark:bg-input/30"
        value={instructions?.text ?? ""}
        onChange={(e) =>
          mutate((d) => {
            if (e.target.value) {
              d.instructions = { type: "text", text: e.target.value };
            } else {
              delete d.instructions;
            }
          })
        }
        spellCheck={false}
      />
    </Section>
  );
}

function ConfigSection({
  universeId,
  draft,
  mutate,
  onValidityChange,
}: {
  universeId: string;
  draft: ProfileDocument;
  mutate: Mutate;
  onValidityChange: (message: string | null) => void;
}) {
  const options = useSessionConfigEditorOptions(universeId);
  return (
    <Section
      title="Session config"
      description="Sparse session behavior and capability grants. Unset values inherit engine defaults."
    >
      <SessionConfigEditor
        value={draft.config}
        mcpServers={options.mcpServers}
        workspaces={options.workspaces}
        workspacesLoading={options.workspacesLoading}
        models={options.models}
        profiles={options.profiles}
        environmentProviders={options.environmentProviders}
        featureDisableReasons={resourceFeatureDisableReasons(draft)}
        onValidityChange={onValidityChange}
        onChange={(config) =>
          mutate((document) => {
            if (config) document.config = config;
            else delete document.config;
          })
        }
      />
    </Section>
  );
}

function InitialEnvironmentSection({
  universeId,
  draft,
  mutate,
}: {
  universeId: string;
  draft: ProfileDocument;
  mutate: Mutate;
}) {
  const environments = useQuery({
    queryKey: ["environments", universeId],
    queryFn: () =>
      api<Environment[]>("GET", `/api/v1/universes/${universeId}/environments`),
  });
  return (
    <ProfileEnvironmentEditor
      value={draft.activeEnvironmentId}
      environments={environments.data}
      disabled={!hasSessionFeature(draft.config, "environments")}
      title="Active environment"
      description="The universe environment to activate when this profile is applied. Absence leaves an existing session's selection unchanged."
      emptyLabel="Do not change the active environment"
      onChange={(environmentId) =>
        mutate((document) => {
          if (environmentId) document.activeEnvironmentId = environmentId;
          else delete document.activeEnvironmentId;
        })
      }
    />
  );
}
function NewProfileDialog({
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
  const [displayName, setDisplayName] = useState("");
  const [profileId, setProfileId] = useState("");
  const [idTouched, setIdTouched] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const create = useMutation({
    mutationFn: () =>
      api("PUT", `/api/v1/universes/${universeId}/profiles/${profileId}`, {
        profileId,
        ...(displayName ? { displayName } : {}),
      }),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["profiles", universeId] });
      onOpenChange(false);
      const target = profileId;
      setDisplayName("");
      setProfileId("");
      setIdTouched(false);
      setError(null);
      navigate(`/u/${slug}/profiles/${target}`);
    },
    onError: (err) => setError(err.message),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!profileId) {
      setError("a name or id is required");
      return;
    }
    create.mutate();
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>New profile</DialogTitle>
          <DialogDescription>
            Routing rules apply it to sessions via the profile id.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={submit} className="grid gap-4">
          <Field>
            <FieldLabel htmlFor="new-profile-name">Display name</FieldLabel>
            <Input
              id="new-profile-name"
              value={displayName}
              onChange={(e) => {
                setDisplayName(e.target.value);
                if (!idTouched) {
                  setProfileId(e.target.value ? slugify(e.target.value) : "");
                }
              }}
              placeholder="Owner"
              autoFocus
            />
          </Field>
          <Field>
            <FieldLabel htmlFor="new-profile-id">Profile id</FieldLabel>
            <Input
              id="new-profile-id"
              value={profileId}
              onChange={(e) => {
                setProfileId(e.target.value);
                setIdTouched(e.target.value.length > 0);
              }}
              placeholder="owner"
              className="font-mono"
            />
            <FieldDescription>
              What routing rules and tooling reference — cannot be changed later.
            </FieldDescription>
          </Field>
          {error && <p className="text-sm text-destructive">{error}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={create.isPending}>
              {create.isPending ? "Creating…" : "Create"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
