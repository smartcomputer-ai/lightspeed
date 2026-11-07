import { useEffect, useMemo, useState, type FormEvent, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useParams } from "react-router-dom";
import { slugify } from "@lightspeed-ai/platform-shared";
import { AddIcon, DeleteIcon, ProfileIcon } from "@/icons";
import {
  api,
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
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";
import {
  MetadataMapEditor,
} from "@/components/session/metadata-editor";
import { ProfileRetentionEditor } from "@/components/session/profile-retention-editor";
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
import { SettingsDisclosure } from "@/components/ui/settings-disclosure";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { DetailPrompt, LoadingNote, UniverseNotFound } from "@/components/page";
import { useCreateParam } from "@/lib/create-param";
import { useSessionConfigEditorOptions } from "@/lib/sessions/editor-options";
import {
  resourceFeatureDisableReasons,
  setupResourceFeatureError,
} from "@/lib/sessions/resource-features";
import { useActiveUniverse } from "@/lib/universes";
import { cn } from "@/lib/utils";
import { useActionPermissions } from "@/lib/permissions";
import { DetailHeader } from "@/components/detail-header";
import { ListPane, ListPaneBody, ListPaneHeader, ListPaneState, ListRow, ListRowLine } from "@/components/list-pane";

/// Structured profile editor. Pane = profile list; detail = sectioned form
/// with a reusable SessionConfig editor and a permanent Edit-as-JSON tab.
/// Saves keep the profiles/put +
/// expectedRevision flow — a stale editor gets a 409, not a clobber.
export function ProfilesPage(_props: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();
  const { profileId } = useParams<{ profileId: string }>();
  const canCreate = useActionPermissions(universe?.id).can("createProfile");
  const [, setCreateOpen] = useCreateParam("profile");

  if (isLoading) {
    return <LoadingNote />;
  }
  if (!universe || !universe.role) {
    return (
      <div className="p-6">
        <UniverseNotFound slug={slug} />
      </div>
    );
  }

  return (
    <div className="flex min-h-0 min-w-0 flex-1">
      <ListPane detailOpen={Boolean(profileId)}>
        <ProfilePane universeId={universe.id} slug={slug!} activeId={profileId} />
      </ListPane>
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
          <DetailPrompt
            icon={<ProfileIcon className="size-10 text-muted-foreground/60" />}
            create={canCreate ? { label: "New profile", onClick: () => setCreateOpen(true) } : undefined}
          >
            Pick a profile{canCreate ? ", or create one" : ""}.
          </DetailPrompt>
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
  const permissions = useActionPermissions(universeId);
  const canCreate = permissions.can("createProfile");
  const [createOpen, setCreateOpen] = useCreateParam("profile");

  return (
    <>
      <ListPaneHeader
        title="Profiles"
        count={profiles.data?.length}
        actions={canCreate && (
          <Button variant="ghost" size="icon-sm" onClick={() => setCreateOpen(true)} aria-label="New profile">
            <AddIcon />
          </Button>
        )}
      />
      <ListPaneBody>
        <ListPaneState
          loading={profiles.isLoading}
          error={profiles.error}
          hasData={Boolean(profiles.data)}
          empty={profiles.data?.length === 0 && "No profiles yet."}
        />
        <ul>
          {profiles.data?.map((profile) => (
            <ListRow
              key={profile.profileId}
              to={`/u/${slug}/profiles/${profile.profileId}`}
              active={profile.profileId === activeId}
              title={profile.displayName ?? profile.profileId}
              badges={!permissions.can("manageProfile") && (
                <span className="shrink-0 text-xs text-muted-foreground">Read only</span>
              )}
              meta={`r${profile.revision}`}
            >
              <ListRowLine>{profile.description ?? profile.profileId}</ListRowLine>
            </ListRow>
          ))}
        </ul>
      </ListPaneBody>
      {canCreate && (
        <NewProfileDialog
          universeId={universeId}
          slug={slug}
          profiles={profiles.data ?? []}
          open={createOpen}
          onOpenChange={setCreateOpen}
        />
      )}
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
  const permissions = useActionPermissions(universeId);
  const manage = permissions.can("manageProfile");
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
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [jsonText, setJsonText] = useState("");
  const [configError, setConfigError] = useState<string | null>(null);
  const [retentionError, setRetentionError] = useState<string | null>(null);
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
      setRetentionError(null);
      setError(null);
    }
  }, [doc.data]);

  const mutate = (fn: (d: ProfileDocument) => void) => {
    if (!manage) return;
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
    mutationFn: async (document: ProfileDocument) => {
      if (!manage) throw new Error("You do not have permission to edit this profile.");
      // The core reconciles bots applying this profile onto the new
      // revision on its own.
      await api("PUT", `/api/v1/universes/${universeId}/profiles/${profileId}`, document);
    },
    onSuccess: () => {
      setError(null);
      void queryClient.invalidateQueries({ queryKey: ["profiles", universeId] });
      void queryClient.invalidateQueries({ queryKey: ["profile", universeId, profileId] });
    },
    onError: (err) => setError(err.message),
  });

  const remove = useMutation({
    mutationFn: () => {
      if (!manage) throw new Error("You do not have permission to delete this profile.");
      return api("DELETE", `/api/v1/universes/${universeId}/profiles/${profileId}`);
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["profiles", universeId] });
      navigate(`/u/${slug}/profiles`);
    },
    onError: (err) => setError(err.message),
  });

  const submit = () => {
    if (!manage) return;
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
    } else if (configError || retentionError) {
      setError(configError ? `Config: ${configError}` : `Retention: ${retentionError}`);
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
      setRetentionError(null);
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
      <DetailHeader
        back={{ to: `/u/${slug}/profiles`, label: "Back to profiles" }}
        title={(draft.displayName as string) || profileId}
        details={<>
          <code className="max-w-48 truncate rounded bg-muted px-1.5 py-0.5 font-mono">{profileId}</code>
          <span>r{(draft.revision as number) ?? "?"}</span>
        </>}
        actions={[
          {
            kind: "choice",
            key: "view",
            label: "Edit as",
            value: tab,
            options: [{ value: "form", label: "Form" }, { value: "json", label: "JSON" }],
            onChange: (value) => switchTab(value as "form" | "json"),
          },
          ...(manage
            ? [
                {
                  key: "save",
                  label: "Save",
                  text: save.isPending ? "Saving…" : dirty ? "Save" : "Saved",
                  primary: true,
                  disabled: !dirty || save.isPending,
                  onSelect: submit,
                },
                {
                  key: "delete",
                  label: "Delete profile",
                  icon: <DeleteIcon />,
                  destructive: true,
                  onSelect: () => setDeleteOpen(true),
                },
              ]
            : []),
        ]}
      />
      <AlertDialog open={deleteOpen && manage} onOpenChange={setDeleteOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete profile {profileId}?</AlertDialogTitle>
            <AlertDialogDescription>
              Bots referencing this profile id fall back to the default
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
      {!manage && (
        <p className="border-b px-4 py-2 text-xs text-muted-foreground">
          Read only — you can view this profile, but cannot change it.
        </p>
      )}
      {error && <p className="border-b px-4 py-2 text-sm text-destructive">{error}</p>}
      <div className="min-h-0 flex-1 overflow-y-auto">
        {tab === "json" ? (
          <textarea
            className="h-full w-full resize-none bg-transparent p-4 font-mono text-xs outline-none"
            readOnly={!manage}
            aria-label="Profile JSON"
            value={jsonText}
            onChange={(e) => setJsonText(e.target.value)}
            spellCheck={false}
          />
        ) : (
          <div className="mx-auto grid w-full max-w-5xl gap-8 px-4 py-6 md:px-8">
            <GeneralSection draft={draft} mutate={mutate} readOnly={!manage} />
            <InstructionsSection draft={draft} mutate={mutate} readOnly={!manage} />
            <ConfigSection
              universeId={universeId}
              draft={draft}
              mutate={mutate}
              readOnly={!manage}
              onValidityChange={setConfigError}
              onRetentionValidityChange={setRetentionError}
            />
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

function GeneralSection({ draft, mutate, readOnly }: { draft: ProfileDocument; mutate: Mutate; readOnly: boolean }) {
  return (
    <Section title="General">
      <div className="grid gap-4 sm:grid-cols-2">
        <Field>
          <FieldLabel htmlFor="profile-display-name">Display name</FieldLabel>
          <Input
            id="profile-display-name"
            readOnly={readOnly}
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
            readOnly={readOnly}
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
  readOnly,
}: {
  draft: ProfileDocument;
  mutate: Mutate;
  readOnly: boolean;
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
        readOnly={readOnly}
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
  readOnly,
  universeId,
  draft,
  mutate,
  onValidityChange,
  onRetentionValidityChange,
}: {
  readOnly: boolean;
  universeId: string;
  draft: ProfileDocument;
  mutate: Mutate;
  onValidityChange: (message: string | null) => void;
  onRetentionValidityChange: (message: string | null) => void;
}) {
  const options = useSessionConfigEditorOptions(universeId);
  return (
    <Section
      title="Model configuration"
      description="Choose a model or inherit the universe default when a session is created. Unset reasoning uses the provider default."
    >
      <SessionConfigEditor
        readOnly={readOnly}
        value={draft.config}
        allowInherit
        mcpServers={options.mcpServers}
        workspaces={options.workspaces}
        workspacesLoading={options.workspacesLoading}
        models={options.models}
        defaultModelLabel={options.defaultModelLabel}
        profiles={options.profiles}
        environments={options.environments}
        mcpToolDiscovery={options.mcpToolDiscovery}
        featureDisableReasons={resourceFeatureDisableReasons(draft)}
        metadataSetup={(
          <MetadataMapEditor
            disabled={readOnly}
            value={draft.metadata}
            onChange={(metadata) =>
              mutate((document) => {
                if (metadata) document.metadata = metadata;
                else delete document.metadata;
              })
            }
          />
        )}
        metadataDescription="Defaults copied when a session is created. Start-time metadata overrides matching keys."
        retentionSetup={(
          <ProfileRetentionEditor
            disabled={readOnly}
            value={draft.retention?.deleteAfterCloseMs}
            onValidityChange={onRetentionValidityChange}
            onChange={(deleteAfterCloseMs) =>
              mutate((document) => {
                if (deleteAfterCloseMs !== undefined) {
                  document.retention = { deleteAfterCloseMs };
                } else {
                  delete document.retention;
                }
              })
            }
          />
        )}
        retentionDescription="Default automatic deletion for new root sessions created from this profile."
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
function NewProfileDialog({
  universeId,
  slug,
  profiles,
  open,
  onOpenChange,
}: {
  universeId: string;
  slug: string;
  profiles: ProfileSummary[];
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const permissions = useActionPermissions(universeId);
  const canCreate = permissions.can("createProfile");
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const [displayName, setDisplayName] = useState("");
  const [profileId, setProfileId] = useState("");
  const [idTouched, setIdTouched] = useState(false);
  const [sourceProfileId, setSourceProfileId] = useState("");
  const [error, setError] = useState<string | null>(null);

  const create = useMutation({
    mutationFn: async () => {
      if (!canCreate) throw new Error("You do not have permission to create profiles.");
      let document = newProfileDocument(profileId, displayName);
      if (sourceProfileId) {
        const source = await api<ProfileDocument>(
          "GET",
          `/api/v1/universes/${universeId}/profiles/${encodeURIComponent(sourceProfileId)}`,
        );
        document = copiedProfileDocument(source, profileId, displayName);
      }
      return api("PUT", `/api/v1/universes/${universeId}/profiles/${profileId}`, document);
    },
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["profiles", universeId] });
      onOpenChange(false);
      const target = profileId;
      setDisplayName("");
      setProfileId("");
      setIdTouched(false);
      setSourceProfileId("");
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
  const sourceName = profiles.find((profile) => profile.profileId === sourceProfileId)?.displayName
    ?? sourceProfileId;

  return (
    <Dialog open={open && canCreate} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>New profile</DialogTitle>
          <DialogDescription>
            Bots apply it to their sessions via the profile id.
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
            {/* The id follows the name until it is edited; it opens on its own when missing. */}
            <SettingsDisclosure
              summary={profileId
                ? <>Id: <code className="font-mono">{profileId}</code></>
                : "Id derived from the display name"}
              action="Change"
              label="Change profile id"
              forceOpen={Boolean(error) && !profileId}
            >
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
                  What bots and tooling reference — cannot be changed later.
                </FieldDescription>
              </Field>
            </SettingsDisclosure>
          </Field>
          <SettingsDisclosure
            summary={sourceProfileId ? `Copies ${sourceName}` : "Starts empty"}
            action="Change"
            label="Change starting point"
          >
            <Field>
              <FieldLabel htmlFor="new-profile-source">Start from</FieldLabel>
              <Select
                value={sourceProfileId}
                onValueChange={(value) => setSourceProfileId((value as string) ?? "")}
              >
                <SelectTrigger id="new-profile-source" className="w-full">
                  <SelectValue>
                    {sourceProfileId ? sourceName : "Empty profile"}
                  </SelectValue>
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="">Empty profile</SelectItem>
                  {profiles.map((profile) => (
                    <SelectItem key={profile.profileId} value={profile.profileId}>
                      {profile.displayName ?? profile.profileId}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <FieldDescription>
                {sourceProfileId
                  ? "Copies the profile’s current setup once. Later changes to it are not inherited."
                  : "Starts with no profile configuration."}
              </FieldDescription>
            </Field>
          </SettingsDisclosure>
          {error && <p className="text-sm text-destructive">{error}</p>}
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={!canCreate || create.isPending}>
              {create.isPending ? "Creating…" : "Create"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function newProfileDocument(profileId: string, displayName: string): ProfileDocument {
  return {
    profileId,
    revision: 0,
    ...(displayName ? { displayName } : {}),
  };
}

/** A copied profile is a snapshot: registry identity and timestamps never carry over. */
export function copiedProfileDocument(
  source: ProfileDocument,
  profileId: string,
  displayName: string,
): ProfileDocument {
  const {
    profileId: _sourceProfileId,
    displayName: _sourceDisplayName,
    revision: _sourceRevision,
    createdAtMs: _sourceCreatedAtMs,
    updatedAtMs: _sourceUpdatedAtMs,
    ...document
  } = structuredClone(source);
  return {
    ...document,
    profileId,
    revision: 0,
    ...(displayName ? { displayName } : {}),
  };
}
