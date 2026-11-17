import { organizationAllows, useDeploymentPermissions } from "@/lib/permissions";
import { organizationLabel } from "@/lib/admin-access";
import { useOrganizations } from "@/lib/platform-api";
import { useEffect, useRef, useState, type RefObject } from "react";
import { useNavigate } from "react-router-dom";
import { AddIcon, SelectedIcon, SwitcherIcon } from "@/icons";
import { UniverseIcon } from "@/components/universe-icon";
import { NewUniverseDialog } from "@/components/new-universe-dialog";
import { Starscape } from "@/components/starscape/starscape";
import { useIsDark } from "@/lib/theme";
import { useReducedMotion } from "@/lib/reduced-motion";
import { SETTINGS, useSetting } from "@/lib/settings";
import type { Universe } from "@/api";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import {
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  sidebarMenuButtonVariants,
} from "@/components/ui/sidebar";
import { cn } from "@/lib/utils";
import { memberships, universeHome, useUniverses } from "@/lib/universes";

/// The sky behind the universe icon fades out over this radius, in CSS px.
/// It reaches past the header into the navigation, which the shell keeps
/// stacked above it.
/// The centre is cleared too, so the stars thin out gradually toward the
/// 40px icon: gone within 10px, fully back only by 44px.
export const HEADER_STARSCAPE_FADE = {
  start: 16,
  end: 100,
  inner: { start: 10, end: 44 },
};

/// Stars and streaks only: without a sky behind it the nebula reads as a
/// blue tint on the sidebar, and how much shows changes with every seed.
export const HEADER_STARSCAPE_CONFIG = { nebulaAmount: 0 };

/// Covers the fade circle around the icon.
export const HEADER_STARSCAPE_CLASS = "absolute inset-x-0 top-0 h-32";

/// How long the header's sky stays after an arrival, then how long it takes
/// to fade. The fade's class below spells the same duration out.
export const HEADER_STARSCAPE_HOLD_MS = 2500;
export const HEADER_STARSCAPE_FADE_OUT_MS = 1500;
const FADE_OUT_CLASS = "transition-opacity duration-[1500ms] ease-out";

type ArrivalPhase = "shown" | "fading" | "gone";

/// The last arrival played in this page load. Coming back to the same
/// universe from Settings or Admin remounts the header, which is no arrival.
let playedArrival: string | undefined;

/// Plays an arrival once per value of `arrival`: shown, fading, then gone.
/// A new value before the end starts it over at full brightness.
function useArrival(arrival: string | null | undefined): ArrivalPhase {
  const key = arrival ?? undefined;
  const [state, setState] = useState<{ key: string | undefined; phase: ArrivalPhase }>(() => ({ key, phase: key !== undefined && key === playedArrival ? "gone" : "shown" }));
  useEffect(() => {
    if (key === undefined) return;
    // Mounted again for the arrival already played: stays gone.
    if (key === playedArrival && state.key === key) return;
    playedArrival = key;
    setState({ key, phase: "shown" });
    const fade = window.setTimeout(() => setState({ key, phase: "fading" }), HEADER_STARSCAPE_HOLD_MS);
    const gone = window.setTimeout(() => setState({ key, phase: "gone" }), HEADER_STARSCAPE_HOLD_MS + HEADER_STARSCAPE_FADE_OUT_MS);
    return () => { window.clearTimeout(fade); window.clearTimeout(gone); };
    // Only a new arrival starts it; the phase changes come from the timers.
  }, [key]);
  // During the render that brings a new arrival, before the effect runs.
  return state.key === key ? state.phase : "shown";
}

/// The header's sky, centred on the universe icon and fading out around it,
/// the stars straight on the sidebar. Dark theme only: on a light sidebar
/// the white stars would not show. Not at all with the starscape turned off
/// in Settings.
///
/// With `arrival` (the sidebar passes the active universe, or null without
/// one), it marks arriving: on loading and on each switch it shows, then
/// fades and is removed, and with reduced motion it does not show. Without
/// it (previews), it stays.
export function HeaderStarscape({
  iconRef,
  arrival,
}: {
  iconRef: RefObject<HTMLElement | null>;
  arrival?: string | null;
}) {
  const dark = useIsDark();
  const [shown] = useSetting(SETTINGS.starscape);
  const reduced = useReducedMotion();
  const phase = useArrival(arrival);
  if (!dark || !shown) return null;
  if (arrival !== undefined && (arrival === null || reduced || phase === "gone")) return null;
  return (
    <Starscape
      anchor={iconRef}
      fade={HEADER_STARSCAPE_FADE}
      config={HEADER_STARSCAPE_CONFIG}
      transparentBackground
      className={cn(HEADER_STARSCAPE_CLASS, arrival !== undefined && FADE_OUT_CLASS, phase === "fading" && "opacity-0")}
    />
  );
}

/// The switcher offers search only past this many universes.
const SEARCH_THRESHOLD = 10;

/// All accessible active universes, grouped by their organization.
export function UniverseSwitcher({
  active,
}: {
  active: Universe | undefined;
}) {
  const distinguishPersonal = useDeploymentPermissions().can("administerOrganizations");
  const universes = useUniverses();
  const navigate = useNavigate();
  const [createOpen, setCreateOpen] = useState(false);
  const mine = memberships(universes.data);
  const [search, setSearch] = useState("");
  // A short list is quicker to scan than to search.
  const searchable = mine.length > SEARCH_THRESHOLD;
  const organizations = useOrganizations();
  // Shown to anyone who creates universes somewhere; enabled while one of
  // those organizations is under its universe limit.
  const creates = organizations.data?.organizations.some((organization) =>
    organizationAllows(organization, "createUniverse"),
  );
  const canCreate = organizations.data?.organizations.some((organization) => organization.canCreateUniverse);
  const iconRef = useRef<HTMLSpanElement>(null);
  const organizationName = (id?: string) => {
    const organization = organizations.data?.organizations.find(
      (item) => item.id === id,
    );
    return organization
      ? organizationLabel(organization, distinguishPersonal)
      : "Organization";
  };
  const visible = mine.filter((universe) =>
    `${universe.name} ${organizationName(universe.organizationId)}`
      .toLowerCase()
      .includes(searchable ? search.toLowerCase() : ""),
  );
  const severalOrganizations = new Set(mine.map((universe) => universe.organizationId)).size > 1;
  const groups = [
    ...new Set(visible.map((universe) => universe.organizationId)),
  ].sort((a, b) => organizationName(a).localeCompare(organizationName(b)));

  return (
    <>
      {/* The header's sky: centred on the universe icon, fading out around it. */}
      <HeaderStarscape iconRef={iconRef} arrival={active?.id ?? null} />
      <SidebarMenu className="relative">
        <SidebarMenuItem>
          <DropdownMenu>
            <DropdownMenuTrigger
              render={
                <SidebarMenuButton
                  size="lg"
                  className={cn(
                    SWITCHER_BUTTON_CLASS,
                    "data-popup-open:bg-sidebar-accent data-popup-open:text-sidebar-accent-foreground",
                  )}
                />
              }
            >
              <SwitcherFace
                universe={active}
                iconRef={iconRef}
              />
              <SwitcherIcon className="ml-auto size-4" />
            </DropdownMenuTrigger>
            <DropdownMenuContent
              className="max-h-[75dvh] min-w-56 overflow-y-auto"
              align="start"
            >
              {searchable && (
                <div className="p-2">
                  <Input
                    aria-label="Search universes"
                    placeholder="Search universes…"
                    value={search}
                    onChange={(event) => setSearch(event.target.value)}
                    onKeyDown={(event) => event.stopPropagation()}
                  />
                </div>
              )}
              {groups.map((organizationId) => (
                <DropdownMenuGroup key={organizationId ?? "unassigned"}>
                  {/* One organization needs no heading. */}
                  {severalOrganizations && (
                    <DropdownMenuLabel>
                      {organizationName(organizationId)}
                    </DropdownMenuLabel>
                  )}
                  {visible
                    .filter(
                      (universe) => universe.organizationId === organizationId,
                    )
                    .map((universe) => (
                      <DropdownMenuItem
                        key={universe.id}
                        onClick={() => navigate(universeHome(universe))}
                      >
                        <UniverseIcon
                          icon={universe.icon}
                          iconColor={universe.iconColor}
                          className="size-6 rounded-md"
                        />
                        <span className="truncate">{universe.name}</span>
                        {universe.id === active?.id && (
                          <SelectedIcon className="ml-auto size-4" />
                        )}
                      </DropdownMenuItem>
                    ))}
                </DropdownMenuGroup>
              ))}
              {!visible.length && (
                <DropdownMenuItem disabled>
                  {searchable && search ? "No matching universes" : "No universes yet"}
                </DropdownMenuItem>
              )}
              {creates && (
                <>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem disabled={!canCreate} onClick={() => setCreateOpen(true)}>
                    <AddIcon />
                    <span className="grid">
                      New universe
                      {!canCreate && <span className="text-xs text-muted-foreground">Universe limit reached</span>}
                    </span>
                  </DropdownMenuItem>
                </>
              )}
            </DropdownMenuContent>
          </DropdownMenu>
          <NewUniverseDialog
            key={active?.organizationId}
            defaultOrganizationId={active?.organizationId}
            open={createOpen}
            onOpenChange={setCreateOpen}
          />
        </SidebarMenuItem>
      </SidebarMenu>
    </>
  );
}

/// Half the usual left padding, so the large icon sits near the edge. The
/// icon is taller than the button and reaches into the header's padding, so
/// the button does not clip.
const SWITCHER_BUTTON_CLASS =
  "h-(--sidebar-header-button-height) overflow-visible pl-1";

/// What the switcher's button shows: the universe's icon and name.
function SwitcherFace({
  universe,
  iconRef,
}: {
  universe:
    Pick<Universe, "name" | "icon" | "iconColor" | "status"> | undefined;
  iconRef: RefObject<HTMLSpanElement | null>;
}) {
  return (
    <>
      <UniverseIcon
        ref={iconRef}
        icon={universe?.icon}
        iconColor={universe?.iconColor}
        // The collapsed sidebar's buttons are size-8.
        className="size-11 group-data-[collapsible=icon]:size-8"
      />
      <div className="grid flex-1 text-left leading-tight">
        <span className="truncate text-sm font-medium">
          {universe?.name ?? "Select universe"}
        </span>
        {universe?.status === "archived" && (
          <span className="truncate text-xs text-muted-foreground">
            archived
          </span>
        )}
      </div>
    </>
  );
}

/// The switcher as the sidebar header draws it, sky and all, for a preview
/// of a universe's appearance: without its menu or chevron, and inert. It
/// needs no sidebar around it. Sized as the header (sidebar width, pane bar
/// height); `scale` enlarges it, and the space it takes, by a factor.
export function UniverseSwitcherPreview({
  universe,
  scale = 1,
  className,
}: {
  universe: Pick<Universe, "name" | "icon" | "iconColor" | "status">;
  scale?: number;
  className?: string;
}) {
  const iconRef = useRef<HTMLSpanElement>(null);
  return (
    <div
      inert
      data-universe-switcher-preview=""
      className={cn(
        "pointer-events-none overflow-hidden rounded-lg border bg-sidebar select-none",
        className,
      )}
      style={{
        width: `calc(${SWITCHER_PREVIEW_WIDTH} * ${scale})`,
        height: `calc(var(--pane-bar-height) * ${scale})`,
      }}
    >
      <div
        className="relative isolate flex h-(--pane-bar-height) origin-top-left flex-col justify-center p-(--sidebar-header-padding) text-sidebar-foreground"
        style={{ width: SWITCHER_PREVIEW_WIDTH, transform: `scale(${scale})` }}
      >
        <HeaderStarscape iconRef={iconRef} />
        <div
          className={cn(
            sidebarMenuButtonVariants({ size: "lg" }),
            SWITCHER_BUTTON_CLASS,
            "relative",
          )}
        >
          <SwitcherFace universe={universe} iconRef={iconRef} />
        </div>
      </div>
    </div>
  );
}

/// The sidebar's width, which the preview takes as the header does.
const SWITCHER_PREVIEW_WIDTH = "16rem";
