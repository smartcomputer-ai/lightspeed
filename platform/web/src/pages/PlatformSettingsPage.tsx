import { useState, type FormEvent, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { PlatformOperationResult, PlatformSettingKey } from "@lightspeed-ai/platform-shared";
import { platformApi } from "@/lib/platform-api";
import { LockedIcon } from "@/icons";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { SaveButton } from "@/components/ui/save-button";
import { Switch } from "@/components/ui/switch";
import { LoadingNote, PageHeader } from "@/components/page";
import { ReadError } from "@/components/read-error";

type Setting = PlatformOperationResult<"platform/settings/list">["settings"][number];
type Settings = Map<PlatformSettingKey, Setting>;

/// Deployment-wide settings, edited like an organization's: a number saves
/// with its own button, a switch when changed. A setting the deployment's
/// environment holds is shown, locked, with the variable that sets it.
export function PlatformSettingsPage() {
  const settings = useQuery({
    queryKey: ["platform-settings"],
    queryFn: () => platformApi("platform/settings/list", {}),
  });
  const byKey: Settings = new Map((settings.data?.settings ?? []).map((setting) => [setting.key, setting]));
  return (
    <>
      <PageHeader title="Platform settings"
        description="Settings for the whole deployment. A setting fixed in the deployment's environment can only be changed there." />
      {settings.isPending && <LoadingNote />}
      {settings.error && <ReadError error={settings.error} loading={!settings.data} prefix="Settings unavailable" />}
      {settings.data && (
        <div className="grid gap-6">
          <PersonalOrganizationsCard setting={byKey.get("personalOrganizations")!} />
          <UniverseLimitsCard settings={byKey} />
        </div>
      )}
    </>
  );
}

function useSettingPut() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: ({ key, value }: { key: PlatformSettingKey; value: boolean | number | null }) =>
      platformApi("platform/settings/put", { key, value }),
    onSuccess: () => Promise.all([
      client.invalidateQueries({ queryKey: ["platform-settings"] }),
      client.invalidateQueries({ queryKey: ["organizations"] }),
    ]),
  });
}

function SaveError({ error }: { error: Error | null }) {
  return error ? <p role="alert" className="text-sm text-destructive">{error.message}</p> : null;
}

/// Where a value comes from, and the way back to the default when it was set here.
function SettingSource({ setting, format, reset, resetting }: {
  setting: Setting;
  format: (value: Setting["value"]) => string;
  reset: () => void;
  resetting: boolean;
}) {
  if (setting.source === "environment") return (
    <p className="flex items-center gap-1.5 text-sm text-muted-foreground">
      <LockedIcon aria-hidden="true" className="size-3.5 shrink-0" />
      <span>Set by <code className="font-mono text-xs">{setting.environmentVariable}</code> in the deployment's environment.</span>
    </p>
  );
  if (setting.source === "default") return <p className="text-sm text-muted-foreground">Using the default.</p>;
  return (
    <p className="flex flex-wrap items-center gap-x-2 text-sm text-muted-foreground">
      <span>Changed here. The default is {format(setting.defaultValue)}.</span>
      <Button variant="link" size="sm" className="h-auto p-0" disabled={resetting} onClick={reset}>Reset to default</Button>
    </p>
  );
}

function PersonalOrganizationsCard({ setting }: { setting: Setting }) {
  const put = useSettingPut();
  const locked = setting.source === "environment";
  const checked = put.isPending && typeof put.variables?.value === "boolean" ? put.variables.value : setting.value === true;
  return (
    <Card>
      <CardHeader>
        <CardTitle>Personal organizations</CardTitle>
        <CardDescription>Each person can create universes of their own, in an organization only they belong to.</CardDescription>
      </CardHeader>
      <CardContent className="grid gap-3">
        <div className="flex items-start justify-between gap-4">
          <div className="grid gap-0.5">
            <label htmlFor="setting-personalOrganizations" className="text-sm font-medium">Give each person a personal organization</label>
            <p className="text-sm text-muted-foreground">Created when they first sign in. Turning this off stops new ones; existing ones stay.</p>
          </div>
          <Switch id="setting-personalOrganizations" checked={checked} disabled={locked || put.isPending}
            onCheckedChange={(value) => put.mutate({ key: "personalOrganizations", value })} />
        </div>
        <SettingSource setting={setting} format={(value) => (value ? "on" : "off")} resetting={put.isPending}
          reset={() => put.mutate({ key: "personalOrganizations", value: null })} />
        <SaveError error={put.error} />
      </CardContent>
    </Card>
  );
}

function UniverseLimitsCard({ settings }: { settings: Settings }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>Universe limits</CardTitle>
        <CardDescription>
          How many universes an organization may hold. A Platform Admin can give one organization its own limit in its settings.
        </CardDescription>
      </CardHeader>
      <CardContent className="grid gap-6">
        <LimitField setting={settings.get("personalUniverseQuota")!} label="Max universes in personal organizations" />
        <LimitField setting={settings.get("sharedUniverseQuota")!} label="Max universes in shared organizations" />
      </CardContent>
    </Card>
  );
}

function LimitField({ setting, label }: { setting: Setting; label: ReactNode }) {
  const put = useSettingPut();
  const locked = setting.source === "environment";
  const [draft, setDraft] = useState(String(setting.value));
  const value = Number(draft);
  const valid = draft !== "" && Number.isInteger(value) && value >= 0 && value <= 100000;
  const dirty = valid && value !== setting.value;
  const id = `setting-${setting.key}`;
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (dirty) put.mutate({ key: setting.key, value });
  };
  const reset = () => put.mutate({ key: setting.key, value: null }, { onSuccess: ({ setting: updated }) => setDraft(String(updated.value)) });
  return (
    <div className="grid gap-2">
      <form onSubmit={submit} className="flex max-w-md flex-wrap items-end gap-3">
        <Field className="flex-1">
          <FieldLabel htmlFor={id}>{label}</FieldLabel>
          <Input id={id} type="number" min={0} max={100000} step={1} value={draft} disabled={locked}
            onChange={(event) => setDraft(event.target.value)} required />
        </Field>
        {!locked && <SaveButton type="submit" dirty={dirty} saving={put.isPending} error={put.isError} />}
      </form>
      <SettingSource setting={setting} format={String} reset={reset} resetting={put.isPending} />
      <SaveError error={put.error} />
    </div>
  );
}
