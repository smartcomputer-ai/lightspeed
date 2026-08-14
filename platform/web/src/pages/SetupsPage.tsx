import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { CheckCircle2, PackageOpen, RefreshCw, Sparkles } from "lucide-react";
import { api, type UniverseSetup } from "@/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card";
import { LoadingNote, PageHeader, UniverseNotFound } from "@/components/page";
import { canManage, useActiveUniverse } from "@/lib/universes";

export function SetupsPage({ admin }: { admin: boolean }) {
  const { universe, slug, isLoading } = useActiveUniverse();

  if (isLoading) {
    return <LoadingNote />;
  }
  if (!universe || !canManage(universe, admin)) {
    return <UniverseNotFound slug={slug} />;
  }

  return <SetupList universeId={universe.id} />;
}

function SetupList({ universeId }: { universeId: string }) {
  const queryClient = useQueryClient();
  const setups = useQuery({
    queryKey: ["setups", universeId],
    queryFn: () => api<UniverseSetup[]>("GET", `/api/v1/universes/${universeId}/setups`),
    refetchInterval: (query) =>
      query.state.data?.some((setup) => setup.status === "installing") ? 2_000 : false,
  });
  const install = useMutation({
    mutationFn: (setupId: string) =>
      api<UniverseSetup>(
        "POST",
        `/api/v1/universes/${universeId}/setups/${setupId}/install`,
      ),
    onSuccess: () =>
      queryClient.invalidateQueries({ queryKey: ["setups", universeId] }),
  });

  return (
    <>
      <PageHeader
        title="Setups"
        description="Install ready-to-use profiles and resources maintained by Lightspeed."
      />
      {setups.isLoading && <LoadingNote />}
      {setups.error && <p className="text-sm text-destructive">{setups.error.message}</p>}
      {install.error && <p className="mb-4 text-sm text-destructive">{install.error.message}</p>}
      <div className="grid gap-4 md:grid-cols-2">
        {(setups.data ?? []).map((setup) => (
          <SetupCard
            key={setup.id}
            setup={setup}
            pending={install.isPending && install.variables === setup.id}
            onInstall={() => install.mutate(setup.id)}
          />
        ))}
      </div>
    </>
  );
}

function SetupCard({
  setup,
  pending,
  onInstall,
}: {
  setup: UniverseSetup;
  pending: boolean;
  onInstall: () => void;
}) {
  const ready = setup.status === "ready";
  const installing = setup.status === "installing" || pending;
  const unavailable = setup.status === "unavailable";
  const action = ready ? "Repair" : setup.status === "failed" ? "Retry" : "Install";

  return (
    <Card className="flex flex-col">
      <CardHeader>
        <div className="mb-2 flex items-start justify-between gap-3">
          <div className="flex size-10 items-center justify-center rounded-lg bg-primary/10 text-primary">
            <PackageOpen className="size-5" />
          </div>
          <SetupStatus setup={setup} />
        </div>
        <CardTitle>{setup.name}</CardTitle>
        <CardDescription>{setup.description}</CardDescription>
      </CardHeader>
      <CardContent className="flex-1">
        {ready && setup.resources?.profileId && (
          <div className="rounded-lg bg-muted p-3 text-sm">
            <div className="flex items-center gap-2 font-medium">
              <CheckCircle2 className="size-4 text-primary" />
              Ready to use
            </div>
            <p className="mt-1 text-muted-foreground">
              Profile: <code className="font-mono text-xs">{setup.resources.profileId}</code>
            </p>
          </div>
        )}
        {setup.error && (
          <p className="rounded-lg bg-destructive/10 p-3 text-sm text-destructive">
            {setup.error}
          </p>
        )}
        {unavailable && (
          <p className="text-sm text-muted-foreground">
            The deployment has not configured a Configurator MCP URL.
          </p>
        )}
      </CardContent>
      <CardFooter className="justify-between gap-3">
        <span className="text-xs text-muted-foreground">Setup version {setup.version}</span>
        <Button onClick={onInstall} disabled={installing || unavailable} variant={ready ? "outline" : "default"}>
          {installing ? (
            <RefreshCw className="animate-spin" data-icon="inline-start" />
          ) : ready ? (
            <RefreshCw data-icon="inline-start" />
          ) : (
            <Sparkles data-icon="inline-start" />
          )}
          {installing ? "Installing…" : action}
        </Button>
      </CardFooter>
    </Card>
  );
}

function SetupStatus({ setup }: { setup: UniverseSetup }) {
  switch (setup.status) {
    case "ready":
      return <Badge variant="secondary">installed</Badge>;
    case "installing":
      return <Badge variant="outline">installing</Badge>;
    case "failed":
      return <Badge variant="destructive">failed</Badge>;
    case "unavailable":
      return <Badge variant="outline">unavailable</Badge>;
    default:
      return <Badge variant="outline">available</Badge>;
  }
}
