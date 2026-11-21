import { Suspense } from "react";
import { CenteredNote, LoadingNote, UniverseNotFound } from "@/components/page";
import { ReadError } from "@/components/read-error";
import { useActiveUniverse } from "@/lib/universes";
import { useAvailableWebModules, type UniverseModuleRoute, type UniverseWebModule } from "@/lib/web-modules";

export function ModuleRoute({ module, Page }: { module: UniverseWebModule; Page: UniverseModuleRoute["Page"] }) {
  const { universe, handle, isLoading } = useActiveUniverse();
  const available = useAvailableWebModules(universe);
  if (isLoading || available.isLoading) return <LoadingNote />;
  if (!universe) return <UniverseNotFound handle={handle} />;
  if (available.error) return <ReadError error={available.error} prefix="Module availability" />;
  if (!available.modules.includes(module)) return <CenteredNote>The {module.label} module is unavailable in this universe.</CenteredNote>;
  return <Suspense fallback={<LoadingNote />}><Page key={universe.id} /></Suspense>;
}
