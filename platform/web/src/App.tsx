import { Navigate, Route, Routes, useLocation } from "react-router-dom";
import { authClient, isPlatformAdmin } from "./auth.js";
import { AppShell } from "@/components/app-shell";
import { canManage, universeHome, useActiveUniverse } from "@/lib/universes";
import { AccountPage } from "@/pages/AccountPage";
import { ApiKeysPage } from "@/pages/ApiKeysPage";
import { AdminUniversesPage } from "@/pages/AdminUniversesPage";
import { AdminUsersPage } from "@/pages/AdminUsersPage";
import { AdminChannelsPage } from "@/pages/AdminChannelsPage";
import { ChannelsPage } from "@/pages/ChannelsPage";
import { EnvironmentsPage } from "@/pages/EnvironmentsPage";
import { FoundryPage } from "@/pages/FoundryPage";
import { GeneralSettingsPage } from "@/pages/GeneralSettingsPage";
import { HomeRedirect } from "@/pages/HomeRedirect";
import { LoginPage } from "@/pages/LoginPage";
import { McpServersPage } from "@/pages/McpServersPage";
import { MembersPage } from "@/pages/MembersPage";
import { ProfilesPage } from "@/pages/ProfilesPage";
import { SecretsPage } from "@/pages/SecretsPage";
import { SessionsPage } from "@/pages/SessionsPage";
import { SetupsPage } from "@/pages/SetupsPage";
import { WorkspacesPage } from "@/pages/WorkspacesPage";

function UniverseIndexRedirect({ admin }: { admin: boolean }) {
  const { universe, slug } = useActiveUniverse();
  if (!universe) {
    return null;
  }
  return <Navigate to={universeHome(slug ?? "", canManage(universe, admin))} replace />;
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
          : `/u/${slug}/settings/channels`
      }
      replace
    />
  );
}

export function App() {
  const session = authClient.useSession();
  const location = useLocation();

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

  const user = session.data.user;
  const admin = isPlatformAdmin(user);

  return (
    <Routes>
      <Route element={<AppShell user={user} admin={admin} />}>
        <Route index element={<HomeRedirect admin={admin} />} />
        <Route path="u/:slug" element={<UniverseIndexRedirect admin={admin} />} />
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
        <Route path="u/:slug/foundry" element={<FoundryPage admin={admin} />} />
        <Route path="u/:slug/foundry/:packId" element={<FoundryPage admin={admin} />} />
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
          path="u/:slug/settings/secrets"
          element={<SecretsPage admin={admin} />}
        />
        <Route
          path="u/:slug/settings/channels"
          element={<ChannelsPage admin={admin} />}
        />
        <Route
          path="u/:slug/settings/chat-bindings"
          element={<Navigate to="../channels" replace relative="path" />}
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
            <Route path="admin/users" element={<AdminUsersPage />} />
            <Route path="admin/universes" element={<AdminUniversesPage />} />
            <Route path="admin/channels" element={<AdminChannelsPage />} />
          </>
        )}
        <Route path="account" element={<AccountPage user={user} />} />
        <Route path="login" element={<Navigate to="/" replace />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Route>
    </Routes>
  );
}
