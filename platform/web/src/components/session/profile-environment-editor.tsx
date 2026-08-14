import { SetupEditorSection } from "@/components/session/setup-editor-section";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

export type EnvironmentOption = {
  environmentId: string;
  displayName?: string | null;
  incarnation: {
    providerTargetId?: string | null;
    templateId?: string | null;
  };
  status?: string;
};

export function ProfileEnvironmentEditor({
  value,
  environments = [],
  disabled = false,
  title = "Initial environment",
  description = "The universe environment to activate when the session starts.",
  emptyLabel = "No initial environment",
  onChange,
}: {
  value?: string | null;
  environments?: EnvironmentOption[];
  disabled?: boolean;
  title?: string;
  description?: string;
  emptyLabel?: string;
  onChange: (environmentId: string | undefined) => void;
}) {
  const none = "__no_profile_environment__";
  const ids = [...new Set([
    ...environments.map((environment) => environment.environmentId),
    ...(value ? [value] : []),
  ])];
  const selected = value
    ? environments.find((environment) => environment.environmentId === value)
    : undefined;
  const unavailable = Boolean(value) && !selected;

  return (
    <SetupEditorSection
      title={title}
      description={description}
    >
      {disabled ? (
        <p className="rounded-lg border border-dashed p-3 text-sm text-muted-foreground">
          Enable the Environments feature in Session config to select an environment.
        </p>
      ) : (
        <Field>
          <FieldLabel>Environment</FieldLabel>
          <Select
            value={value || none}
            onValueChange={(environmentId) =>
              onChange(environmentId === none ? undefined : environmentId as string)
            }
          >
            <SelectTrigger className="w-full">
              <SelectValue>
                {(environmentId: string) => {
                  if (environmentId === none) return emptyLabel;
                  const environment = environments.find(
                    (candidate) => candidate.environmentId === environmentId,
                  );
                  return environment
                    ? environmentLabel(environment)
                    : `${environmentId} (unavailable)`;
                }}
              </SelectValue>
            </SelectTrigger>
            <SelectContent>
              <SelectItem value={none}>{emptyLabel}</SelectItem>
              {ids.map((environmentId) => {
                const environment = environments.find(
                  (candidate) => candidate.environmentId === environmentId,
                );
                return (
                  <SelectItem key={environmentId} value={environmentId}>
                    {environment
                      ? environmentLabel(environment)
                      : `${environmentId} (unavailable)`}
                  </SelectItem>
                );
              })}
            </SelectContent>
          </Select>
          <FieldDescription className={unavailable ? "text-xs text-destructive" : "text-xs"}>
            {unavailable
              ? "This saved environment is no longer available."
              : "Profiles select an existing environment; lifecycle and provisioning are managed separately."}
          </FieldDescription>
        </Field>
      )}
    </SetupEditorSection>
  );
}

function environmentLabel(environment: EnvironmentOption): string {
  return `${environment.displayName
    ?? environment.incarnation.templateId
    ?? environment.incarnation.providerTargetId
    ?? environment.environmentId} (${environment.environmentId})`;
}
