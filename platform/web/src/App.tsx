import { PermissionIdentityProvider } from "@/lib/permissions";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/api";
import type { SessionUser } from "./auth.js";
import { Navigate, Route, Routes, useLocation } from "react-router-dom";
import { authClient, isPlatformAdmin } from "./auth.js";
import { AppShell } from "@/components/app-shell";
import { SettingsIndexRedirect } from "@/components/universe-nav";
import { universeHome, useActiveUniverse } from "@/lib/universes";
import { AccountPage } from "@/pages/AccountPage";
import { ApiKeysPage } from "@/pages/ApiKeysPage";
import { AdminApiKeysPage } from "@/pages/AdminApiKeysPage";
import { AdminUniversesPage } from "@/pages/AdminUniversesPage";
import { AdminUsersPage } from "@/pages/AdminUsersPage";
import { AdminChannelsPage } from "@/pages/AdminChannelsPage";
import { AdminEnvironmentProvidersPage } from "@/pages/AdminEnvironmentProvidersPage";
import { BotsPage } from "@/pages/BotsPage";
import { ChannelsPage } from "@/pages/ChannelsPage";
import { CredentialsPage } from "@/pages/CredentialsPage";
import { EnvironmentsPage } from "@/pages/EnvironmentsPage";
import { GeneralSettingsPage } from "@/pages/GeneralSettingsPage";
import { HomeRedirect } from "@/pages/HomeRedirect";
import { LoginPage } from "@/pages/LoginPage";
import { McpServersPage } from "@/pages/McpServersPage";
import { MembersPage } from "@/pages/MembersPage";
import { ModelsPage } from "@/pages/ModelsPage";
import { ProfilesPage } from "@/pages/ProfilesPage";
import { SessionsPage } from "@/pages/SessionsPage";
import { SetupsPage } from "@/pages/SetupsPage";
import { WorkspacesPage } from "@/pages/WorkspacesPage";
import { UserPreferencesProvider } from "@/lib/user-preferences";
import { FeatureGate } from "@/components/feature-gate";

function UniverseIndexRedirect() {
  const { universe } = useActiveUniverse();
  if (!universe) {
    return null;
  }
  return <Navigate to={universeHome(universe)} replace />;
}

export function App() {
  const session = authClient.useSession();
  const location = useLocation();
  const me = useQuery({ queryKey: ["me", session.data?.user.id], enabled: !!session.data,
    queryFn: () => api<{ user: SessionUser }>("GET", "/api/v1/me"), retry: false,
  });

  // While unauthenticated on /login, always render the form and never
  // unmount it: the session hook revalidates on window focus (isPending
  // flips true), and a loading gate here would remount the form and wipe
  // half-typed credentials on every tab switch.
  if (!session.data && location.pathname === "/login") {
    return <LoginPage />;
  }

  if (session.isPending && !session.data) {
    return (
      <div className="flex min-h-svh items-center justify-center text-sm text-muted-foreground">
        Loading…
      </div>
    );
  }

  if (!session.data) {
    return <Navigate to="/login" replace />;
  }

  if (!me.data) return <div className="p-8">{me.error ? me.error.message : "Loading permissions…"}</div>;
  const user = me.data.user;
  const admin = isPlatformAdmin(user);

  return (
    <Routes>
      <Route element={
        <UserPreferencesProvider userId={user.id}>
          <PermissionIdentityProvider userId={user.id} platformAdmin={admin}>
            <AppShell user={user} admin={admin} />
          </PermissionIdentityProvider>
        </UserPreferencesProvider>
      }>
        <Route index element={<HomeRedirect admin={admin} />} />
        <Route path="u/:slug" element={<UniverseIndexRedirect />} />
        <Route path="u/:slug/sessions" element={<SessionsPage admin={admin} />} />
        <Route
          path="u/:slug/sessions/:sessionId"
          element={<SessionsPage admin={admin} />}
        />
        <Route path="u/:slug/workspaces" element={<WorkspacesPage admin={admin} />} />
        <Route
          path="u/:slug/workspaces/:workspaceId"
          element={<WorkspacesPage admin={admin} />}
        />
        <Route
          path="u/:slug/workspaces/:workspaceId/files/*"
          element={<WorkspacesPage admin={admin} />}
        />
        <Route path="u/:slug/bots" element={<FeatureGate feature="bots"><BotsPage admin={admin} /></FeatureGate>} />
        <Route path="u/:slug/bots/:botId" element={<FeatureGate feature="bots"><BotsPage admin={admin} view="chat" /></FeatureGate>} />
        <Route
          path="u/:slug/bots/:botId/chat/:sessionId"
          element={<FeatureGate feature="bots"><BotsPage admin={admin} view="chat" /></FeatureGate>}
        />
        <Route
          path="u/:slug/bots/:botId/activity"
          element={<FeatureGate feature="bots"><BotsPage admin={admin} view="activity" /></FeatureGate>}
        />
        <Route path="u/:slug/profiles" element={<ProfilesPage admin={admin} />} />
        <Route
          path="u/:slug/profiles/:profileId"
          element={<ProfilesPage admin={admin} />}
        />
        <Route path="u/:slug/models" element={<ModelsPage admin={admin} />} />
        <Route path="u/:slug/environments" element={<EnvironmentsPage admin={admin} />} />
        <Route path="u/:slug/mcp-servers" element={<McpServersPage admin={admin} />} />
        <Route path="u/:slug/members" element={<MembersPage admin={admin} />} />
        <Route path="u/:slug/api-keys" element={<ApiKeysPage admin={admin} />} />
        <Route path="u/:slug/credentials" element={<CredentialsPage admin={admin} />} />
        <Route path="u/:slug/settings" element={<SettingsIndexRedirect />} />
        <Route
          path="u/:slug/settings/general"
          element={<GeneralSettingsPage admin={admin} />}
        />
        <Route
          path="u/:slug/settings/channels"
          element={<FeatureGate feature="channels"><ChannelsPage admin={admin} /></FeatureGate>}
        />
        <Route
          path="u/:slug/settings/templates"
          element={<SetupsPage admin={admin} />}
        />
        {admin && (
          <>
            <Route path="admin" element={<Navigate to="/admin/users" replace />} />
            <Route path="admin/users" element={<AdminUsersPage currentUser={user} />} />
            <Route path="admin/universes" element={<AdminUniversesPage />} />
            <Route path="admin/api-keys" element={<AdminApiKeysPage />} />
            <Route path="admin/channels" element={<AdminChannelsPage />} />
            <Route path="admin/environment-providers" element={<AdminEnvironmentProvidersPage />} />
          </>
        )}
        <Route path="account" element={<AccountPage user={user} />} />
        <Route path="login" element={<Navigate to="/" replace />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Route>
    </Routes>
  );
}
