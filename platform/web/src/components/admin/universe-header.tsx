import { Link } from "react-router-dom";
import type { Universe } from "@/api";
import { PageHeader } from "@/components/page";
import { Button } from "@/components/ui/button";
import { universeHome } from "@/lib/universes";

export function AdminUniverseHeader({
  universe,
  section,
}: {
  universe: Universe;
  section: "access" | "settings";
}) {
  const base = `/admin/universes/${encodeURIComponent(universe.handle)}`;
  return (
    <>
      <Link
        className="mb-3 inline-block text-sm text-muted-foreground hover:underline"
        to="/admin/universes"
      >
        ← Universes
      </Link>
      <PageHeader
        title={universe.name}
        description={`Manage ${universe.handle}.`}
        actions={
          <Button
          nativeButton={false}
            variant="outline"
            render={<Link to={universeHome(universe)} />}
          >
            Open universe
          </Button>
        }
      />
      <nav
        aria-label="Universe administration"
        className="mb-6 flex flex-wrap gap-2"
      >
        <Button
          nativeButton={false}
          variant={section === "access" ? "secondary" : "ghost"}
          render={
            <Link
              to={`${base}/access`}
              aria-current={section === "access" ? "page" : undefined}
            />
          }
        >
          Access
        </Button>
        <Button
          nativeButton={false}
          variant={section === "settings" ? "secondary" : "ghost"}
          render={
            <Link
              to={`${base}/settings`}
              aria-current={section === "settings" ? "page" : undefined}
            />
          }
        >
          Settings and lifecycle
        </Button>
      </nav>
    </>
  );
}
