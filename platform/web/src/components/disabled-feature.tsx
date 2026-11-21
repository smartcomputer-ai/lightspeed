import { Link } from "react-router-dom";
import { FeatureOffIcon } from "@/icons";
import { FEATURES, type FeatureKey } from "@lightspeed-ai/platform-shared";
import { EmptyState } from "@/components/page";

/** A direct link to an installed module whose universe capability is off. */
export function DisabledFeature({ feature, handle, mayConfigure }: {
  feature: FeatureKey;
  handle: string;
  mayConfigure?: boolean;
}) {
  const { label } = FEATURES[feature];
  return (
    <div className="p-6">
      <EmptyState icon={FeatureOffIcon} title={`${label} are turned off in this universe`}>
        {mayConfigure ? (
          <>
            <Link to={`/u/${handle}/settings/general`} className="font-medium text-foreground underline">
              Turn them on in universe settings
            </Link>
            .
          </>
        ) : (
          "A universe Owner can turn them on."
        )}
      </EmptyState>
    </div>
  );
}
