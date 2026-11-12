import { useActionPermissions } from "@/lib/permissions";
import { LightspeedMark } from "@/components/brand/lightspeed-logo";
import { useEffect, type ComponentType, type CSSProperties } from "react";
import { Link, matchPath, NavLink, Outlet, useLocation, useMatch } from "react-router-dom";
import { ApiKeyIcon, AppearanceIcon, AuditLogIcon, BackIcon, ChannelIcon, EnvironmentProviderIcon, SecurityIcon, UniversesIcon, UserAdminIcon, UserIcon } from "@/icons";
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
import { UNIVERSE_NAV, navItemVisible, universeSectionTrail } from "@/components/universe-nav";
import { DocumentTitleProvider } from "@/lib/document-title";
import { ResizeHandle, useResizableWidth } from "@/components/resize-handle";
import { useUserPreferences } from "@/lib/user-preferences";
import { cn } from "@/lib/utils";
import { isMobileDetailRoute } from "@/lib/shell-navigation";
import { WelcomeDialog } from "@/components/onboarding/welcome-dialog";
import { PageNebula } from "@/components/starscape/page-nebula";
import type { SessionUser } from "@/auth";
import { rememberUniverse, useUniverses } from "@/lib/universes";

/// The sidebar has three modes. Universe mode is the app's top level:
/// switcher + universe nav. Admin and account are modes *above* the
/// universe structure — the header becomes a deliberately generic "Back"
/// (naming no universe) and the content is the mode's own menu.
type ShellMode = "universe" | "admin" | "account";

const ADMIN_TITLE = "Platform admin";
const ACCOUNT_TITLE = "Your account";

const ADMIN_NAV: Array<{ to: string; icon: ComponentType; label: string }> = [
  { to: "/admin/users", icon: UserAdminIcon, label: "Users" },
  { to: "/admin/universes", icon: UniversesIcon, label: "Universes" },
  { to: "/admin/api-keys", icon: ApiKeyIcon, label: "API keys" },
  { to: "/admin/channels", icon: ChannelIcon, label: "Channels" },
  { to: "/admin/audit", icon: AuditLogIcon, label: "Audit log" },
  { to: "/admin/environment-providers", icon: EnvironmentProviderIcon, label: "Environment providers" },
];

/// The part of the page title the route alone determines, most specific
/// first: the sidebar's name for the page and the universe or mode holding
/// it. Pages add what they have open through `useDocumentTitle`.
export function shellTitleTrail(pathname: string, universeName: string | undefined): string[] {
  if (pathname === "/admin" || pathname.startsWith("/admin/")) {
    const item = ADMIN_NAV.find(({ to }) => pathname === to || pathname.startsWith(`${to}/`));
    return item ? [item.label, ADMIN_TITLE] : [ADMIN_TITLE];
  }
  if (pathname === "/account" || pathname.startsWith("/account/")) return [ACCOUNT_TITLE];
  const universe = matchPath("/u/:slug/*", pathname);
  if (!universe) return [];
  const section = universeSectionTrail(universe.params["*"] ?? "");
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

/// Header slot for admin/account modes: whole row navigates back to the
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
  const permissions = useActionPermissions(active?.id);
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

  // The main menu's width can be dragged, and dragged narrow enough it
  // folds to icons; both are kept per account.
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

  const mobileTitle =
    mode === "admin"
      ? ADMIN_TITLE
      : mode === "account"
        ? "Account"
        : (active?.name ?? "Lightspeed");

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
        {/* Fixed at the pane bar height, so the collapsed sidebar keeps the line too. */}
        <SidebarHeader className="relative h-(--pane-bar-height) shrink-0 justify-center p-(--sidebar-header-padding)">
          {mode === "universe" ? (
            <UniverseSwitcher active={active} admin={admin} />
          ) : (
            <ModeHeader title={mode === "admin" ? ADMIN_TITLE : ACCOUNT_TITLE} />
          )}
        </SidebarHeader>
        <SidebarContent className="relative">
          {mode === "universe" && active && UNIVERSE_NAV.map((group, index) => {
            const items = group.items.filter((item) => navItemVisible(item, permissions.can, active.features));
            if (items.length === 0) return null;
            return (
              <SidebarGroup key={group.label ?? index}>
                {group.label && <SidebarGroupLabel>{group.label}</SidebarGroupLabel>}
                <SidebarGroupContent>
                  <SidebarMenu>
                    {items.map((item) => (
                      <NavItem
                        key={item.path}
                        to={`/u/${active.slug}/${item.path}`}
                        icon={item.icon}
                        label={item.label}
                      />
                    ))}
                  </SidebarMenu>
                </SidebarGroupContent>
              </SidebarGroup>
            );
          })}
          {mode === "admin" && (
            <SidebarGroup>
              <SidebarGroupLabel>{ADMIN_TITLE}</SidebarGroupLabel>
              <SidebarGroupContent>
                <SidebarMenu>
                  {ADMIN_NAV.map((item) => (
                    <NavItem key={item.to} to={item.to} icon={item.icon} label={item.label} />
                  ))}
                </SidebarMenu>
              </SidebarGroupContent>
            </SidebarGroup>
          )}
          {mode === "account" && (
            <SidebarGroup>
              <SidebarGroupLabel>{ACCOUNT_TITLE}</SidebarGroupLabel>
              <SidebarGroupContent>
                <SidebarMenu>
                  <SidebarMenuItem>
                    <SidebarMenuButton onClick={() => scrollToSection("profile")} tooltip="Profile">
                      <UserIcon />
                      <span>Profile</span>
                    </SidebarMenuButton>
                  </SidebarMenuItem>
                  <SidebarMenuItem>
                    <SidebarMenuButton onClick={() => scrollToSection("security")} tooltip="Security">
                      <SecurityIcon />
                      <span>Security</span>
                    </SidebarMenuButton>
                  </SidebarMenuItem>
                  <SidebarMenuItem>
                    <SidebarMenuButton onClick={() => scrollToSection("appearance")} tooltip="Appearance">
                      <AppearanceIcon />
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
        <ResizeHandle label="Resize menu" handle={sidebarResize.handle} />
      </Sidebar>
      {/* Its own stacking context, so the nebula sits over its background and under everything else. */}
      <SidebarInset className="isolate max-h-svh">
        {/* The pages' background: behind the content area and the single-column
            pages (the list pane draws its own surface over it). */}
        <PageNebula className="absolute inset-0 -z-10" />
        {!isMobileDetailRoute(location.pathname) && (
          <header className="flex h-12 shrink-0 items-center gap-2 px-4 md:hidden">
            <SidebarTrigger />
            <span className="text-sm font-medium">{mobileTitle}</span>
          </header>
        )}
        {/* Master-detail surfaces (sessions, workspaces, profiles, bots)
            manage their own panes and scrolling — full-bleed. Everything else
            gets the centered scrolling column. */}
        <DocumentTitleProvider base={shellTitleTrail(location.pathname, active?.name)}>
          {/\/u\/[^/]+\/(sessions|workspaces|profiles|bots)/.test(location.pathname) ? (
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
