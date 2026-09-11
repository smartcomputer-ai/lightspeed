import { Link } from "react-router-dom";
import { KeyRound } from "lucide-react";
import { Button } from "@/components/ui/button";
import { addIntegrationHref, useProviderReadiness } from "@/lib/provider-readiness";

/// Nudges toward Integrations when no model provider has a usable credential.
/// Renders nothing while loading or when at least one provider is ready.
export function ProviderReadinessBanner({
  universeId,
  slug,
  className,
}: {
  universeId: string;
  slug: string;
  className?: string;
}) {
  const readiness = useProviderReadiness(universeId);
  if (readiness.isLoading || readiness.ready) return null;
  const invalidOnly = readiness.missing.length === 0 && readiness.invalid.length > 0;
  return (
    <div
      role="status"
      className={`flex flex-wrap items-center gap-3 border-b bg-amber-500/10 px-4 py-2 text-sm ${className ?? ""}`}
    >
      <KeyRound className="size-4 shrink-0 text-amber-600 dark:text-amber-400" />
      <span className="flex-1">
        {invalidOnly
          ? "The configured model provider key was rejected. Sessions cannot run until a valid model provider API key is set."
          : "No model provider is configured for this universe. Sessions cannot run until a model provider API key is added."}
      </span>
      <Button size="sm" nativeButton={false} render={<Link to={addIntegrationHref(slug, "openAiApiKey")} />}>
        Add API key
      </Button>
    </div>
  );
}
