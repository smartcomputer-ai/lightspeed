import { useActionPermissions } from "@/lib/permissions";
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
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  LoadingNote,
  PageHeader,
  UniverseNotFound,
} from "@/components/page";
import { Switch } from "@/components/ui/switch";
import { universeHandleSchema, UNIVERSE_HANDLE_MAX_LENGTH, FEATURES, FEATURE_KEYS, type FeatureKey } from "@lightspeed-ai/platform-shared";
import { useActiveUniverse } from "@/lib/universes";
import { UniverseAppearanceCard } from "@/components/universe-appearance-card";

export function GeneralSettingsPage({ admin: _admin }: { admin: boolean }) {
  const { universe, handle, isLoading } = useActiveUniverse();
  const permissions = useActionPermissions(universe?.id);

  if (isLoading) {
    return <LoadingNote />;
  }
  if (!universe || !permissions.can("manageAccess")) {
    return <UniverseNotFound handle={handle} />;
  }

  return (
    <>
      <PageHeader title="General" description="Universe name, features, appearance, identifiers, and lifecycle." />
      <div className="grid gap-6">
        <RenameCard universe={universe} />
        <HandleCard key={`${universe.id}/${universe.handle}`} universe={universe} />
        <FeaturesCard universe={universe} />
        <UniverseAppearanceCard key={`${universe.id}/${universe.icon}/${universe.iconColor}`} universe={universe} />
        <IdentifiersCard universe={universe} />
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
      api<Universe>("PATCH", `/api/v1/universes/${universe.id}`, { features }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["universes"] }),
  });
  return (
    <Card>
      <CardHeader>
        <CardTitle>Features</CardTitle>
        <CardDescription>
          A feature that is off is hidden and unavailable to everyone in this universe. Turning one
          off does not stop what already runs: a bot keeps answering on its channels until it is
          paused or closed.
        </CardDescription>
      </CardHeader>
      <CardContent className="grid gap-4">
        {FEATURE_KEYS.map((key) => {
          const feature = FEATURES[key];
          const missing = (feature.requires as readonly FeatureKey[]).filter((required) => !universe.features[required]);
          const id = `feature-${key}`;
          return (
            <div key={key} className="flex items-start justify-between gap-4">
              <div className="grid gap-0.5">
                <label htmlFor={id} className="text-sm font-medium">{feature.label}</label>
                <p className="text-sm text-muted-foreground">
                  {feature.description}
                  {missing.length > 0 && ` Needs ${missing.map((required) => FEATURES[required].label).join(" and ")}.`}
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
        {change.error && <p className="text-sm text-destructive">{change.error.message}</p>}
      </CardContent>
    </Card>
  );
}

export function HandleCard({ universe }: { universe: Universe }) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const [handle, setHandle] = useState(universe.handle);
  const rename = useMutation({
    mutationFn: () => api<Universe>("PUT", `/api/v1/universes/${universe.id}/handle`, { handle: handle.trim() }),
    onSuccess: (updated) => {
      queryClient.setQueryData<Universe[]>(["universes"], rows => rows?.map(row => row.id === updated.id ? updated : row));
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
          The universe's unique name in this deployment, used in its URL and by API clients that address it by handle.
          Lowercase letters, digits and hyphens. After a rename, links and clients using the previous handle stop reaching it.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <form className="flex max-w-md items-end gap-3" onSubmit={event => { event.preventDefault(); if (valid) rename.mutate(); }}>
          <Field className="flex-1">
            <FieldLabel htmlFor="universe-handle">Handle</FieldLabel>
            <Input id="universe-handle" value={handle} onChange={event => setHandle(event.target.value)} required maxLength={UNIVERSE_HANDLE_MAX_LENGTH} />
          </Field>
          <Button type="submit" disabled={rename.isPending || !valid || handle.trim() === universe.handle}>
            {rename.isPending ? "Saving…" : "Save handle"}
          </Button>
        </form>
        {rename.error && <p role="alert" className="mt-2 text-sm text-destructive">{rename.error.message}</p>}
      </CardContent>
    </Card>
  );
}

function RenameCard({ universe }: { universe: Universe }) {
  const queryClient = useQueryClient();
  const [name, setName] = useState(universe.name);
  const managed = universe.runtimeConfiguration?.displayName !== undefined;

  const rename = useMutation({
    mutationFn: () =>
      api<Universe>("PATCH", `/api/v1/universes/${universe.id}`, { name: name.trim() }),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ["universes"] }),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!managed) rename.mutate();
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle>Name</CardTitle>
        <CardDescription>
          {managed ? "Managed by the universe configuration. Update displayName in its document to rename it." : "The display name shown in the switcher. Changing it does not change the handle."}
        </CardDescription>
      </CardHeader>
      <CardContent>
        <form onSubmit={submit} className="flex max-w-md items-end gap-3">
          <Field className="flex-1">
            <FieldLabel htmlFor="universe-name">Display name</FieldLabel>
            <Input
              id="universe-name"
              disabled={managed}
              value={name}
              onChange={(e) => setName(e.target.value)}
              required
            />
          </Field>
          <Button
            type="submit"
            disabled={managed || rename.isPending || name.trim() === universe.name || !name.trim()}
          >
            {rename.isPending ? "Saving…" : "Save"}
          </Button>
        </form>
        {rename.error && (
          <p className="mt-2 text-sm text-destructive">{rename.error.message}</p>
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
          <code className="w-fit rounded-md bg-muted px-2 py-0.5 font-mono text-xs">
            {universe.handle}
          </code>
        </div>
        <div className="grid gap-1">
          <span className="text-muted-foreground">Lightspeed universe</span>
          <code className="w-fit rounded-md bg-muted px-2 py-0.5 font-mono text-xs">
            {universe.lightspeedUniverseId}
          </code>
        </div>
      </CardContent>
    </Card>
  );
}

function DangerZone({ universe }: { universe: Universe }) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const archived = universe.status === "archived";

  const setStatus = useMutation({
    mutationFn: (status: "active" | "archived") =>
      api<Universe>("PATCH", `/api/v1/universes/${universe.id}`, { status }),
    onSuccess: async (_, status) => {
      await queryClient.invalidateQueries({ queryKey: ["universes"] });
      if (status === "archived") {
        navigate("/");
      }
    },
  });

  return (
    <Card className="border-destructive/40">
      <CardHeader>
        <CardTitle className="text-destructive">Danger zone</CardTitle>
        <CardDescription>
          Archiving hides this universe from the switcher and keeps its data.
          Bots, chats, and API access remain active until you stop them separately.
        </CardDescription>
      </CardHeader>
      <CardFooter>
        {archived ? (
          <Button variant="outline" onClick={() => setStatus.mutate("active")}>
            Restore universe
          </Button>
        ) : (
          <AlertDialog>
            <AlertDialogTrigger render={<Button variant="destructive" />}>
              Archive universe
            </AlertDialogTrigger>
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>Archive {universe.name}?</AlertDialogTitle>
                <AlertDialogDescription>
                  The universe will be hidden from the switcher. This does not
                  stop bots or chats, revoke API keys, or block existing access.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>Cancel</AlertDialogCancel>
                <AlertDialogAction
                  className="bg-destructive text-white hover:bg-destructive/90"
                  onClick={() => setStatus.mutate("archived")}
                >
                  Archive
                </AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
        )}
      </CardFooter>
    </Card>
  );
}
