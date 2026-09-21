import { AdminGroupsPage } from "@/pages/AdminGroupsPage";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/api";
import type { SessionUser } from "./auth.js";
import { Navigate, Route, Routes, useLocation } from "react-router-dom";
import { authClient, isPlatformAdmin } from "./auth.js";
import { AppShell } from "@/components/app-shell";
import { canManage, universeHome, useActiveUniverse } from "@/lib/universes";
import { AccountPage } from "@/pages/AccountPage";
import { ApiKeysPage } from "@/pages/ApiKeysPage";
import { AdminUniversesPage } from "@/pages/AdminUniversesPage";
import { AdminUsersPage } from "@/pages/AdminUsersPage";
import { AdminChannelsPage } from "@/pages/AdminChannelsPage";
import { AdminEnvironmentProvidersPage } from "@/pages/AdminEnvironmentProvidersPage";
import { BotsPage } from "@/pages/BotsPage";
import { ChannelsPage } from "@/pages/ChannelsPage";
import { EnvironmentsPage } from "@/pages/EnvironmentsPage";
import { GeneralSettingsPage } from "@/pages/GeneralSettingsPage";
import { HomeRedirect } from "@/pages/HomeRedirect";
import { LoginPage } from "@/pages/LoginPage";
import { McpServersPage } from "@/pages/McpServersPage";
import { MembersPage } from "@/pages/MembersPage";
import { ProfilesPage } from "@/pages/ProfilesPage";
import { IntegrationsPage } from "@/pages/IntegrationsPage";
import { SecretsPage } from "@/pages/SecretsPage";
import { SessionsPage } from "@/pages/SessionsPage";
import { SetupsPage } from "@/pages/SetupsPage";
import { WorkspacesPage } from "@/pages/WorkspacesPage";
import { UserPreferencesProvider } from "@/lib/user-preferences";

function UniverseIndexRedirect() {
  const { universe, slug } = useActiveUniverse();
  if (!universe) {
    return null;
  }
  return <Navigate to={universeHome(slug ?? "")} replace />;
}

/// Bare /settings → General for people who can manage the universe, else
/// the first section members can see.
function SettingsIndexRedirect({ admin }: { admin: boolean }) {
  const { universe, slug } = useActiveUniverse();
  if (!universe) {
    return null;
  }
  return (
    <Navigate
      to={
        canManage(universe, admin)
          ? `/u/${slug}/settings/general`
          : `/u/${slug}/bots`
      }
      replace
    />
  );
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
          <AppShell user={user} admin={admin} />
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
        <Route path="u/:slug/bots" element={<BotsPage admin={admin} />} />
        <Route
          path="u/:slug/channels"
          element={<Navigate to="../settings/channels" replace relative="path" />}
        />
        <Route path="u/:slug/bots/:botId" element={<BotsPage admin={admin} view="chat" />} />
        <Route
          path="u/:slug/bots/:botId/chat/:sessionId"
          element={<BotsPage admin={admin} view="chat" />}
        />
        <Route
          path="u/:slug/bots/:botId/activity"
          element={<BotsPage admin={admin} view="activity" />}
        />
        <Route path="u/:slug/profiles" element={<ProfilesPage admin={admin} />} />
        <Route
          path="u/:slug/environments"
          element={<Navigate to="../settings/environments" replace relative="path" />}
        />
        <Route
          path="u/:slug/profiles/:profileId"
          element={<ProfilesPage admin={admin} />}
        />
        <Route path="u/:slug/settings" element={<SettingsIndexRedirect admin={admin} />} />
        <Route
          path="u/:slug/settings/general"
          element={<GeneralSettingsPage admin={admin} />}
        />
        <Route
          path="u/:slug/settings/setups"
          element={<SetupsPage admin={admin} />}
        />
        <Route
          path="u/:slug/settings/environments"
          element={<EnvironmentsPage admin={admin} />}
        />
        <Route
          path="u/:slug/settings/mcp-servers"
          element={<McpServersPage admin={admin} />}
        />
        <Route
          path="u/:slug/settings/channels"
          element={<ChannelsPage admin={admin} />}
        />
        <Route
          path="u/:slug/settings/integrations"
          element={<IntegrationsPage admin={admin} />}
        />
        <Route
          path="u/:slug/settings/secrets"
          element={<SecretsPage admin={admin} />}
        />
        <Route
          path="u/:slug/settings/api-keys"
          element={<ApiKeysPage admin={admin} />}
        />
        <Route
          path="u/:slug/settings/members"
          element={<MembersPage admin={admin} />}
        />
        {admin && (
          <>
            <Route path="admin" element={<Navigate to="/admin/users" replace />} />
            <Route path="admin/groups" element={<AdminGroupsPage />} />
            <Route path="admin/users" element={<AdminUsersPage currentUser={user} />} />
            <Route path="admin/universes" element={<AdminUniversesPage />} />
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
