import { useState } from "react";
import { Navigate } from "react-router-dom";
import { Plus } from "lucide-react";
import { Button } from "@/components/ui/button";
import { NewUniverseDialog } from "@/components/universe-switcher";
import { CenteredNote } from "@/components/page";
import { canManage, lastUniverse, memberships, universeHome, useUniverses } from "@/lib/universes";

/// `/` → the last-visited universe when it still exists, else the first
/// membership, else an empty state.
export function HomeRedirect({ admin }: { admin: boolean }) {
  const universes = useUniverses();
  const [createOpen, setCreateOpen] = useState(false);

  if (universes.isLoading) {
    return null;
  }
  if (universes.error) {
    return <CenteredNote>{universes.error.message}</CenteredNote>;
  }

  const mine = memberships(universes.data);
  const last = lastUniverse();
  const target = mine.find((u) => u.slug === last) ?? mine[0];
  if (target) {
    return <Navigate to={universeHome(target.slug, canManage(target, admin))} replace />;
  }

  return (
    <div className="mx-auto flex min-h-[60svh] max-w-md items-center">
      <div className="grid w-full gap-4 text-center">
        <h1 className="text-xl font-semibold">No universes yet</h1>
        <p className="text-sm text-muted-foreground">
          {admin
            ? "Create your first universe to get started."
            : "You are not a member of any universe yet — ask your admin for an invite."}
        </p>
        {admin && (
          <div>
            <Button onClick={() => setCreateOpen(true)}>
              <Plus data-icon="inline-start" />
              New universe
            </Button>
            <NewUniverseDialog open={createOpen} onOpenChange={setCreateOpen} />
          </div>
        )}
      </div>
    </div>
  );
}
