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
import { canManage, useActiveUniverse } from "@/lib/universes";

export function GeneralSettingsPage({ admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();

  if (isLoading) {
    return <LoadingNote />;
  }
  if (!universe || !canManage(universe, admin)) {
    return <UniverseNotFound slug={slug} />;
  }

  return (
    <>
      <PageHeader title="General" description="Universe name, identifiers, and lifecycle." />
      <div className="grid gap-6">
        <RenameCard universe={universe} />
        <IdentifiersCard universe={universe} />
        <DangerZone universe={universe} />
      </div>
    </>
  );
}

function RenameCard({ universe }: { universe: Universe }) {
  const queryClient = useQueryClient();
  const [name, setName] = useState(universe.name);

  const rename = useMutation({
    mutationFn: () =>
      api<Universe>("PATCH", `/api/v1/universes/${universe.id}`, { name: name.trim() }),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ["universes"] }),
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
          The display name shown in the switcher. The URL slug never changes —
          links stay stable.
        </CardDescription>
      </CardHeader>
      <CardContent>
        <form onSubmit={submit} className="flex max-w-md items-end gap-3">
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
            disabled={rename.isPending || name.trim() === universe.name || !name.trim()}
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
          <span className="text-muted-foreground">URL slug (immutable)</span>
          <code className="w-fit rounded-md bg-muted px-2 py-0.5 font-mono text-xs">
            {universe.slug}
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
          Archiving detaches every chat immediately — Channels stops routing to
          this universe. Data is kept and the universe can be restored.
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
                  All routing rules stop matching and paired chats go silent until the
                  universe is restored.
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
