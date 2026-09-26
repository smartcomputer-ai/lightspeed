import type { ReactNode } from "react";
import { Link } from "react-router-dom";
import { PowerOff } from "lucide-react";
import { FEATURES, type FeatureKey } from "@lightspeed/platform-shared";
import { EmptyState } from "@/components/page";
import { useActionPermissions } from "@/lib/permissions";
import { useActiveUniverse, useFeature } from "@/lib/universes";

/**
 * A page that belongs to a feature: shown while it is on, and in its place a
 * note while it is off, for anyone following an old link. Admins are pointed
 * to where it is switched back on. This hides; it does not enforce.
 */
export function FeatureGate({ feature, children }: { feature: FeatureKey; children: ReactNode }) {
  const on = useFeature(feature);
  const { universe, slug } = useActiveUniverse();
  const permissions = useActionPermissions(universe?.id);
  if (on) return children;
  const { label } = FEATURES[feature];
  return (
    <div className="p-6">
      <EmptyState icon={PowerOff} title={`${label} are turned off in this universe`}>
        {permissions.can("manage_access") ? (
          <>
            An admin turned them off.{" "}
            <Link to={`/u/${slug}/settings/general`} className="font-medium text-foreground underline">
              Turn them on in Settings
            </Link>
            .
          </>
        ) : (
          "A universe admin can turn them back on."
        )}
      </EmptyState>
    </div>
  );
}
