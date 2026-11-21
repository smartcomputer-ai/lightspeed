import { useMutation, useQueryClient } from "@tanstack/react-query";
import { platformApi } from "@/lib/platform-api";
import { organizationModulesKey, useOrganizationModules } from "@/lib/organization-modules";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Switch } from "@/components/ui/switch";
import { ReadError } from "@/components/read-error";

export function OrganizationModules({ organizationId }: { organizationId: string }) {
  const client = useQueryClient();
  const catalog = useOrganizationModules(organizationId);
  const update = useMutation({
    mutationFn: (setting: { moduleId: string; enabled: boolean }) =>
      platformApi("platform/organizations/modules/put", { organizationId, ...setting }),
    onSuccess: () => client.invalidateQueries({ queryKey: organizationModulesKey(organizationId) }),
  });
  return (
    <Card>
      <CardHeader>
        <CardTitle>Modules</CardTitle>
        <CardDescription>Choose which module pages are available in this organization’s universes.</CardDescription>
      </CardHeader>
      <CardContent className="grid gap-4">
        {catalog.isLoading && <p role="status">Loading modules…</p>}
        {catalog.error && <ReadError error={catalog.error} loading={false} prefix="Modules unavailable" />}
        {catalog.data?.modules.length === 0 && <p>This distribution offers no modules.</p>}
        {catalog.data?.modules.map(module => (
          <div key={module.id} className="flex items-start justify-between gap-4">
            <div>
              <label htmlFor={`module-${module.id}`} className="text-sm font-medium">{module.name}</label>
              <p id={`module-help-${module.id}`} className="text-sm text-muted-foreground">{module.description}</p>
            </div>
            <Switch id={`module-${module.id}`} aria-describedby={`module-help-${module.id}`}
              checked={module.enabled} disabled={update.isPending}
              onCheckedChange={enabled => update.mutate({ moduleId: module.id, enabled })} />
          </div>
        ))}
        {update.error && <p role="alert" className="text-sm text-destructive">{update.error.message}</p>}
      </CardContent>
    </Card>
  );
}
