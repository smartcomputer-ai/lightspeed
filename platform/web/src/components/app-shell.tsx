import { OrganizationSwitcher } from "@/components/admin/organization-switcher";
import { organizationAdminPath, useAdminAccess } from "@/lib/admin-access";
import type { DeploymentAction, OrganizationAction } from "@lightspeed-ai/platform-shared";
import { useActionPermissions } from "@/lib/permissions";
import { LightspeedMark } from "@/components/brand/lightspeed-logo";
import { useEffect, type ComponentType, type CSSProperties } from "react";
import { Link, matchPath, NavLink, Outlet, useLocation, useMatch } from "react-router-dom";
import { ApiKeyIcon, AppearanceIcon, AuditLogIcon, BackIcon, ResetIcon, ReducedMotionOnIcon, EnvironmentProviderIcon, MemberIcon, SignInIcon, SettingsIcon, UniversesIcon, UserAdminIcon, UserIcon } from "@/icons";
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
  SidebarSeparator,
  SidebarTrigger,
} from "@/components/ui/sidebar";
import { UniverseSwitcher } from "@/components/universe-switcher";
import { UserMenu } from "@/components/user-menu";
import { universeNavigation, universeSectionTrail, type UniverseNavGroup } from "@/components/universe-nav";
import { DocumentTitleProvider } from "@/lib/document-title";
import { demoMode } from "@/lib/demo-mode";
import { ResizeHandle, useResizableWidth } from "@/components/resize-handle";
import { useUserPreferences } from "@/lib/user-preferences";
import { cn } from "@/lib/utils";
import { useAvailableWebModules } from "@/lib/web-modules";
import { isMasterDetailRoute, isMobileDetailRoute } from "@/lib/shell-navigation";
import { WelcomeDialog } from "@/components/onboarding/welcome-dialog";
import { PageNebula } from "@/components/starscape/page-nebula";
import { useDialogStarscapeWarmth } from "@/components/starscape/use-dialog-starscape-warmth";
import type { SessionUser } from "@/auth";
import { rememberUniverse, useUniverses } from "@/lib/universes";

/// Working universes and organization administration have explicit context switchers.
/// Platform controls are a separate deployment-wide section inside Admin.
type ShellMode = "universe" | "admin" | "settings";

const MODE_TITLE: Record<Exclude<ShellMode, "universe">, string> = {
  admin: "Admin",
  settings: "Settings",
};

/// The mode a path belongs to: its first segment, for the modes above the
/// universes.
function shellMode(pathname: string): ShellMode {
  const first = pathname.split("/")[1];
  return first === "admin" || first === "settings" ? first : "universe";
}

/// Each admin entry shows when the caller holds the action its page needs.
const PLATFORM_NAV: Array<{ to: string; icon: ComponentType; label: string; action: DeploymentAction }> = [
  { to: "/admin/platform/organizations", icon: UserIcon, label: "All Organizations", action: "administerOrganizations" },
  { to: "/admin/platform/users", icon: UserAdminIcon, label: "Users", action: "manageUsers" },
  { to: "/admin/platform/audit", icon: AuditLogIcon, label: "Audit log", action: "readAudit" },
  { to: "/admin/platform/environment-providers", icon: EnvironmentProviderIcon, label: "Environment providers", action: "configureDeployment" },
  { to: "/admin/platform/unclaimed-universes", icon: UniversesIcon, label: "Unclaimed Universes", action: "manageUnclaimedUniverses" },
  { to: "/admin/platform/settings", icon: SettingsIcon, label: "Settings", action: "configureDeployment" },
];
/// Universes has no action of its own: it opens for the organization's
/// administrators and for Owners of any of its universes.
const ORGANIZATION_NAV: Array<{ path: string; icon: ComponentType; label: string; action?: OrganizationAction }> = [
  { path: "universes", icon: UniversesIcon, label: "Universes" },
  { path: "members", icon: MemberIcon, label: "Members", action: "manageMembers" },
  { path: "api-keys", icon: ApiKeyIcon, label: "API keys", action: "manageMembers" },
  { path: "settings", icon: SettingsIcon, label: "Settings", action: "managePolicy" },
];

/// The part of the page title the route alone determines, most specific
/// first: the sidebar's name for the page and the universe or mode holding
/// it. Pages add what they have open through `useDocumentTitle`.
export function shellTitleTrail(pathname: string, universeName: string | undefined, groups: UniverseNavGroup[]): string[] {
  const mode = shellMode(pathname);
  if (mode === "admin") {
    const item = PLATFORM_NAV.find(({ to }) => pathname === to || pathname.startsWith(`${to}/`));
    if (item) return [item.label, "Platform administration"];
    const section = pathname.split("/")[4];
    return [ORGANIZATION_NAV.find(item => item.path === section)?.label ?? MODE_TITLE.admin, MODE_TITLE.admin];
  }
  if (mode !== "universe") return [MODE_TITLE[mode]];
  const universe = matchPath("/u/:handle/*", pathname);
  if (!universe) return [];
  const section = universeSectionTrail(universe.params["*"] ?? "", groups);
  return universeName ? [...section, universeName] : section;
}

/// Active on the page itself and on detail views nested under it.
function NavItem({
  to,
  icon: Icon,
  label,
}: {
  to: string;
  icon: ComponentType;
  label: string;
}) {
  const location = useLocation();
  const isActive = location.pathname === to || location.pathname.startsWith(`${to}/`);
  return (
    <SidebarMenuItem>
      <SidebarMenuButton
        isActive={isActive}
        render={<NavLink to={to} />}
        tooltip={label}
        // Dimmer at rest like list titles; hover, press and the active link
        // brighten through the button's own accent styles.
        className="text-(--item-foreground-rest) focus-visible:text-sidebar-accent-foreground"
      >
        <Icon />
        <span>{label}</span>
      </SidebarMenuButton>
    </SidebarMenuItem>
  );
}

/// Header slot for Settings: whole row navigates back to the
/// app (HomeRedirect resolves the last-visited universe or the empty
/// state); the mode title marks where you are.
function ModeHeader({ title }: { title: string }) {
  return (
    <SidebarMenu>
      <SidebarMenuItem>
        <SidebarMenuButton size="lg" className="h-(--sidebar-header-button-height)" render={<Link to="/" />} tooltip="Back">
          <div className="flex size-8 shrink-0 items-center justify-center rounded-lg border">
            <BackIcon className="size-4" />
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

/// The Settings page's cards, by element id, as the sidebar's links to
/// them, in the page's groups; `shown` leaves out a card the page leaves out.
const SETTINGS_SECTIONS: Array<{ label: string; items: Array<{ id: string; label: string; icon: ComponentType; shown?: () => boolean }> }> = [
  { label: "Account", items: [
    { id: "profile", label: "Profile", icon: UserIcon },
    { id: "security", label: "Sign-in", icon: SignInIcon },
    { id: "api-keys", label: "API keys", icon: ApiKeyIcon },
  ] },
  { label: "Appearance", items: [
    { id: "appearance", label: "Theme", icon: AppearanceIcon },
    { id: "accessibility", label: "Accessibility", icon: ReducedMotionOnIcon },
    { id: "demo-data", label: "Demo data", icon: ResetIcon, shown: () => demoMode() !== null },
  ] },
];

function scrollToSection(id: string) {
  document.getElementById(id)?.scrollIntoView({ behavior: "smooth", block: "start" });
}

export function AppShell({ user }: { user: SessionUser }) {
  const universes = useUniverses();
  const adminAccess = useAdminAccess();
  const platformNav = PLATFORM_NAV.filter(item => adminAccess.deployment(item.action));
  const match = useMatch("/u/:handle/*");
  const adminMatch = useMatch("/admin/organizations/:organizationId/universes/:handle/*");
  const activeHandle = match?.params.handle ?? adminMatch?.params.handle;
  const active = activeHandle
    ? universes.data?.find((u) => u.handle === activeHandle && (!adminMatch || u.organizationId === adminMatch.params.organizationId))
    : undefined;
  const permissions = useActionPermissions(active?.id);
  const { modules } = useAvailableWebModules(match ? active : undefined);
  const navGroups = universeNavigation(modules);
  const location = useLocation();
  const mode = shellMode(location.pathname);
  useEffect(() => {
    if (mode !== "settings") return;
    const section = SETTINGS_SECTIONS.flatMap((group) => group.items).find((item) => `#${item.id}` === location.hash);
    if (section) scrollToSection(section.id);
  }, [location.hash, location.pathname, mode]);
  // Creating dialogs open on a starscape drawn ahead, after the page has loaded.
  useDialogStarscapeWarmth();

  useEffect(() => {
    if (active && mode === "universe") {
      rememberUniverse(active.handle);
    }
  }, [active, mode]);

  // The main menu's width can be dragged, and dragged narrow enough it
  // folds to icons; both are kept per user.
  const { sidebarWidth, setSidebarWidth, sidebarCollapsed, setSidebarCollapsed } = useUserPreferences();
  const sidebarResize = useResizableWidth({
    stored: sidebarWidth,
    fallback: 256,
    min: 192,
    max: 384,
    collapse: {
      collapsed: sidebarCollapsed,
      below: 160,
      collapsedWidth: 48,
      onCollapse: setSidebarCollapsed,
    },
    onCommit: setSidebarWidth,
  });

  const mobileTitle = mode === "universe" ? (active?.name ?? "Lightspeed") : MODE_TITLE[mode];

  return (
    <SidebarProvider
      open={!sidebarResize.collapsed}
      onOpenChange={(open) => setSidebarCollapsed(!open)}
      style={{ "--sidebar-width": `${sidebarResize.width}px` } as CSSProperties}
      // A drag moves the menu with the pointer, not behind its animation.
      className={cn(
        sidebarResize.resizing
          && "[&_[data-slot=sidebar-container]]:transition-none [&_[data-slot=sidebar-gap]]:transition-none",
      )}
    >
      <Sidebar collapsible="icon">
        {/* The large mark behind the sidebar's lower part, cut off by its edges. */}
        <LightspeedMark
          data-sidebar-logo=""
          className="sidebar-logo pointer-events-none absolute -z-10 group-data-[collapsible=icon]:hidden"
        />
        {/* Admin stacks Back, the Organizations heading, and its selector. */}
        <SidebarHeader className={cn("relative shrink-0 justify-center p-(--sidebar-header-padding)", mode !== "admin" && "h-(--pane-bar-height)")}>
          {mode === "universe" ? (
            <UniverseSwitcher active={active} />
          ) : mode === "admin" ? <OrganizationSwitcher /> : (
            <ModeHeader title={MODE_TITLE[mode]} />
          )}
        </SidebarHeader>
        <SidebarContent className="relative">
          {mode === "universe" && active && navGroups.map((group, index) => {
            const items = group.items.filter((item) => permissions.can(item.action));
            if (items.length === 0) return null;
            return (
              <SidebarGroup key={group.label ?? index}>
                {group.label && <SidebarGroupLabel>{group.label}</SidebarGroupLabel>}
                <SidebarGroupContent>
                  <SidebarMenu>
                    {items.map((item) => (
                      <NavItem
                        key={item.path}
                        to={`/u/${active.handle}/${item.path}`}
                        icon={item.icon}
                        label={item.label}
                      />
                    ))}
                  </SidebarMenu>
                </SidebarGroupContent>
              </SidebarGroup>
            );
          })}
          {mode === "admin" && <>
            {adminAccess.selected && <SidebarGroup><SidebarGroupContent><SidebarMenu>
              {ORGANIZATION_NAV.filter(item => item.action ? adminAccess.organization(item.action) : adminAccess.universes).map(item => <NavItem key={item.path} to={`${organizationAdminPath(adminAccess.selected!.id)}/${item.path}`} icon={item.icon} label={item.label} />)}
            </SidebarMenu></SidebarGroupContent></SidebarGroup>}
            {platformNav.length > 0 && <><SidebarSeparator /><SidebarGroup><SidebarGroupLabel>Platform</SidebarGroupLabel><SidebarGroupContent><SidebarMenu>
              {platformNav.map(({ action: _action, ...item }) => <NavItem key={item.to} {...item} />)}
            </SidebarMenu></SidebarGroupContent></SidebarGroup></>}
          </>}
          {mode === "settings" && SETTINGS_SECTIONS.map((group) => (
            <SidebarGroup key={group.label}>
              <SidebarGroupLabel>{group.label}</SidebarGroupLabel>
              <SidebarGroupContent>
                <SidebarMenu>
                  {group.items.filter((section) => section.shown?.() ?? true).map((section) => (
                    <SidebarMenuItem key={section.id}>
                      <SidebarMenuButton
                        render={<Link to={`#${section.id}`} />}
                        onClick={() => scrollToSection(section.id)}
                        isActive={location.hash === `#${section.id}`}
                        tooltip={section.label}
                      >
                        <section.icon />
                        <span>{section.label}</span>
                      </SidebarMenuButton>
                    </SidebarMenuItem>
                  ))}
                </SidebarMenu>
              </SidebarGroupContent>
            </SidebarGroup>
          ))}
        </SidebarContent>
        <SidebarFooter>
          <UserMenu user={user} canAdmin={adminAccess.any} organizationId={active?.organizationId} />
        </SidebarFooter>
        <ResizeHandle label="Resize menu" handle={sidebarResize.handle} />
      </Sidebar>
      {/* Its own stacking context, so the nebula sits over its background and under everything else. */}
      <SidebarInset className="isolate max-h-svh">
        {/* The pages' background: behind the content area and the single-column
            pages (the list pane draws its own surface over it). */}
        <PageNebula className="absolute inset-0 -z-10" />
        {!isMobileDetailRoute(location.pathname, modules) && (
          <header className="flex h-12 shrink-0 items-center gap-2 px-4 md:hidden">
            <SidebarTrigger />
            <span className="text-sm font-medium">{mobileTitle}</span>
          </header>
        )}
        {/* Master-detail surfaces (sessions, workspaces, profiles, bots)
            manage their own panes and scrolling — full-bleed. Everything else
            gets the centered scrolling column. */}
        <DocumentTitleProvider base={shellTitleTrail(location.pathname, active?.name, navGroups)}>
          {isMasterDetailRoute(location.pathname, modules) ? (
            <main className="flex min-h-0 min-w-0 flex-1 flex-col">
              <Outlet />
            </main>
          ) : (
            <main className="min-h-0 flex-1 overflow-y-auto">
              <div className="mx-auto w-full max-w-5xl px-4 py-6 md:px-8 md:py-10">
                <Outlet />
              </div>
            </main>
          )}
        </DocumentTitleProvider>
      </SidebarInset>
      <WelcomeDialog />
    </SidebarProvider>
  );
}
