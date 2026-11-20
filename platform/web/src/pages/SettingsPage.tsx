import { Navigate, useLocation } from "react-router-dom";
import type { SessionUser } from "@/auth";
import { PageHeader } from "@/components/page";
import { AccountSettings } from "@/components/settings/account-settings";
import { AppearanceSettings } from "@/components/settings/appearance-settings";

/// The former profile and preferences pages, now sections of Settings; the
/// section in the address is kept.
export function SettingsRedirect() {
  return <Navigate to={{ pathname: "/settings", hash: useLocation().hash }} replace />;
}

/// Everything that is the signed-in person's own, on one page: the account
/// (kept on the server) and appearance (kept in this browser). The sidebar
/// lists its sections.
export function SettingsPage({ user }: { user: SessionUser }) {
  return (
    <>
      <PageHeader title="Settings" />
      <div className="grid gap-10">
        <section aria-labelledby="settings-account" className="grid gap-4">
          <h2 id="settings-account" className="text-sm font-semibold text-muted-foreground">Account</h2>
          <AccountSettings user={user} />
        </section>
        <section aria-labelledby="settings-appearance" className="grid gap-4">
          <h2 id="settings-appearance" className="text-sm font-semibold text-muted-foreground">Appearance</h2>
          <AppearanceSettings />
        </section>
      </div>
    </>
  );
}
