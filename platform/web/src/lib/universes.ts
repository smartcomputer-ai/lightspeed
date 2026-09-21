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

/// Deployment administration never supplies universe membership.
export function effectiveRole(universe: Universe, _admin: boolean): string | null {
  return universe.role ?? null;
}
export function canManage(universe: Universe, _admin?: boolean): boolean {
  return universe.role === "admin" || universe.role === "operator";
}
export function canAdminister(universe: Universe, _admin?: boolean): boolean {
  return universe.role === "admin";
}
export function canContribute(universe: Universe, _admin?: boolean): boolean {
  return ["admin", "operator", "contributor"].includes(universe.role ?? "");
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

/// Landing page inside a universe.
export function universeHome(slug: string): string {
  return `/u/${slug}/bots`;
}

export function rememberUniverse(slug: string) {
  localStorage.setItem(LAST_UNIVERSE_KEY, slug);
}

export function lastUniverse(): string | null {
  return localStorage.getItem(LAST_UNIVERSE_KEY);
}
