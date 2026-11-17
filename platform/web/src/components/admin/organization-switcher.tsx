import { useState } from "react";
import { Link, useNavigate } from "react-router-dom";
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
  SidebarGroupLabel,
} from "@/components/ui/sidebar";
import { AddIcon, BackIcon, SwitcherIcon, UserIcon } from "@/icons";

export function OrganizationSwitcher() {
  const access = useAdminAccess();
  const navigate = useNavigate();
  const client = useQueryClient();
  const [createOpen, setCreateOpen] = useState(false);
  return (
    <>
      <SidebarMenu>
        <SidebarMenuItem>
          <SidebarMenuButton render={<Link to="/" />} tooltip="Back to universes">
            <BackIcon />
            <span>Back to universes</span>
          </SidebarMenuButton>
        </SidebarMenuItem>
      </SidebarMenu>
      <SidebarGroupLabel render={<h2 />}>Organizations</SidebarGroupLabel>
      <SidebarMenu>
        <SidebarMenuItem>
          <DropdownMenu>
            <DropdownMenuTrigger
              render={
                <SidebarMenuButton
                  size="lg"
                  className="h-(--sidebar-header-button-height)"
                  tooltip="Organizations"
                />
              }
            >
              <UserIcon />
              <div className="grid flex-1 text-left leading-tight">
                <span className="truncate text-sm font-medium">
                  {access.selected
                      ? organizationLabel(access.selected, access.platform)
                      : "Select organization"}
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
