import { useState, type FormEvent } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import type { BaseUniverseRole, PlatformOperationParams, PlatformOperationResult } from "@lightspeed-ai/platform-shared";
import { platformApi } from "@/lib/platform-api";
import { useDeploymentPermissions } from "@/lib/permissions";
import { Button } from "@/components/ui/button";
import { ButtonGroup } from "@/components/ui/button-group";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { SaveButton } from "@/components/ui/save-button";
import { Switch } from "@/components/ui/switch";
import { InfoBox, type InfoBoxTone } from "@/components/ui/info-box";

type Organization = PlatformOperationResult<"platform/organizations/read">["organization"];
type OrganizationChange = Omit<PlatformOperationParams<"platform/organizations/update">, "organizationId">;

/// An organization's settings, each edited where it is shown, as a
/// universe's settings are: a text setting saves with its own button, a
/// choice or a switch saves when changed.
export function OrganizationSettings({ organization }: { organization: Organization }) {
  return (
    <div className="grid gap-6">
      {!organization.personal && <NameCard organization={organization} />}
      <UniversesCard organization={organization} />
      {!organization.personal && <DefaultUniverseAccessCard organization={organization} />}
      <ProtectionCard organization={organization} />
    </div>
  );
}

function useOrganizationUpdate(organization: Organization) {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (change: OrganizationChange) =>
      platformApi("platform/organizations/update", { organizationId: organization.id, ...change }),
    onSuccess: () => client.invalidateQueries({ queryKey: ["organizations"] }),
  });
}

function SaveError({ error }: { error: Error | null }) {
  return error ? <p role="alert" className="text-sm text-destructive">{error.message}</p> : null;
}

function NameCard({ organization }: { organization: Organization }) {
  const [name, setName] = useState(organization.name);
  const update = useOrganizationUpdate(organization);
  const trimmed = name.trim();
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (trimmed && trimmed !== organization.name) update.mutate({ name: trimmed });
  };
  return (
    <Card>
      <CardHeader>
        <CardTitle>Name</CardTitle>
        <CardDescription>Shown in the organization switcher and across administration.</CardDescription>
      </CardHeader>
      <CardContent className="grid gap-2">
        <form onSubmit={submit} className="flex max-w-md flex-wrap items-end gap-3">
          <Field className="flex-1">
            <FieldLabel htmlFor="organization-name">Organization name</FieldLabel>
            <Input id="organization-name" value={name} onChange={(event) => setName(event.target.value)} required />
          </Field>
          <SaveButton type="submit" dirty={!!trimmed && trimmed !== organization.name} saving={update.isPending} error={update.isError} />
        </form>
        <SaveError error={update.error} />
      </CardContent>
    </Card>
  );
}

/// Usage is visible to the organization's administrators; the quota is a
/// deployment limit, so only those who may set it can change it here.
function UniversesCard({ organization }: { organization: Organization }) {
  const canSetQuota = useDeploymentPermissions().can("setUniverseQuota");
  const [quota, setQuota] = useState(String(organization.universeQuota));
  const update = useOrganizationUpdate(organization);
  const value = Number(quota);
  const valid = quota !== "" && Number.isInteger(value) && value >= 0 && value <= 100000;
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (valid && value !== organization.universeQuota) update.mutate({ universeQuota: value });
  };
  return (
    <Card>
      <CardHeader>
        <CardTitle>Universes</CardTitle>
        <CardDescription>
          <span className="tabular-nums">{organization.universeCount ?? 0} of {organization.universeQuota}</span> used.
          {" "}Universes being created or deleted count until they are gone.
          {!canSetQuota && " A Platform Admin sets the quota."}
        </CardDescription>
      </CardHeader>
      {canSetQuota && (
        <CardContent className="grid gap-2">
          <form onSubmit={submit} className="flex max-w-md flex-wrap items-end gap-3">
            <Field className="flex-1">
              <FieldLabel htmlFor="organization-quota">Universe quota</FieldLabel>
              <Input id="organization-quota" type="number" min={0} max={100000} step={1} value={quota}
                onChange={(event) => setQuota(event.target.value)} required />
            </Field>
            <SaveButton type="submit" dirty={valid && value !== organization.universeQuota} saving={update.isPending} error={update.isError} />
          </form>
          <SaveError error={update.error} />
        </CardContent>
      )}
    </Card>
  );
}

/// Each choice with what it means for every member, coloured by how much it
/// opens up: reading is a note, acting and configuring are warnings, owning
/// everything is an error-level warning.
const DEFAULT_ACCESS: Array<{ value: BaseUniverseRole | null; label: string; tone: InfoBoxTone; implication: string }> = [
  { value: null, label: "None", tone: "info", implication: "Members see only the universes they are granted." },
  { value: "viewer", label: "Viewer", tone: "info", implication: "Every member can read every universe. Unshared sessions stay private." },
  { value: "contributor", label: "Contributor", tone: "warning", implication: "Every member can start sessions, use bots and change files in every universe." },
  { value: "operator", label: "Operator", tone: "warning", implication: "Every member can configure models, credentials, bots and environments in every universe." },
  { value: "owner", label: "Owner", tone: "error", implication: "Every member can manage access, delete universes and read everyone's unshared sessions." },
];

function DefaultUniverseAccessCard({ organization }: { organization: Organization }) {
  const update = useOrganizationUpdate(organization);
  const current = update.isPending ? update.variables?.baseUniverseRole : organization.baseUniverseRole;
  const selected = DEFAULT_ACCESS.find((option) => option.value === (current ?? null));
  return (
    <Card>
      <CardHeader>
        <CardTitle id="default-universe-access-label">Default universe access</CardTitle>
        <CardDescription>
          The role every member holds in every universe of this organization. Roles granted under a
          universe's Members add to it. Service users get only what they are granted.
        </CardDescription>
      </CardHeader>
      <CardContent className="grid gap-2">
        <ButtonGroup aria-labelledby="default-universe-access-label">
          {DEFAULT_ACCESS.map(({ value, label }) => (
            <Button key={label} aria-pressed={current === value} disabled={update.isPending}
              onClick={() => value !== organization.baseUniverseRole && update.mutate({ baseUniverseRole: value })}>
              {label}
            </Button>
          ))}
        </ButtonGroup>
        {selected && <InfoBox tone={selected.tone} className="max-w-xl">{selected.implication}</InfoBox>}
        <SaveError error={update.error} />
      </CardContent>
    </Card>
  );
}

function ProtectionCard({ organization }: { organization: Organization }) {
  const update = useOrganizationUpdate(organization);
  const settings = [
    {
      key: "defaultProtected" as const,
      label: "Protect new universes",
      description: "New universes start with deletion protection. An Organization Admin can still clear it on one universe unless protection is required.",
    },
    {
      key: "requireProtected" as const,
      label: "Require protection",
      description: "Every universe must stay protected and cannot expire. Protect every existing universe before turning this on.",
    },
  ];
  return (
    <Card>
      <CardHeader>
        <CardTitle>Deletion protection</CardTitle>
        <CardDescription>A protected universe cannot be deleted until an Organization Admin clears its protection.</CardDescription>
      </CardHeader>
      <CardContent className="grid gap-4">
        {settings.map(({ key, label, description }) => {
          const id = `organization-${key}`;
          return (
            <div key={key} className="flex items-start justify-between gap-4">
              <div className="grid gap-0.5">
                <label htmlFor={id} className="text-sm font-medium">{label}</label>
                <p className="text-sm text-muted-foreground">{description}</p>
              </div>
              <Switch id={id} checked={organization[key]} disabled={update.isPending}
                onCheckedChange={(checked) => update.mutate({ [key]: checked })} />
            </div>
          );
        })}
        <SaveError error={update.error} />
      </CardContent>
    </Card>
  );
}
