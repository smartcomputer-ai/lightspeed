import { useActionPermissions } from "@/lib/permissions";
import { Link } from "react-router-dom";
import { Info } from "lucide-react";
import type { ModelConfig } from "@/api";
import { Button } from "@/components/ui/button";
import { useProviderReadiness } from "@/lib/provider-readiness";

export function ProviderReadinessBanner({ universeId, slug, model, enabled = true, className }: {
  universeId: string;
  slug: string;
  /// Omitted means universe policy; an existing session supplies its stored model.
  model?: ModelConfig | null;
  enabled?: boolean;
  className?: string;
}) {
  const readiness = useProviderReadiness(universeId, model, enabled);
  const permissions = useActionPermissions(universeId);
  if (!enabled || readiness.isLoading || readiness.state === "configured") return null;
  return (
    <div role="status" className={`flex flex-wrap items-center gap-3 border-b bg-amber-500/10 px-4 py-2 text-sm ${className ?? ""}`}>
      <Info className="size-4 shrink-0 text-amber-600 dark:text-amber-400" />
      <span className="flex-1">{readiness.message}</span>
      {permissions.can("configure_resource") && <Button size="sm" nativeButton={false} render={<Link to={`/u/${slug}/models`} />}>Model settings</Button>}
    </div>
  );
}
