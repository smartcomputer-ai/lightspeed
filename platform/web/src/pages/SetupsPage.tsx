import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type UniverseSetup } from "@/api";
import { useActionPermissions } from "@/lib/permissions";
import { useActiveUniverse } from "@/lib/universes";
import { LoadingNote, PageHeader, UniverseNotFound } from "@/components/page";
import { ReadError } from "@/components/read-error";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card";

export function SetupsPage({ admin }: { admin: boolean }) {
  const { universe, handle, isLoading } = useActiveUniverse();
  const permissions = useActionPermissions(universe?.id);
  if (isLoading) return <LoadingNote />;
  if (!universe || !permissions.can("configureResource")) return <UniverseNotFound handle={handle} />;
  const installable = permissions.can("manageAccess") && (admin || universe.accountRole === "admin");
  return <SetupList universeId={universe.id} installable={installable} />;
}

function SetupList({ universeId, installable }: { universeId: string; installable: boolean }) {
  const queryClient = useQueryClient();
  const setups = useQuery({
    queryKey: ["setups", universeId],
    queryFn: () => api<UniverseSetup[]>("GET", `/api/ui/universes/${universeId}/setups`),
    refetchInterval: query => query.state.data?.some(setup => setup.status === "installing") ? 2_000 : false,
  });
  const install = useMutation({
    mutationFn: (setupId: string) => api<UniverseSetup>("POST", `/api/ui/universes/${universeId}/setups/${setupId}/install`),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["setups", universeId] });
      void queryClient.invalidateQueries({ queryKey: ["mcp-servers", universeId] });
    },
  });
  return <>
    <PageHeader title="Templates" description="Install managed resources for this universe." />
    {setups.isLoading && <LoadingNote />}
    {setups.error && <ReadError error={setups.error} loading={!setups.data} />}
    {install.error && <p role="alert" className="mb-4 text-sm text-destructive">{install.error.message}</p>}
    <div className="grid gap-4 md:grid-cols-2">
      {(setups.data ?? []).map(setup => {
        const pending = setup.status === "installing" || (install.isPending && install.variables === setup.id);
        const ready = setup.status === "ready";
        return <Card key={setup.id}>
          <CardHeader>
            <CardTitle>{setup.name}</CardTitle>
            <CardDescription>{setup.description}</CardDescription>
          </CardHeader>
          <CardContent className="space-y-3">
            <Badge variant={setup.status === "failed" ? "destructive" : "secondary"}>{pending ? "installing" : setup.status}</Badge>
            {setup.error && <p className="text-sm text-destructive">{setup.error}</p>}
            {ready && setup.resources?.serverId && <p className="text-sm">MCP server: <code>{setup.resources.serverId}</code></p>}
            {!setup.available && <p className="text-sm text-muted-foreground">The deployment has not configured a Configurator endpoint.</p>}
            {!installable && <p className="text-sm text-muted-foreground">An Account Admin installs and rotates this setup's application credential.</p>}
          </CardContent>
          <CardFooter>
            <Button disabled={!installable || !setup.available || pending} onClick={() => install.mutate(setup.id)}>
              {pending ? "Installing…" : ready ? "Reinstall and rotate credential" : setup.status === "failed" ? "Retry installation" : "Install"}
            </Button>
          </CardFooter>
        </Card>;
      })}
    </div>
  </>;
}
