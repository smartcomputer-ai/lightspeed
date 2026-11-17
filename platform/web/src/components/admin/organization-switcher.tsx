import { useState } from "react";
import { Link, useNavigate, useLocation } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import {
  organizationAdminPath,
  organizationLabel,
  useAdminAccess,
} from "@/lib/admin-access";
import { CreateOrganizationDialog } from "@/components/admin/organization-dialogs";
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
} from "@/components/ui/dropdown-menu";
import {
  SidebarMenu,
  SidebarMenuItem,
  SidebarMenuButton,
} from "@/components/ui/sidebar";
import { AddIcon, BackIcon, SwitcherIcon, UserIcon } from "@/icons";

export function OrganizationSwitcher() {
  const access = useAdminAccess();
  const navigate = useNavigate();
  const location = useLocation();
  const client = useQueryClient();
  const [createOpen, setCreateOpen] = useState(false);
  const platformPage = location.pathname.startsWith("/admin/platform/");
  return (
    <>
      <SidebarMenu>
        <SidebarMenuItem>
          <DropdownMenu>
            <DropdownMenuTrigger
              render={
                <SidebarMenuButton
                  size="lg"
                  className="h-(--sidebar-header-button-height)"
                />
              }
            >
              <UserIcon />
              <div className="grid flex-1 text-left leading-tight">
                <span className="truncate text-sm font-medium">
                  {platformPage
                    ? "Platform administration"
                    : access.selected
                      ? organizationLabel(access.selected, access.platform)
                      : "Administration"}
                </span>
                <span className="text-xs text-muted-foreground">
                  {platformPage ? "Deployment-wide" : "Organization"}
                </span>
              </div>
              <SwitcherIcon />
            </DropdownMenuTrigger>
            <DropdownMenuContent
              align="start"
              className="max-h-[70dvh] min-w-56 overflow-y-auto"
            >
              {access.manageable.map((organization) => (
                <DropdownMenuItem
                  key={organization.id}
                  onClick={() =>
                    navigate(
                      `${organizationAdminPath(organization.id)}/universes`,
                    )
                  }
                >
                  {organizationLabel(organization, access.platform)}
                  {!organization.enabled ? " (suspended)" : ""}
                </DropdownMenuItem>
              ))}
              {access.canCreate && (
                <>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem onClick={() => setCreateOpen(true)}>
                    <AddIcon />
                    New organization
                  </DropdownMenuItem>
                </>
              )}
              <DropdownMenuSeparator />
              <DropdownMenuItem render={<Link to="/" />}>
                <BackIcon />
                Back to universes
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </SidebarMenuItem>
      </SidebarMenu>
      {createOpen && (
        <CreateOrganizationDialog
          close={() => setCreateOpen(false)}
          saved={async (id) => {
            await client.invalidateQueries({ queryKey: ["organizations"] });
            setCreateOpen(false);
            navigate(`${organizationAdminPath(id)}/universes`);
          }}
        />
      )}
    </>
  );
}
