import { createAuthClient } from "better-auth/react";
import { adminClient } from "better-auth/client/plugins";
import { useQuery } from "@tanstack/react-query";
import { failureText, readResponseBody } from "@/lib/response-body";

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
    return readLoginConfig(response);
  } });
}

/// The sign-in page shows this error as it is. An answer that is not JSON,
/// under any status, came from something in front of the Platform, so its
/// status and text are part of the message.
export async function readLoginConfig(response: Response): Promise<LoginConfig> {
  const answer = await readResponseBody(response);
  if (answer.kind === "text") {
    throw new Error(`Sign-in settings are unavailable (${failureText(response.status, answer.text)}). Try again.`);
  }
  if (!response.ok || answer.kind === "empty") throw new Error("Sign-in settings are unavailable. Try again.");
  return answer.value as LoginConfig;
}

export function isPlatformAdmin(user: SessionUser | undefined | null): boolean {
  return !!user?.role?.split(",").includes("admin");
}
