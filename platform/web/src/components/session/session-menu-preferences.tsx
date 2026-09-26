import { DropdownMenuCheckboxItem, DropdownMenuGroup } from "@/components/ui/dropdown-menu";
import { useUserPreferences } from "@/lib/user-preferences";

/** Shared account-wide display controls in session and bot conversation menus. */
export function SessionMenuPreferences() {
  const preferences = useUserPreferences();
  return (
    <DropdownMenuGroup>
        <DropdownMenuCheckboxItem
          checked={preferences.collapseCompletedRuns}
          onCheckedChange={preferences.setCollapseCompletedRuns}
          closeOnClick={false}
        >
          Collapse completed runs
        </DropdownMenuCheckboxItem>
        <DropdownMenuCheckboxItem
          checked={preferences.showRunStatistics}
          onCheckedChange={preferences.setShowRunStatistics}
          closeOnClick={false}
        >
          Show run statistics
        </DropdownMenuCheckboxItem>
    </DropdownMenuGroup>
  );
}
