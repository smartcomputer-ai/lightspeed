export * from "./lightspeed.js";
export * from "./control-plane.js";

import type { FoundryControlPlaneActivities } from "./control-plane.js";
import type { FoundryLightspeedActivities } from "./lightspeed.js";

export type FoundryActivities = FoundryLightspeedActivities & FoundryControlPlaneActivities;
