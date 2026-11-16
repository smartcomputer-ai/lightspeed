import { useActionPermissions } from "@/lib/permissions";
import { LoadingNote, PageHeader, UniverseNotFound } from "@/components/page";
import { useActiveUniverse } from "@/lib/universes";

export function SetupsPage({ admin: _admin }: { admin: boolean }) {
  const { universe, handle, isLoading } = useActiveUniverse();
  const permissions = useActionPermissions(universe?.id);
  if (isLoading) return <LoadingNote />;
  if (!universe || !permissions.can("configureResource")) return <UniverseNotFound handle={handle} />;
  return <>
    <PageHeader title="Setups" />
    <p className="text-muted-foreground">Configurator setup is temporarily unavailable.</p>
  </>;
}
