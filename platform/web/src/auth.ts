import { createAuthClient } from "better-auth/react";
import { adminClient, organizationClient } from "better-auth/client/plugins";

/// Same-origin client: cookies carry the session, /api/auth is the server's
/// better-auth base path (works from both /app and the vite dev proxy).
export const authClient = createAuthClient({
  plugins: [adminClient(), organizationClient()],
});

export type SessionUser = {
  id: string;
  name: string;
  email: string;
  role?: string | null;
};

export function isPlatformAdmin(user: SessionUser | undefined | null): boolean {
  return !!user?.role?.split(",").includes("admin");
}
