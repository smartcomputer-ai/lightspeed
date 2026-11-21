import { skipToken, useQuery } from "@tanstack/react-query";
import { platformApi } from "./platform-api";

export function organizationModulesKey(organizationId: string | undefined) {
  return ["organization-modules", organizationId] as const;
}

export function useOrganizationModules(organizationId: string | undefined) {
  return useQuery({
    queryKey: organizationModulesKey(organizationId),
    queryFn: organizationId ? () => platformApi("platform/organizations/modules/list", { organizationId }) : skipToken,
    refetchOnWindowFocus: true,
    refetchInterval: 30_000,
  });
}
