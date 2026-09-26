import { useActionPermissions } from "@/lib/permissions";
import { useEffect, type ComponentType, type CSSProperties } from "react";
import { Link, NavLink, Outlet, useLocation, useMatch } from "react-router-dom";
import {
  ArrowLeft,
  Globe,
  KeyRound,
  Palette,
  RadioTower,
  ServerCog,
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
import { UNIVERSE_NAV } from "@/components/universe-nav";
import { ResizeHandle, useResizableWidth } from "@/components/resize-handle";
import { useUserPreferences } from "@/lib/user-preferences";
import { cn } from "@/lib/utils";
import { isMobileDetailRoute } from "@/lib/shell-navigation";
import type { SessionUser } from "@/auth";
import { rememberUniverse, useUniverses } from "@/lib/universes";

/// The sidebar has three modes. Universe mode is the app's top level:
/// switcher + universe nav. Admin and account are modes *above* the
/// universe structure — the header becomes a deliberately generic "Back"
/// (naming no universe) and the content is the mode's own menu.
type ShellMode = "universe" | "admin" | "account";

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
        <SidebarMenuButton size="lg" render={<Link to="/" />} tooltip="Back">
          <div className="flex size-8 shrink-0 items-center justify-center rounded-lg border">
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
      ? "Platform admin"
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
        <SidebarHeader>
          {mode === "universe" ? (
            <UniverseSwitcher active={active} admin={admin} />
          ) : (
            <ModeHeader title={mode === "admin" ? "Platform admin" : "Your account"} />
          )}
        </SidebarHeader>
        <SidebarContent>
          {mode === "universe" && active && UNIVERSE_NAV.map((group, index) => {
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
              <SidebarGroupLabel>Platform admin</SidebarGroupLabel>
              <SidebarGroupContent>
                <SidebarMenu>
                  <NavItem to="/admin/users" icon={UserCog} label="Users" />
                  <NavItem to="/admin/universes" icon={Globe} label="Universes" />
                  <NavItem to="/admin/api-keys" icon={KeyRound} label="API keys" />
                  <NavItem to="/admin/channels" icon={RadioTower} label="Channels" />
                  <NavItem
                    to="/admin/environment-providers"
                    icon={ServerCog}
                    label="Environment providers"
                  />
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
                    <SidebarMenuButton onClick={() => scrollToSection("profile")} tooltip="Profile">
                      <UserRound />
                      <span>Profile</span>
                    </SidebarMenuButton>
                  </SidebarMenuItem>
                  <SidebarMenuItem>
                    <SidebarMenuButton onClick={() => scrollToSection("security")} tooltip="Security">
                      <KeyRound />
                      <span>Security</span>
                    </SidebarMenuButton>
                  </SidebarMenuItem>
                  <SidebarMenuItem>
                    <SidebarMenuButton onClick={() => scrollToSection("appearance")} tooltip="Appearance">
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
        <ResizeHandle label="Resize menu" handle={sidebarResize.handle} />
      </Sidebar>
      <SidebarInset className="max-h-svh">
        {!isMobileDetailRoute(location.pathname) && (
          <header className="flex h-12 shrink-0 items-center gap-2 px-4 md:hidden">
            <SidebarTrigger />
            <span className="text-sm font-medium">{mobileTitle}</span>
          </header>
        )}
        {/* Master-detail surfaces (sessions, workspaces, profiles, bots)
            manage their own panes and scrolling — full-bleed. Everything else
            gets the centered scrolling column. */}
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
      </SidebarInset>
    </SidebarProvider>
  );
}
