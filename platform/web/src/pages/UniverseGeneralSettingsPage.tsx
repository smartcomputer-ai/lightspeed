import { platformApi } from "@/lib/platform-api";
import { useActionPermissions, useDeploymentPermissions } from "@/lib/permissions";
import { useState, type FormEvent } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router-dom";
import { api, type Universe } from "@/api";
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
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { LoadingNote, PageHeader, UniverseNotFound } from "@/components/page";
import { Switch } from "@/components/ui/switch";
import {
  universeHandleSchema,
  UNIVERSE_HANDLE_MAX_LENGTH,
  FEATURES,
  FEATURE_KEYS,
  type FeatureKey,
} from "@lightspeed-ai/platform-shared";
import { useActiveUniverse } from "@/lib/universes";
import { DeleteUniverseDialog } from "@/components/admin/delete-universe-dialog";
import { UniverseAppearanceCard } from "@/components/universe-appearance-card";
import { DeletedSessionsDialog } from "@/components/admin/deleted-sessions";
import { organizationAdminPath } from "@/lib/admin-access";

/// A universe's own settings, each edited where it is shown, with its
/// lifecycle in the danger zone at the end.
export function UniverseGeneralSettingsPage() {
  const { universe, handle, isLoading } = useActiveUniverse();
  const permissions = useActionPermissions(universe?.id);

  if (isLoading) {
    return <LoadingNote />;
  }
  if (!universe || !permissions.can("administer")) {
    return <UniverseNotFound handle={handle} />;
  }

  return (
    <>
      <PageHeader title="General" description={`Name, handle, features and lifecycle of ${universe.name}.`} />
      <div className="grid gap-6">
        <RenameCard universe={universe} />
        <HandleCard
          key={`${universe.id}/${universe.handle}`}
          universe={universe}
        />
        <FeaturesCard universe={universe} />
        <UniverseAppearanceCard
          key={`${universe.id}/${universe.icon}/${universe.iconColor}`}
          universe={universe}
        />
        <IdentifiersCard universe={universe} />
        <ProtectionCard
          key={`${universe.id}/${universe.expiresAtMs}`}
          universe={universe}
        />
        <DangerZone universe={universe} />
      </div>
    </>
  );
}

/// What this universe offers its members. A switched-off feature is hidden
/// and its requests are refused; the runtime is not told, so what already
/// runs keeps running.
function FeaturesCard({ universe }: { universe: Universe }) {
  const queryClient = useQueryClient();
  const change = useMutation({
    mutationFn: (features: Partial<Record<FeatureKey, boolean>>) =>
      api<Universe>("PATCH", `/api/ui/universes/${universe.id}`, { features }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["universes"] }),
  });
  return (
    <Card>
      <CardHeader>
        <CardTitle>Features</CardTitle>
        <CardDescription>
          A feature that is off is hidden and unavailable to everyone in this
          universe. Turning one off does not stop what already runs: a bot keeps
          answering on its channels until it is paused or closed.
        </CardDescription>
      </CardHeader>
      <CardContent className="grid gap-4">
        {FEATURE_KEYS.map((key) => {
          const feature = FEATURES[key];
          const missing = (feature.requires as readonly FeatureKey[]).filter(
            (required) => !universe.features[required],
          );
          const id = `feature-${key}`;
          return (
            <div key={key} className="flex items-start justify-between gap-4">
              <div className="grid gap-0.5">
                <label htmlFor={id} className="text-sm font-medium">
                  {feature.label}
                </label>
                <p className="text-sm text-muted-foreground">
                  {feature.description}
                  {missing.length > 0 &&
                    ` Needs ${missing.map((required) => FEATURES[required].label).join(" and ")}.`}
                </p>
              </div>
              <Switch
                id={id}
                checked={universe.features[key]}
                disabled={missing.length > 0 || change.isPending}
                onCheckedChange={(checked) => change.mutate({ [key]: checked })}
              />
            </div>
          );
        })}
        {change.error && (
          <p className="text-sm text-destructive">{change.error.message}</p>
        )}
      </CardContent>
    </Card>
  );
}

export function HandleCard({ universe }: { universe: Universe }) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const [handle, setHandle] = useState(universe.handle);
  const rename = useMutation({
    mutationFn: () =>
      api<Universe>("PUT", `/api/ui/universes/${universe.id}/handle`, {
        handle: handle.trim(),
      }),
    onSuccess: (updated) => {
      queryClient.setQueryData<Universe[]>(["universes"], (rows) =>
        rows?.map((row) => (row.id === updated.id ? updated : row)),
      );
      setHandle(updated.handle);
      navigate(`/u/${updated.handle}/settings/general`, { replace: true });
      void queryClient.invalidateQueries({ queryKey: ["universes"] });
    },
  });
  const valid = universeHandleSchema.safeParse(handle.trim()).success;
  return (
    <Card>
      <CardHeader>
        <CardTitle>Handle</CardTitle>
        <CardDescription>
          The universe's unique name in this deployment, used in its URL and by
          API clients that address it by handle. Lowercase letters, digits and
          hyphens. After a rename, links and clients using the previous handle
          stop reaching it.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <form
          className="flex max-w-md flex-wrap items-end gap-3"
          onSubmit={(event) => {
            event.preventDefault();
            if (valid) rename.mutate();
          }}
        >
          <Field className="flex-1">
            <FieldLabel htmlFor="universe-handle">Handle</FieldLabel>
            <Input
              id="universe-handle"
              value={handle}
              onChange={(event) => setHandle(event.target.value)}
              required
              maxLength={UNIVERSE_HANDLE_MAX_LENGTH}
            />
          </Field>
          <Button
            type="submit"
            disabled={
              rename.isPending || !valid || handle.trim() === universe.handle
            }
          >
            {rename.isPending ? "Saving…" : "Save handle"}
          </Button>
        </form>
        {rename.error && (
          <p role="alert" className="mt-2 text-sm text-destructive">
            {rename.error.message}
          </p>
        )}
      </CardContent>
    </Card>
  );
}

function RenameCard({ universe }: { universe: Universe }) {
  const queryClient = useQueryClient();
  const [name, setName] = useState(universe.name);

  const rename = useMutation({
    mutationFn: () =>
      api<Universe>("PATCH", `/api/ui/universes/${universe.id}`, {
        name: name.trim(),
      }),
    onSuccess: () =>
      void queryClient.invalidateQueries({ queryKey: ["universes"] }),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    rename.mutate();
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle>Name</CardTitle>
        <CardDescription>
          The display name shown in the switcher. Changing it does not change
          the handle.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <form
          onSubmit={submit}
          className="flex max-w-md flex-wrap items-end gap-3"
        >
          <Field className="flex-1">
            <FieldLabel htmlFor="universe-name">Display name</FieldLabel>
            <Input
              id="universe-name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              required
            />
          </Field>
          <Button
            type="submit"
            disabled={
              rename.isPending || name.trim() === universe.name || !name.trim()
            }
          >
            {rename.isPending ? "Saving…" : "Save"}
          </Button>
        </form>
        {rename.error && (
          <p className="mt-2 text-sm text-destructive">
            {rename.error.message}
          </p>
        )}
        {rename.isSuccess && !rename.isPending && (
          <p className="mt-2 text-sm text-muted-foreground">Saved.</p>
        )}
      </CardContent>
    </Card>
  );
}

function IdentifiersCard({ universe }: { universe: Universe }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>Identifiers</CardTitle>
      </CardHeader>
      <CardContent className="grid gap-3 text-sm">
        <div className="grid gap-1">
          <span className="text-muted-foreground">Handle</span>
          <code className="max-w-full w-fit break-all rounded-md bg-muted px-2 py-0.5 font-mono text-xs">
            {universe.handle}
          </code>
        </div>
        <div className="grid gap-1">
          <span className="text-muted-foreground">Lightspeed universe</span>
          <code className="max-w-full w-fit break-all rounded-md bg-muted px-2 py-0.5 font-mono text-xs">
            {universe.lightspeedUniverseId}
          </code>
        </div>
      </CardContent>
    </Card>
  );
}

/// Archive, delete, and for deployment administrators the purge of deleted
/// sessions: everything that removes or hides the universe's work.
function DangerZone({ universe }: { universe: Universe }) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const archived = universe.status === "archived";
  const mayPurge = useDeploymentPermissions().can("purgeSessions");
  const [deletedSessionsOpen, setDeletedSessionsOpen] = useState(false);

  const setStatus = useMutation({
    mutationFn: (status: "active" | "archived") =>
      api<Universe>("PATCH", `/api/ui/universes/${universe.id}`, { status }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["universes"] }),
  });
  const remove = useMutation({
    mutationFn: () => api("DELETE", `/api/ui/universes/${universe.id}`, { handle: universe.handle }),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["universes"] });
      navigate(`${organizationAdminPath(universe.organizationId!)}/universes`);
    },
  });

  return (
    <Card className="border-destructive/40">
      <CardHeader>
        <CardTitle className="text-destructive">Danger zone</CardTitle>
        <CardDescription>
          Archiving hides this universe from the switcher and keeps everything; bots, chats and API access keep
          running until you stop them. Deleting removes the universe and its data for good.
        </CardDescription>
      </CardHeader>
      <CardFooter className="flex flex-wrap items-center gap-3">
        {archived ? (
          <Button variant="outline" disabled={setStatus.isPending} onClick={() => setStatus.mutate("active")}>
            Restore universe
          </Button>
        ) : (
          <AlertDialog>
            <AlertDialogTrigger render={<Button variant="outline" className="text-destructive" />}>
              Archive universe
            </AlertDialogTrigger>
            <AlertDialogContent className="max-h-[calc(100dvh-2rem)] overflow-y-auto">
              <AlertDialogHeader>
                <AlertDialogTitle>Archive {universe.name}?</AlertDialogTitle>
                <AlertDialogDescription>
                  The universe will be hidden from the switcher. This does not
                  stop bots or chats, or block existing access.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>Cancel</AlertDialogCancel>
                <AlertDialogAction
                  className="bg-destructive text-white hover:bg-destructive/90"
                  disabled={setStatus.isPending}
                  onClick={() => setStatus.mutate("archived")}
                >
                  Archive
                </AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
        )}
        <DeleteUniverseDialog name={universe.name} handle={universe.handle} protected={universe.protected}
          pending={remove.isPending} onDelete={() => remove.mutate()} />
        {mayPurge && (
          <Button variant="ghost" onClick={() => setDeletedSessionsOpen(true)}>Deleted sessions</Button>
        )}
        {[setStatus.error, remove.error].filter(Boolean).map((error, index) => (
          <p key={index} role="alert" className="basis-full text-sm text-destructive">{error?.message}</p>
        ))}
      </CardFooter>
      {mayPurge && <DeletedSessionsDialog universe={deletedSessionsOpen ? universe : null} onClose={() => setDeletedSessionsOpen(false)} />}
    </Card>
  );
}

function ProtectionCard({ universe }: { universe: Universe }) {
  const client = useQueryClient();
  const [expiry, setExpiry] = useState(
    universe.expiresAtMs ? new Date(universe.expiresAtMs).toISOString() : "",
  );
  const mayUnprotect = universe.organizationActions?.includes("administerUniverses") === true;
  const update = useMutation({
    mutationFn: (input: { protected?: boolean; expiresAtMs?: number | null }) =>
      platformApi("platform/universes/update", {
        universe: universe.lightspeedUniverseId,
        ...input,
      }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["universes"] }),
  });
  return (
    <Card>
      <CardHeader>
        <CardTitle>Deletion protection and expiry</CardTitle>
        <CardDescription>
          Protected universes cannot be deleted. Clearing protection requires an
          Organization Admin or Platform Admin and must be a separate action before
          deletion.
        </CardDescription>
      </CardHeader>
      <CardContent className="grid gap-4">
        <label className="flex items-center gap-2">
          <Switch
            aria-label="Deletion protection"
            checked={!!universe.protected}
            disabled={
              update.isPending ||
              (!universe.protected && universe.expiresAtMs != null) ||
              (!!universe.protected && !mayUnprotect)
            }
            onCheckedChange={(value) => update.mutate({ protected: value })}
          />
          Protected
        </label>
        {universe.expiresAtMs != null && (
          <p className="text-sm text-muted-foreground">
            Clear the expiry before enabling protection.
          </p>
        )}
        <form
          className="grid max-w-xl gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            const value = expiry.trim() ? Date.parse(expiry) : null;
            if (
              value !== null &&
              (!Number.isFinite(value) || value <= Date.now())
            )
              return;
            update.mutate({ expiresAtMs: value });
          }}
        >
          <Field>
            <FieldLabel>
              Automatic deletion time (ISO timestamp; blank means no expiry)
            </FieldLabel>
            <Input
              aria-label="Universe expiry"
              value={expiry}
              disabled={!!universe.protected}
              onChange={(e) => setExpiry(e.target.value)}
              placeholder="2027-01-15T12:00:00Z"
            />
          </Field>
          <Button
            type="submit"
            className="w-fit"
            variant="outline"
            disabled={
              !!universe.protected ||
              update.isPending ||
              (!!expiry.trim() &&
                (!Number.isFinite(Date.parse(expiry)) ||
                  Date.parse(expiry) <= Date.now()))
            }
          >
            Save expiry
          </Button>
        </form>
        {[update.error].filter(Boolean).map((error, i) => (
          <p role="alert" className="text-destructive" key={i}>
            {error?.message}
          </p>
        ))}
      </CardContent>
    </Card>
  );
}
