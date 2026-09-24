import { createContext, useContext, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import type { AccessReadAs, AccessReadResponse, ResourceRef, UniverseAction } from "@lightspeed-ai/agent-client";
import { api } from "@/api";

const Identity = createContext<string | null>(null);
export function usePermissionIdentity() { return useContext(Identity); }

/** Keep permission previews isolated across sign-ins, including cached results. */
export function PermissionIdentityProvider({ userId, children }: { userId: string; children: ReactNode }) {
  return <Identity.Provider value={userId}>{children}</Identity.Provider>;
}

export type { ResourceRef, UniverseAction };

export function allowsAction(data: AccessReadResponse | undefined, action: UniverseAction, resource?: ResourceRef): boolean {
  if (!data) return false;
  return resource
    ? data.resources.some((entry) => entry.resource.kind === resource.kind && entry.resource.id === resource.id && entry.actions.includes(action))
    : data.actions.includes(action);
}

/**
 * Presentation hints from core. Every mutation still performs its own authorization.
 * `as: "execution_service"` decides the listed resources for the universe's
 * default agent identity instead of the caller; universe-level actions stay
 * the caller's.
 */
export function useActionPermissions(
  universeId: string | undefined,
  resources: ResourceRef[] = [],
  options: { sessionDeleteCascade?: boolean; as?: AccessReadAs } = {},
) {
  const userId = useContext(Identity);
  const targets = [...new Map(resources.map((resource) => [`${resource.kind}:${resource.id}`, resource])).values()]
    .sort((a, b) => a.kind.localeCompare(b.kind) || a.id.localeCompare(b.id));
  const cascade = options.sessionDeleteCascade === true;
  const as = options.as === "execution_service" ? options.as : undefined;
  const query = useQuery({
    queryKey: ["action-permissions", userId, universeId, targets, cascade, as ?? "caller"],
    enabled: !!universeId && !!userId,
    queryFn: async () => {
      const batches = Array.from({ length: Math.max(1, Math.ceil(targets.length / 100)) }, (_, index) => targets.slice(index * 100, (index + 1) * 100));
      let result: AccessReadResponse | undefined;
      for (const resources of batches) {
        const next = await api<AccessReadResponse>("POST", `/api/v1/universes/${universeId}/access`, {
          resources, sessionDeleteCascade: cascade, ...(as ? { as } : {}),
        });
        result = result ? { actions: result.actions.filter((action) => next.actions.includes(action)), resources: [...result.resources, ...next.resources] } : next;
      }
      return result!;
    },
    staleTime: 0,
    refetchOnMount: "always",
    // Hints follow mounts, focus and mutations. They are never authority, so
    // there is no background polling: a stale hint only means a refused action.
    refetchOnWindowFocus: true,
    retry: false,
  });
  return {
    ...query,
    can: (action: UniverseAction, resource?: ResourceRef) => !!userId && !!universeId && !query.isError && allowsAction(query.data, action, resource),
  };
}
