import { UniverseWatchProvider } from "@/lib/universe-watch";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { useEffect } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/api";
import type { SessionUser } from "./auth.js";
import { Navigate, Route, Routes, useLocation } from "react-router-dom";
import { authClient, isPlatformAdmin, useLoginConfig } from "./auth.js";
import { AppShell } from "@/components/app-shell";
import { SettingsIndexRedirect } from "@/components/universe-nav";
import { universeHome, useActiveUniverse } from "@/lib/universes";
import { AccountPage } from "@/pages/AccountPage";
import { ApiKeysPage } from "@/pages/ApiKeysPage";
import { AdminApiKeysPage } from "@/pages/AdminApiKeysPage";
import { AdminUniversesPage } from "@/pages/AdminUniversesPage";
import { AdminAuditPage } from "@/pages/AdminAuditPage";
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
import { BlobPage } from "@/pages/BlobPage";
import { UserPreferencesProvider } from "@/lib/user-preferences";
import { FeatureGate } from "@/components/feature-gate";
import { finishAutomaticSignIn } from "@/lib/automatic-sign-in";

function UniverseIndexRedirect() {
  const { universe } = useActiveUniverse();
  if (!universe) {
    return null;
  }
  return <Navigate to={universeHome(universe)} replace />;
}

export function App() {
  const session = authClient.useSession();
  const loginConfig = useLoginConfig();
  const automaticSignIn = loginConfig.data?.sso === true && loginConfig.data.autoSignIn;
  useEffect(() => {
    if (session.data) finishAutomaticSignIn();
  }, [session.data?.session.id]);
  useEffect(() => {
    const expiresAt = session.data?.session.expiresAt;
    if (!expiresAt) return;
    const timer = setTimeout(() => window.location.assign(`${import.meta.env.BASE_URL}login?expired=1`),
      Math.min(Math.max(new Date(expiresAt).getTime() - Date.now(), 0), 2_147_483_647));
    return () => clearTimeout(timer);
  }, [session.data?.session.expiresAt]);
  const location = useLocation();
  const me = useQuery({ queryKey: ["me", session.data?.user.id], enabled: !!session.data,
    queryFn: () => api<{ user: SessionUser }>("GET", "/api/v1/me"), retry: false,
  });

  // While unauthenticated on /login, always render the form and never
  // unmount it: the session hook revalidates on window focus (isPending
  // flips true), and a loading gate here would remount the form and wipe
  // half-typed credentials on every tab switch.
  if (!session.data && location.pathname === "/login") {
    const params = new URLSearchParams(location.search);
    if (!loginConfig.isPending && !automaticSignIn && params.has("auto")) {
      params.delete("auto");
      return <Navigate to={{ pathname: "/login", search: params.toString(), hash: location.hash }} replace />;
    }
    return <LoginPage />;
  }

  if (!session.data && (session.isPending || loginConfig.isPending)) {
    return (
      <div className="flex min-h-svh items-center justify-center text-sm text-muted-foreground">
        Loading…
      </div>
    );
  }

  if (!session.data) {
    return <Navigate to={automaticSignIn ? "/login?auto=1" : "/login"} replace />;
  }

  if (!me.data) return <div className="p-8">{me.error ? me.error.message : "Loading permissions…"}</div>;
  const user = me.data.user;
  const admin = isPlatformAdmin(user);

  return (
    <Routes>
      <Route element={
        <UserPreferencesProvider userId={user.id}>
          <PermissionIdentityProvider userId={user.id} platformAdmin={admin}>
            <UniverseWatchProvider><AppShell user={user} admin={admin} /></UniverseWatchProvider>
          </PermissionIdentityProvider>
        </UserPreferencesProvider>
      }>
        <Route index element={<HomeRedirect admin={admin} />} />
        <Route path="u/:handle" element={<UniverseIndexRedirect />} />
        <Route path="u/:handle/sessions" element={<SessionsPage admin={admin} />} />
        <Route
          path="u/:handle/sessions/:sessionId"
          element={<SessionsPage admin={admin} />}
        />
        <Route path="u/:handle/workspaces" element={<WorkspacesPage admin={admin} />} />
        <Route
          path="u/:handle/workspaces/:workspaceId"
          element={<WorkspacesPage admin={admin} />}
        />
        <Route
          path="u/:handle/workspaces/:workspaceId/files/*"
          element={<WorkspacesPage admin={admin} />}
        />
        <Route path="u/:handle/bots" element={<FeatureGate feature="bots"><BotsPage admin={admin} /></FeatureGate>} />
        <Route path="u/:handle/bots/:botId" element={<FeatureGate feature="bots"><BotsPage admin={admin} view="chat" /></FeatureGate>} />
        <Route
          path="u/:handle/bots/:botId/chat/:sessionId"
          element={<FeatureGate feature="bots"><BotsPage admin={admin} view="chat" /></FeatureGate>}
        />
        <Route
          path="u/:handle/bots/:botId/activity"
          element={<FeatureGate feature="bots"><BotsPage admin={admin} view="activity" /></FeatureGate>}
        />
        <Route path="u/:handle/blobs/:digest" element={<BlobPage admin={admin} />} />
        <Route path="u/:handle/profiles" element={<ProfilesPage admin={admin} />} />
        <Route
          path="u/:handle/profiles/:profileId"
          element={<ProfilesPage admin={admin} />}
        />
        <Route path="u/:handle/models" element={<ModelsPage admin={admin} />} />
        <Route path="u/:handle/environments" element={<EnvironmentsPage admin={admin} />} />
        <Route path="u/:handle/mcp-servers" element={<McpServersPage admin={admin} />} />
        <Route path="u/:handle/members" element={<MembersPage admin={admin} />} />
        <Route path="u/:handle/api-keys" element={<ApiKeysPage admin={admin} />} />
        <Route path="u/:handle/credentials" element={<CredentialsPage admin={admin} />} />
        <Route path="u/:handle/settings" element={<SettingsIndexRedirect />} />
        <Route
          path="u/:handle/settings/general"
          element={<GeneralSettingsPage admin={admin} />}
        />
        <Route
          path="u/:handle/settings/channels"
          element={<FeatureGate feature="channels"><ChannelsPage admin={admin} /></FeatureGate>}
        />
        <Route
          path="u/:handle/settings/templates"
          element={<SetupsPage admin={admin} />}
        />
        {admin && (
          <>
            <Route path="admin" element={<Navigate to="/admin/users" replace />} />
            <Route path="admin/audit" element={<AdminAuditPage />} />
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
