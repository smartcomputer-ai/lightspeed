import { useOrganizations } from "@/lib/platform-api";
import { organizationAllows } from "@/lib/permissions";
import { useState } from "react";
import { Navigate } from "react-router-dom";
import { AddIcon } from "@/icons";
import { Button } from "@/components/ui/button";
import { NewUniverseDialog } from "@/components/new-universe-dialog";
import { CenteredNote } from "@/components/page";
import { lastUniverse, memberships, universeHome, useUniverses } from "@/lib/universes";

/// `/` → the last-visited universe when it still exists, else the first
/// membership, else an empty state.
export function HomeRedirect() {
  const universes = useUniverses();
  const organizations = useOrganizations();
  const creates = organizations.data?.organizations.some(organization => organizationAllows(organization, "createUniverse"));
  const canCreate = organizations.data?.organizations.some(organization => organization.canCreateUniverse);
  const [createOpen, setCreateOpen] = useState(false);

  if (universes.isLoading) {
    return null;
  }
  if (universes.error) {
    return <CenteredNote>{universes.error.message}</CenteredNote>;
  }

  const mine = memberships(universes.data);
  const last = lastUniverse();
  const target = mine.find((u) => u.handle === last) ?? mine[0];
  if (target) {
    return <Navigate to={universeHome(target)} replace />;
  }

  return (
    <div className="mx-auto flex min-h-[60svh] max-w-md items-center">
      <div className="grid w-full gap-4 text-center">
        <h1 className="text-xl font-semibold">No universes yet</h1>
        <p className="text-sm text-muted-foreground">
          {canCreate
            ? "Create your first universe to get started."
            : creates
              ? "Your organizations have reached their universe limit. Ask a Platform Admin to raise it."
              : "You have not been added to a universe yet. Ask an organization admin or universe owner to add you."}
        </p>
        {canCreate && (
          <div>
            <Button onClick={() => setCreateOpen(true)}>
              <AddIcon data-icon="inline-start" />
              New universe
            </Button>
            <NewUniverseDialog open={createOpen} onOpenChange={setCreateOpen} />
          </div>
        )}
      </div>
    </div>
  );
}
