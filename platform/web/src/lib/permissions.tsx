import { createContext, useContext, useMemo, type ReactNode } from "react";
import type { ResourceAccessSummary } from "@lightspeed-ai/sdk";
import {
  canCloseSession,
  canDeleteSession,
  deploymentActions,
  universeRoleSchema,
  type DeploymentAction,
  type OrganizationAction,
  type UniverseAction,
  type UniverseRole,
} from "@lightspeed-ai/platform-shared";
import { useActiveUniverse } from "@/lib/universes";

/// Every hint here mirrors a decision the server makes again on each
/// request. Screens ask about actions; role names are only displayed.

type Identity = { userId: string; platformAdmin: boolean };
const IdentityContext = createContext<Identity | null>(null);
export function usePermissionIdentity() { return useContext(IdentityContext)?.userId ?? null; }

/** The signed-in user permission hints are computed for. */
export function PermissionIdentityProvider({ userId, platformAdmin, children }: { userId: string; platformAdmin: boolean; children: ReactNode }) {
  return <IdentityContext.Provider value={{ userId, platformAdmin }}>{children}</IdentityContext.Provider>;
}

/** The signed-in user's deployment actions. */
export function useDeploymentPermissions() {
  const platformAdmin = useContext(IdentityContext)?.platformAdmin ?? false;
  return useMemo(() => {
    const actions = deploymentActions(platformAdmin);
    return { can: (action: DeploymentAction) => actions.has(action) };
  }, [platformAdmin]);
}

/** Whether an organization or universe view the server returned grants `action`. */
export function organizationAllows(view: { actions?: readonly OrganizationAction[] } | undefined | null, action: OrganizationAction): boolean {
  return view?.actions?.includes(action) ?? false;
}

/** The signed-in user's highest role in the universe on screen, for display. */
export function useUniverseRole(universeId: string | undefined): UniverseRole | null {
  return useUniverseAccess(universeId).role;
}

/// Hints are for the universe on screen; any other universe gets none.
function useUniverseAccess(universeId: string | undefined) {
  const identity = useContext(IdentityContext);
  const active = useActiveUniverse();
  const universe = active.universe?.id === universeId ? active.universe : undefined;
  const actions = useMemo(() => new Set<UniverseAction>(universe?.actions ?? []), [universe?.actions]);
  if (!identity || !universe) return { role: null, actions: new Set<UniverseAction>(), isLoading: active.isLoading };
  return { role: universeRoleSchema.safeParse(universe.role).data ?? null, actions, isLoading: false };
}

/** What the signed-in user may do in the universe on screen. */
export function useActionPermissions(universeId: string | undefined) {
  const { role, actions, isLoading } = useUniverseAccess(universeId);
  return { role, isLoading, can: (action: UniverseAction) => actions.has(action) };
}

/** Session-aware hints shared by individual and bulk lifecycle controls. */
export function useSessionClosePermission(universeId: string | undefined) {
  const { actions } = useUniverseAccess(universeId);
  const userId = usePermissionIdentity();
  return (access: ResourceAccessSummary | undefined) => !!access && canCloseSession(
    actions, access.createdBy?.kind === "actor" && access.createdBy.id === userId,
    access.visibility === "universe",
  );
}

export function useSessionDeletePermission(universeId: string | undefined) {
  const { actions } = useUniverseAccess(universeId);
  return (access: ResourceAccessSummary | undefined) => !!access && canDeleteSession(actions, access.visibility === "universe");
}
