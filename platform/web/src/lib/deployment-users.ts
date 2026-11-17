import { useQuery } from "@tanstack/react-query";
import { authClient, type SessionUser } from "@/auth";

export interface DeploymentUser extends SessionUser {
  createdAt?: string | Date;
}

/// Everyone who can sign in to the deployment, for those who manage users.
export function useDeploymentUsers(enabled = true) {
  return useQuery({
    queryKey: ["admin", "users"],
    enabled,
    queryFn: async () => {
      const result = await authClient.admin.listUsers({ query: { limit: 200, sortBy: "createdAt" } });
      if (result.error) throw new Error(result.error.message ?? "failed to load users");
      return result.data.users as DeploymentUser[];
    },
  });
}
