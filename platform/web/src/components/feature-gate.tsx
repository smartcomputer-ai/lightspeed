import type { ReactNode } from "react";
import { Link } from "react-router-dom";
import { FeatureOffIcon } from "@/icons";
import { FEATURES, type FeatureKey } from "@lightspeed-ai/platform-shared";
import { EmptyState } from "@/components/page";
import { useActionPermissions } from "@/lib/permissions";
import { useActiveUniverse, useFeature } from "@/lib/universes";

/**
 * A page that belongs to a feature: shown while it is on, and in its place a
 * note while it is off, for anyone following a link. Admins are pointed to
 * where it is switched on. The page's requests would be refused anyway; the
 * note says why instead of showing their errors.
 */
export function FeatureGate({ feature, children }: { feature: FeatureKey; children: ReactNode }) {
  const on = useFeature(feature);
  const { universe, handle, isLoading } = useActiveUniverse();
  const permissions = useActionPermissions(universe?.id);
  if (on) return children;
  // Features are off until the universe says otherwise: wait for it rather
  // than flash the note over a page that is about to appear.
  if (isLoading) return null;
  const { label } = FEATURES[feature];
  return (
    <div className="p-6">
      <EmptyState icon={FeatureOffIcon} title={`${label} are turned off in this universe`}>
        {permissions.can("manageAccess") ? (
          <>
            <Link to={`/u/${handle}/settings/general`} className="font-medium text-foreground underline">
              Turn them on in Settings
            </Link>
            .
          </>
        ) : (
          "A universe admin can turn them on."
        )}
      </EmptyState>
    </div>
  );
}
