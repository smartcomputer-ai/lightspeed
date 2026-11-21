import { AdminGate, AdminIndexRedirect } from "@/components/admin/admin-gate";
import { OrganizationMembersPage } from "@/pages/OrganizationMembersPage";
import { OrganizationApiKeysPage } from "@/pages/OrganizationApiKeysPage";
import { OrganizationSettingsPage } from "@/pages/OrganizationSettingsPage";
import { PlatformOrganizationsPage } from "@/pages/PlatformOrganizationsPage";
import { PlatformSettingsPage } from "@/pages/PlatformSettingsPage";
import { UniverseWatchProvider } from "@/lib/universe-watch";
import { PermissionIdentityProvider } from "@/lib/permissions";
import { lazy, Suspense, useEffect } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/api";
import type { SessionUser } from "./auth.js";
import { Navigate, Route, Routes, useLocation } from "react-router-dom";
import { authClient, isPlatformAdmin, useLoginConfig } from "./auth.js";
import { AppShell } from "@/components/app-shell";
import { SettingsIndexRedirect } from "@/components/universe-nav";
import { universeHome, useActiveUniverse } from "@/lib/universes";
import { AdminUniversesPage } from "@/pages/AdminUniversesPage";
import { UnclaimedUniversesPage } from "@/pages/UnclaimedUniversesPage";
import { AdminAuditPage } from "@/pages/AdminAuditPage";
import { AdminUsersPage } from "@/pages/AdminUsersPage";
import { AdminEnvironmentProvidersPage } from "@/pages/AdminEnvironmentProvidersPage";
import { BotsPage } from "@/pages/BotsPage";
import { CredentialsPage } from "@/pages/CredentialsPage";
import { EnvironmentsPage } from "@/pages/EnvironmentsPage";
import { UniverseGeneralSettingsPage } from "@/pages/UniverseGeneralSettingsPage";
import { HomeRedirect } from "@/pages/HomeRedirect";
import { LoginPage } from "@/pages/LoginPage";
import { DevicePage } from "@/pages/DevicePage";
import { deviceLoginPath, loginReturnPath } from "@/lib/login-return";
import { McpServersPage } from "@/pages/McpServersPage";
import { UniverseMembersPage } from "@/pages/UniverseMembersPage";
import { UniverseRedirect } from "@/components/universe-redirect";
import { ModelsPage } from "@/pages/ModelsPage";
import { SettingsPage, SettingsRedirect } from "@/pages/SettingsPage";
import { ProfilesPage } from "@/pages/ProfilesPage";
import { ModuleRoute } from "@/components/module-route";
import { CenteredNote, LoadingNote, UniverseNotFound } from "@/components/page";
import { ReadError } from "@/components/read-error";
import { useAvailableWebModules, useBundledModules } from "@/lib/web-modules";
import { WorkspacesPage } from "@/pages/WorkspacesPage";
import { BlobPage } from "@/pages/BlobPage";
import { UserPreferencesProvider } from "@/lib/user-preferences";
import { FeatureGate } from "@/components/feature-gate";
import { finishAutomaticSignIn } from "@/lib/automatic-sign-in";
import { debugModeEnabled } from "@/devtools/debug-mode";

// Tool pages under /debug: the UI overview in development builds, the
// design-token editor in debug mode. Builds with neither never load them.
const DebugRoutes = import.meta.env.DEV || debugModeEnabled ? lazy(() => import("@/devtools/DebugRoutes")) : null;

function UniverseIndexRedirect() {
  const { universe, handle, isLoading } = useActiveUniverse();
  const available = useAvailableWebModules(universe);
  if (isLoading) return <LoadingNote />;
  if (!universe) return <UniverseNotFound handle={handle} />;
  if (!universe.actions?.includes("read")) return <CenteredNote>You do not have access to this universe.</CenteredNote>;
  if (universe.features.bots) return <Navigate to={universeHome(universe)} replace />;
  if (available.isLoading) return <LoadingNote />;
  if (available.error) return <ReadError error={available.error} prefix="Module availability" />;
  return <Navigate to={`/u/${universe.handle}/${available.modules[0]?.id ?? "workspaces"}`} replace />;
}

export function App() {
  const modules = useBundledModules();
  const location = useLocation();
  const session = authClient.useSession();
  const loginConfig = useLoginConfig();
  const automaticSignIn = loginConfig.data?.sso === true && loginConfig.data.autoSignIn;
  useEffect(() => {
    if (session.data) finishAutomaticSignIn();
  }, [session.data?.session.id]);
  useEffect(() => {
    const expiresAt = session.data?.session.expiresAt;
    if (!expiresAt) return;
    const timer = setTimeout(() => window.location.assign(location.pathname === "/device"
      ? `${import.meta.env.BASE_URL.replace(/\/$/, "")}${deviceLoginPath(location.search)}&expired=1`
      : `${import.meta.env.BASE_URL}login?expired=1`),
      Math.min(Math.max(new Date(expiresAt).getTime() - Date.now(), 0), 2_147_483_647));
    return () => clearTimeout(timer);
  }, [session.data?.session.expiresAt, location.pathname, location.search]);
  const me = useQuery({ queryKey: ["me", session.data?.user.id], enabled: !!session.data,
    queryFn: () => api<{ user: SessionUser }>("GET", "/api/ui/me"), retry: false,
  });

  if (DebugRoutes && location.pathname.startsWith("/debug/")) {
    return <Suspense fallback={null}><DebugRoutes /></Suspense>;
  }

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
    if (location.pathname === "/device") return <Navigate to={deviceLoginPath(location.search, automaticSignIn)} replace />;
    return <Navigate to={automaticSignIn ? "/login?auto=1" : "/login"} replace />;
  }

  if (location.pathname === "/login") return <Navigate to={loginReturnPath(location.search)} replace />;
  if (location.pathname === "/device") return <DevicePage key={location.search} user={session.data.user} />;

  if (!me.data) return <div className="p-8">{me.error ? me.error.message : "Loading permissions…"}</div>;
  const user = me.data.user;
  const admin = isPlatformAdmin(user);

  return (
    <Routes>
      <Route element={
        <UserPreferencesProvider userId={user.id}>
          <PermissionIdentityProvider userId={user.id} platformAdmin={admin}>
            <UniverseWatchProvider><AppShell user={user} /></UniverseWatchProvider>
          </PermissionIdentityProvider>
        </UserPreferencesProvider>
      }>
        <Route index element={<HomeRedirect />} />
        <Route path="u/:handle" element={<UniverseIndexRedirect />} />
        <Route path="u/:handle/*" element={<UniverseIndexRedirect />} />
        {modules.flatMap(module => module.routes.map(({ path, Page }) => (
          <Route key={`${module.id}/${path}`} path={`u/:handle/${module.id}${path && `/${path}`}`}
            element={<ModuleRoute module={module} Page={Page} />} />
        )))}
        <Route path="u/:handle/workspaces" element={<WorkspacesPage />} />
        <Route
          path="u/:handle/workspaces/:workspaceId"
          element={<WorkspacesPage />}
        />
        <Route
          path="u/:handle/workspaces/:workspaceId/files/*"
          element={<WorkspacesPage />}
        />
        <Route path="u/:handle/bots" element={<FeatureGate feature="bots"><BotsPage /></FeatureGate>} />
        <Route path="u/:handle/bots/:botId" element={<FeatureGate feature="bots"><BotsPage view="chat" /></FeatureGate>} />
        <Route
          path="u/:handle/bots/:botId/chat/:sessionId"
          element={<FeatureGate feature="bots"><BotsPage view="chat" /></FeatureGate>}
        />
        <Route
          path="u/:handle/bots/:botId/activity"
          element={<FeatureGate feature="bots"><BotsPage view="activity" /></FeatureGate>}
        />
        <Route path="u/:handle/blobs/:digest" element={<BlobPage />} />
        <Route path="u/:handle/profiles" element={<ProfilesPage />} />
        <Route
          path="u/:handle/profiles/:profileId"
          element={<ProfilesPage />}
        />
        <Route path="u/:handle/models" element={<ModelsPage />} />
        <Route path="u/:handle/environments" element={<EnvironmentsPage />} />
        <Route path="u/:handle/mcp-servers" element={<McpServersPage />} />
        <Route path="u/:handle/credentials" element={<CredentialsPage />} />
        <Route path="u/:handle/settings" element={<SettingsIndexRedirect />} />
        <Route path="u/:handle/settings/general" element={<UniverseGeneralSettingsPage />} />
        <Route path="u/:handle/settings/members" element={<UniverseMembersPage />} />
        <Route element={<AdminGate />}>
          <Route path="admin" element={<AdminIndexRedirect />} />
          <Route element={<AdminGate universes />}>
            <Route path="admin/organizations/:organizationId/universes" element={<AdminUniversesPage />} />
            {/* A universe's settings and members live in the universe itself. */}
            <Route path="admin/organizations/:organizationId/universes/:handle" element={<UniverseRedirect to="settings/general" />} />
            <Route path="admin/organizations/:organizationId/universes/:handle/access" element={<UniverseRedirect to="settings/members" />} />
            <Route path="admin/organizations/:organizationId/universes/:handle/settings" element={<UniverseRedirect to="settings/general" />} />
          </Route>
          <Route element={<AdminGate organizationAction="manageMembers" />}>
            <Route path="admin/organizations/:organizationId/members" element={<OrganizationMembersPage />} />
            <Route path="admin/organizations/:organizationId/api-keys" element={<OrganizationApiKeysPage />} />
            {["people", "service-users"].map((path) => (
              <Route key={path} path={`admin/organizations/:organizationId/${path}`} element={<Navigate to="../members" relative="path" replace />} />
            ))}
          </Route>
          <Route element={<AdminGate organizationAction="managePolicy" />}>
            <Route path="admin/organizations/:organizationId/settings" element={<OrganizationSettingsPage />} />
          </Route>
          <Route element={<AdminGate deploymentAction="administerOrganizations" />}>
            <Route path="admin/platform/organizations" element={<PlatformOrganizationsPage />} />
          </Route>
          <Route element={<AdminGate deploymentAction="manageUnclaimedUniverses" />}>
            <Route path="admin/platform/unclaimed-universes" element={<UnclaimedUniversesPage />} />
          </Route>
          <Route element={<AdminGate deploymentAction="readAudit" />}>
            <Route path="admin/platform/audit" element={<AdminAuditPage />} />
          </Route>
          <Route element={<AdminGate deploymentAction="manageUsers" />}>
            <Route path="admin/platform/users" element={<AdminUsersPage currentUser={user} />} />
          </Route>
          <Route element={<AdminGate deploymentAction="configureDeployment" />}>
            <Route path="admin/platform/environment-providers" element={<AdminEnvironmentProvidersPage />} />
            <Route path="admin/platform/settings" element={<PlatformSettingsPage />} />
          </Route>
        </Route>
        <Route path="settings" element={<SettingsPage user={user} />} />
        <Route path="account" element={<SettingsRedirect />} />
        <Route path="preferences" element={<SettingsRedirect />} />
        <Route path="login" element={<Navigate to="/" replace />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Route>
    </Routes>
  );
}
