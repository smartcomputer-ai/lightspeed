import { createAuthClient } from "better-auth/react";
import { adminClient } from "better-auth/client/plugins";
import { useQuery } from "@tanstack/react-query";

/// Same-origin client: cookies carry the session, /api/auth is the server's
/// better-auth base path (works from both /app and the vite dev proxy).
export const authClient = createAuthClient({
  plugins: [adminClient()],
});

export type SessionUser = {
  id: string;
  name: string;
  email: string;
  role?: string | null;
  identitySource?: string | null;
  companyAdmitted?: boolean | null;
  providerCheckedAt?: string | Date | null;
  emergencyAdmin?: boolean | null;
  banned?: boolean | null;
};

export interface LoginConfig {
  sso: boolean;
  autoSignIn: boolean;
  providerId: string | null;
  password: "local" | "break-glass" | "off";
}

export function useLoginConfig() {
  return useQuery({ queryKey: ["login-config"], staleTime: Infinity, queryFn: async (): Promise<LoginConfig> => {
    const response = await fetch("/api/login-config", { credentials: "same-origin" });
    if (!response.ok) throw new Error("Sign-in settings are unavailable. Try again.");
    return response.json();
  } });
}

export function isPlatformAdmin(user: SessionUser | undefined | null): boolean {
  return !!user?.role?.split(",").includes("admin");
}
