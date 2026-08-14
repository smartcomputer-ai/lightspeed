import { useEffect } from "react";
import { Link, NavLink, Outlet, useLocation, useMatch } from "react-router-dom";
import {
  ArrowLeft,
  Boxes,
  FolderGit2,
  Globe,
  Hammer,
  KeyRound,
  LockKeyhole,
  MessagesSquare,
  Palette,
  PackageOpen,
  RadioTower,
  Server,
  Settings,
  SlidersHorizontal,
  UserRound,
  Users,
  UserCog,
} from "lucide-react";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarInset,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarProvider,
  SidebarTrigger,
} from "@/components/ui/sidebar";
import { UniverseSwitcher } from "@/components/universe-switcher";
import { UserMenu } from "@/components/user-menu";
import type { SessionUser } from "@/auth";
import { canManage, rememberUniverse, useUniverses } from "@/lib/universes";

/// The sidebar has three modes. Universe mode is the app's top level:
/// switcher + universe nav. Admin and account are modes *above* the
/// universe structure — the header becomes a deliberately generic "Back"
/// (naming no universe) and the content is the mode's own menu.
type ShellMode = "universe" | "admin" | "account";

function NavItem({
  to,
  icon: Icon,
  label,
  prefix = false,
}: {
  to: string;
  icon: typeof MessagesSquare;
  label: string;
  /// Match nested routes too (detail views under this section).
  prefix?: boolean;
}) {
  const location = useLocation();
  const isActive = prefix
    ? location.pathname === to || location.pathname.startsWith(`${to}/`)
    : location.pathname === to;
  return (
    <SidebarMenuItem>
      <SidebarMenuButton isActive={isActive} render={<NavLink to={to} />} tooltip={label}>
        <Icon />
        <span>{label}</span>
      </SidebarMenuButton>
    </SidebarMenuItem>
  );
}

/// Header slot for admin/account modes: whole row navigates back to the
/// app (HomeRedirect resolves the last-visited universe or the empty
/// state); the mode title marks where you are.
function ModeHeader({ title }: { title: string }) {
  return (
    <SidebarMenu>
      <SidebarMenuItem>
        <SidebarMenuButton size="lg" render={<Link to="/" />}>
          <div className="flex size-8 items-center justify-center rounded-lg border">
            <ArrowLeft className="size-4" />
          </div>
          <div className="grid flex-1 text-left leading-tight">
            <span className="truncate text-sm font-medium">{title}</span>
            <span className="truncate text-xs text-muted-foreground">Back</span>
          </div>
        </SidebarMenuButton>
      </SidebarMenuItem>
    </SidebarMenu>
  );
}

function scrollToSection(id: string) {
  document.getElementById(id)?.scrollIntoView({ behavior: "smooth", block: "start" });
}

export function AppShell({ user, admin }: { user: SessionUser; admin: boolean }) {
  const universes = useUniverses();
  const match = useMatch("/u/:slug/*");
  const activeSlug = match?.params.slug;
  const active = activeSlug
    ? universes.data?.find((u) => u.slug === activeSlug)
    : undefined;
  const location = useLocation();
  const mode: ShellMode = location.pathname.startsWith("/admin")
    ? "admin"
    : location.pathname.startsWith("/account")
      ? "account"
      : "universe";

  useEffect(() => {
    if (active) {
      rememberUniverse(active.slug);
    }
  }, [active]);

  const mobileTitle =
    mode === "admin"
      ? "Platform admin"
      : mode === "account"
        ? "Account"
        : (active?.name ?? "Lightspeed");

  return (
    <SidebarProvider>
      <Sidebar>
        <SidebarHeader>
          {mode === "universe" ? (
            <UniverseSwitcher active={active} admin={admin} />
          ) : (
            <ModeHeader title={mode === "admin" ? "Platform admin" : "Your account"} />
          )}
        </SidebarHeader>
        <SidebarContent>
          {mode === "universe" && active && (
            <>
              {/* No group label: the switcher above already names the
                  universe. Members see only the Settings group. */}
              {canManage(active, admin) && (
                <SidebarGroup>
                  <SidebarGroupContent>
                    <SidebarMenu>
                      <NavItem
                        to={`/u/${active.slug}/sessions`}
                        icon={MessagesSquare}
                        label="Sessions"
                        prefix
                      />
                      <NavItem
                        to={`/u/${active.slug}/workspaces`}
                        icon={FolderGit2}
                        label="Workspaces"
                        prefix
                      />
                      <NavItem
                        to={`/u/${active.slug}/profiles`}
                        icon={SlidersHorizontal}
                        label="Profiles"
                        prefix
                      />
                      <NavItem
                        to={`/u/${active.slug}/foundry`}
                        icon={Hammer}
                        label="Foundry"
                        prefix
                      />
                    </SidebarMenu>
                  </SidebarGroupContent>
                </SidebarGroup>
              )}
              {/* Configuration: which chats bind here, who has access,
                  and (owner/admin) the universe itself. */}
              <SidebarGroup>
                <SidebarGroupLabel>Settings</SidebarGroupLabel>
                <SidebarGroupContent>
                  <SidebarMenu>
                    {canManage(active, admin) && (
                      <>
                        <NavItem
                          to={`/u/${active.slug}/settings/general`}
                          icon={Settings}
                          label="General"
                        />
                        <NavItem
                          to={`/u/${active.slug}/settings/setups`}
                          icon={PackageOpen}
                          label="Setups"
                        />
                        <NavItem
                          to={`/u/${active.slug}/settings/environments`}
                          icon={Boxes}
                          label="Environments"
                        />
                        <NavItem
                          to={`/u/${active.slug}/settings/mcp-servers`}
                          icon={Server}
                          label="MCP servers"
                        />
                      </>
                    )}
                    <NavItem
                      to={`/u/${active.slug}/settings/channels`}
                      icon={RadioTower}
                      label="Channels"
                    />
                    {canManage(active, admin) && (
                      <>
                        <NavItem
                          to={`/u/${active.slug}/settings/secrets`}
                          icon={LockKeyhole}
                          label="Secrets"
                        />
                        <NavItem
                          to={`/u/${active.slug}/settings/api-keys`}
                          icon={KeyRound}
                          label="API keys"
                        />
                      </>
                    )}
                    <NavItem
                      to={`/u/${active.slug}/settings/members`}
                      icon={Users}
                      label="Members"
                    />
                  </SidebarMenu>
                </SidebarGroupContent>
              </SidebarGroup>
            </>
          )}
          {mode === "admin" && (
            <SidebarGroup>
              <SidebarGroupLabel>Platform admin</SidebarGroupLabel>
              <SidebarGroupContent>
                <SidebarMenu>
                  <NavItem to="/admin/users" icon={UserCog} label="Users" />
                  <NavItem to="/admin/universes" icon={Globe} label="Universes" />
                  <NavItem to="/admin/channels" icon={RadioTower} label="Channels" />
                </SidebarMenu>
              </SidebarGroupContent>
            </SidebarGroup>
          )}
          {mode === "account" && (
            <SidebarGroup>
              <SidebarGroupLabel>Your account</SidebarGroupLabel>
              <SidebarGroupContent>
                <SidebarMenu>
                  <SidebarMenuItem>
                    <SidebarMenuButton onClick={() => scrollToSection("profile")}>
                      <UserRound />
                      <span>Profile</span>
                    </SidebarMenuButton>
                  </SidebarMenuItem>
                  <SidebarMenuItem>
                    <SidebarMenuButton onClick={() => scrollToSection("security")}>
                      <KeyRound />
                      <span>Security</span>
                    </SidebarMenuButton>
                  </SidebarMenuItem>
                  <SidebarMenuItem>
                    <SidebarMenuButton onClick={() => scrollToSection("appearance")}>
                      <Palette />
                      <span>Appearance</span>
                    </SidebarMenuButton>
                  </SidebarMenuItem>
                </SidebarMenu>
              </SidebarGroupContent>
            </SidebarGroup>
          )}
        </SidebarContent>
        <SidebarFooter>
          <UserMenu user={user} admin={admin} />
        </SidebarFooter>
      </Sidebar>
      <SidebarInset className="max-h-svh">
        <header className="flex h-12 shrink-0 items-center gap-2 px-4 md:hidden">
          <SidebarTrigger />
          <span className="text-sm font-medium">{mobileTitle}</span>
        </header>
        {/* Master-detail surfaces (sessions, workspaces, profiles, Foundry) manage
            their own panes and scrolling — full-bleed. Everything else
            gets the centered scrolling column. */}
        {/\/u\/[^/]+\/(sessions|workspaces|profiles|foundry)/.test(location.pathname) ? (
          <main className="flex min-h-0 min-w-0 flex-1 flex-col">
            <Outlet />
          </main>
        ) : (
          <main className="min-h-0 flex-1 overflow-y-auto">
            <div className="mx-auto w-full max-w-4xl px-4 py-6 md:px-8 md:py-10">
              <Outlet />
            </div>
          </main>
        )}
      </SidebarInset>
    </SidebarProvider>
  );
}
