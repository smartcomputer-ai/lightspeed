import { createContext, useContext, type ReactNode } from "react";
import type { ResourceAccessSummary, UniverseAction } from "@lightspeed-ai/sdk";
import { canCloseSession, canDeleteSession, roleAtLeast, universeRoleSchema, type UniverseRole } from "@lightspeed-ai/platform-shared";
import { useActiveUniverse } from "@/lib/universes";

type Identity = { userId: string; platformAdmin: boolean };
const IdentityContext = createContext<Identity | null>(null);
export function usePermissionIdentity() { return useContext(IdentityContext)?.userId ?? null; }

/** The signed-in user permission hints are computed for. */
export function PermissionIdentityProvider({ userId, platformAdmin, children }: { userId: string; platformAdmin: boolean; children: ReactNode }) {
  return <IdentityContext.Provider value={{ userId, platformAdmin }}>{children}</IdentityContext.Provider>;
}

/** What the UI offers; `manage_access` is managing members and keys. */
export type PermissionAction = UniverseAction | "manage_access";

/** The least role each action needs, as the core manifest recommends it. */
const ACTION_ROLES: Record<PermissionAction, UniverseRole> = {
  read: "viewer",
  create_session: "contributor",
  control_session: "contributor",
  configure_session: "operator",
  stop_session: "contributor",
  close_session: "contributor",
  delete_session: "operator",
  set_session_retention: "admin",
  share_session: "contributor",
  invoke_bot: "contributor",
  use_resource: "contributor",
  create_workspace: "operator",
  create_profile: "operator",
  manage_profile: "operator",
  create_bot: "operator",
  manage_bot: "operator",
  configure_resource: "operator",
  manage_access: "admin",
};

/** Whether `role` may take `action`. */
export function allowsAction(role: UniverseRole | null | undefined, action: PermissionAction): boolean {
  return !!role && roleAtLeast(role, ACTION_ROLES[action]);
}

/** The signed-in user's role in a universe: their membership, or admin for a platform admin. */
export function useUniverseRole(universeId: string | undefined): UniverseRole | null {
  return useRole(universeId).role;
}

/// Hints are for the universe on screen; any other universe gets none.
function useRole(universeId: string | undefined): { role: UniverseRole | null; isLoading: boolean } {
  const identity = useContext(IdentityContext);
  const active = useActiveUniverse();
  const universe = active.universe?.id === universeId ? active.universe : undefined;
  if (!identity || !universe) return { role: null, isLoading: active.isLoading };
  if (identity.platformAdmin) return { role: "admin", isLoading: false };
  return { role: universeRoleSchema.safeParse(universe.role).data ?? null, isLoading: false };
}

/**
 * Presentation hints from the member's role. The server checks every request
 * again, including whether a session is shared or the member's own.
 */
export function useActionPermissions(universeId: string | undefined) {
  const { role, isLoading } = useRole(universeId);
  return {
    role,
    isLoading,
    can: (action: PermissionAction) => allowsAction(role, action),
  };
}

/** Session-aware hints shared by individual and bulk lifecycle controls. */
export function useSessionClosePermission(universeId: string | undefined) {
  const role = useUniverseRole(universeId);
  const userId = usePermissionIdentity();
  return (access: ResourceAccessSummary | undefined) => !!access && canCloseSession(
    role, access.createdBy?.kind === "actor" && access.createdBy.id === userId,
    access.visibility === "universe",
  );
}

export function useSessionDeletePermission(universeId: string | undefined) {
  const role = useUniverseRole(universeId);
  return (access: ResourceAccessSummary | undefined) => !!access && canDeleteSession(role, access.visibility === "universe");
}
