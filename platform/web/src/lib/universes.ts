import { useQuery } from "@tanstack/react-query";
import { useMatch } from "react-router-dom";
import { FEATURES, type FeatureKey } from "@lightspeed-ai/platform-shared";
import { api, type Universe } from "@/api";
import { SETTINGS, settings } from "@/lib/settings";

/// The server returns effective access, including inherited organization and
/// platform administration. Working navigation filters this list to active universes.
export function useUniverses() {
  return useQuery({
    queryKey: ["universes"],
    queryFn: () => api<Universe[]>("GET", "/api/ui/universes"),
    refetchInterval: 30000,
  });
}

export function memberships(universes: Universe[] | undefined): Universe[] {
  return (universes ?? []).filter((u) => u.role != null && u.status === "active");
}

/// Resolves the working or administrative universe route against the same
/// authorized inventory.
export function useActiveUniverse(): {
  universe: Universe | undefined;
  handle: string | undefined;
  isLoading: boolean;
} {
  const work = useMatch("/u/:handle/*");
  const admin = useMatch("/admin/organizations/:organizationId/universes/:handle/*");
  const handle = work?.params.handle ?? admin?.params.handle;
  const universes = useUniverses();
  return {
    universe: handle ? universes.data?.find((u) => u.status !== "deleting" && u.handle === handle && (!admin || u.organizationId === admin.params.organizationId)) : undefined,
    handle,
    isLoading: universes.isLoading,
  };
}

/// The universe index resolves the first enabled module after loading its organization.
export function universeHome(universe: { handle: string }): string {
  return `/u/${universe.handle}`;
}

/// Whether a feature is on in the active universe. The Platform refuses a
/// switched-off feature's requests itself, so this only decides what to show
/// and which requests are worth making; before the universe loads, the
/// default holds.
export function useFeature(feature: FeatureKey): boolean {
  const { universe } = useActiveUniverse();
  return universe?.features?.[feature] ?? FEATURES[feature].default;
}

export function rememberUniverse(handle: string) {
  settings.set(SETTINGS.lastUniverse, handle).catch(() => {
    // The landing page falls back to the first universe.
  });
}

export function lastUniverse(): string | null {
  return settings.get(SETTINGS.lastUniverse);
}
