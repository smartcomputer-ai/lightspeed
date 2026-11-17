import { useAccounts } from "@/lib/platform-api";
import { usePlatformAdmin } from "@/lib/permissions";
import { useUniverses } from "@/lib/universes";

/** Navigation hints only; the server authorizes every account and universe operation. */
export function useAdminAccess() {
  const platform = usePlatformAdmin();
  const accounts = useAccounts();
  const universes = useUniverses();
  const accountAdmin = accounts.data?.accounts.some(account => account.enabled && account.role === "admin") ?? false;
  const universeOwner = universes.data?.some(universe => universe.role === "owner") ?? false;
  const manageAccounts = platform || accountAdmin || accounts.data?.canCreate === true;
  const manageUniverses = platform || accountAdmin || universeOwner;
  const manageKeys = platform || accountAdmin;
  return {
    platform,
    accounts: manageAccounts,
    universes: manageUniverses,
    keys: manageKeys,
    any: manageAccounts || manageUniverses,
    isLoading: !platform && (accounts.isLoading || universes.isLoading),
    error: accounts.error ?? universes.error,
  };
}
