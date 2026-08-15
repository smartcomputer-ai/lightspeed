import { useQuery } from "@tanstack/react-query";
import { useParams } from "react-router-dom";
import { api, type Universe } from "@/api";

const LAST_UNIVERSE_KEY = "lightspeed:last-universe";

/// One list drives everything: memberships for the switcher, all universes
/// for platform admins (role marks the ones they are actually members of).
export function useUniverses() {
  return useQuery({
    queryKey: ["universes"],
    queryFn: () => api<Universe[]>("GET", "/api/v1/universes"),
  });
}

export function memberships(universes: Universe[] | undefined): Universe[] {
  return (universes ?? []).filter((u) => u.role != null && u.status === "active");
}

/// Effective role in a universe: membership role, else platform-admin for
/// admins browsing foreign universes (the API only returns those to admins).
export function effectiveRole(universe: Universe, admin: boolean): string | null {
  return universe.role ?? (admin ? "platform-admin" : null);
}

export function canManage(universe: Universe, admin: boolean): boolean {
  const role = effectiveRole(universe, admin);
  return role === "owner" || role === "admin" || role === "platform-admin";
}

/// Resolves the /u/:slug route param against the loaded list.
export function useActiveUniverse(): {
  universe: Universe | undefined;
  slug: string | undefined;
  isLoading: boolean;
} {
  const { slug } = useParams<{ slug: string }>();
  const universes = useUniverses();
  return {
    universe: slug ? universes.data?.find((u) => u.slug === slug) : undefined,
    slug,
    isLoading: universes.isLoading,
  };
}

/// Landing page inside a universe: the Sessions browser for people who
/// can manage it, channels for members (sessions are owner/admin —
/// transcripts can span the whole universe).
export function universeHome(slug: string, manage: boolean): string {
  return manage ? `/u/${slug}/sessions` : `/u/${slug}/settings/channels`;
}

export function rememberUniverse(slug: string) {
  localStorage.setItem(LAST_UNIVERSE_KEY, slug);
}

export function lastUniverse(): string | null {
  return localStorage.getItem(LAST_UNIVERSE_KEY);
}
