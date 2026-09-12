import { SetupEditorSection } from "@/components/session/setup-editor-section";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import type { ProfileEnvironment } from "@/api";
import { isTerminalEnvironmentStatus, selectableEnvironments } from "@/lib/sessions/resource-features";

export type EnvironmentOption = {
  environmentId: string;
  displayName?: string | null;
  incarnation: {
    providerTargetId?: string | null;
    templateId?: string | null;
  };
  status?: string;
  /// Present on core environment views; registered environments show their
  /// identity mode so an ephemeral pick can be flagged.
  source?: { type: string; identityMode?: string };
};

function isEphemeralRegistered(environment: EnvironmentOption | undefined): boolean {
  return environment?.source?.type === "registered" && environment.source.identityMode === "ephemeral";
}

type Mode = "none" | "existing" | "inherit";

const NONE = "__no_profile_environment__";

/// Profile environment intent: leave the session's selection alone,
/// activate an existing universe environment, or inherit the parent’s selection.
export function ProfileEnvironmentEditor({
  value,
  environments: allEnvironments = [],
  disabled = false,
  embedded = false,
  title = "Environment",
  description = "How the session obtains its active environment when this profile is applied.",
  onChange,
}: {
  value?: ProfileEnvironment | null;
  environments?: EnvironmentOption[];
  disabled?: boolean;
  /** Render inside the Environments capability panel instead of as its own section. */
  embedded?: boolean;
  title?: string;
  description?: string;
  onChange: (environment: ProfileEnvironment | undefined) => void;
}) {
  const mode: Mode = value?.type ?? "none";
  const environments = selectableEnvironments(
    allEnvironments,
    value?.type === "existing" ? value.environmentId : undefined,
  );
  const content = (
    <>
      {disabled ? (
        <p className="rounded-lg border border-dashed p-3 text-sm text-muted-foreground">
          Enable Environment access above to select an existing environment.
        </p>
      ) : (
        <div className="space-y-3">
          <Field>
            <FieldLabel>Mode</FieldLabel>
            <Select
              value={mode}
              onValueChange={(next) => {
                const nextMode = next as Mode;
                if (nextMode === "none") onChange(undefined);
                else if (nextMode === "inherit") onChange({ type: "inherit" });
                else if (nextMode === "existing") {
                  onChange({ type: "existing", environmentId: environments[0]?.environmentId ?? "" });
                }
              }}
            >
              <SelectTrigger className="w-full">
                <SelectValue>
                  {(current: string) => modeLabel(current as Mode)}
                </SelectValue>
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="none">{modeLabel("none")}</SelectItem>
                <SelectItem value="existing">{modeLabel("existing")}</SelectItem>
                <SelectItem value="inherit">{modeLabel("inherit")}</SelectItem>
              </SelectContent>
            </Select>
          </Field>

          {value?.type === "existing" && (
            <ExistingEnvironmentField
              value={value.environmentId}
              environments={environments}
              onChange={(environmentId) =>
                onChange(environmentId ? { type: "existing", environmentId } : undefined)
              }
            />
          )}

          {value?.type === "inherit" && (
            <FieldDescription className="text-xs">
              Applied only when this profile runs as a sub-agent: the child activates the delegating
              parent's environment (shared, never copied, never closed by the child).
            </FieldDescription>
          )}

        </div>
      )}
    </>
  );

  if (embedded) {
    return (
      <div className="grid min-w-0 max-w-full gap-3">
        <div className="grid min-w-0 gap-0.5">
          <p className="text-sm font-medium">Session environment</p>
          <p className="text-xs text-muted-foreground">{description}</p>
        </div>
        {content}
      </div>
    );
  }

  return (
    <SetupEditorSection title={title} description={description}>
      {content}
    </SetupEditorSection>
  );
}

function modeLabel(mode: Mode): string {
  switch (mode) {
    case "none":
      return "Do not change the active environment";
    case "existing":
      return "Activate an existing environment";
    case "inherit":
      return "Inherit the parent's active environment (sub-agents only)";
  }
}

function ExistingEnvironmentField({
  value,
  environments,
  onChange,
}: {
  value: string;
  environments: EnvironmentOption[];
  onChange: (environmentId: string | undefined) => void;
}) {
  const ids = [...new Set([
    ...environments.map((environment) => environment.environmentId),
    ...(value ? [value] : []),
  ])];
  const selected = value
    ? environments.find((environment) => environment.environmentId === value)
    : undefined;
  const unavailable = Boolean(value) && !selected;
  const closed = isTerminalEnvironmentStatus(selected?.status);
  return (
    <Field>
      <FieldLabel>Environment</FieldLabel>
      <Select
        value={value || NONE}
        onValueChange={(environmentId) =>
          onChange(environmentId === NONE ? undefined : environmentId as string)
        }
      >
        <SelectTrigger className="w-full">
          <SelectValue>
            {(environmentId: string) => {
              if (environmentId === NONE) return "Select an environment";
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
          <SelectItem value={NONE}>Select an environment</SelectItem>
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
      <FieldDescription className={unavailable || closed ? "text-xs text-destructive" : "text-xs"}>
        {unavailable
          ? "This saved environment is no longer available."
          : closed
            ? "This saved environment is closed and can no longer be activated."
            : isEphemeralRegistered(selected)
              ? "This is an ephemeral registered environment: it closes on its own once its daemon has been away longer than its key's disconnect grace, and sessions that name it will then fail to start. Prefer a persistent key for anything a profile or bot points at."
              : "The profile activates this environment and never closes it; a bot's sessions share it this way. Whether it sleeps while idle is the environment's own idle policy, set on the Environments page. Closed environments are not offered."}
      </FieldDescription>
    </Field>
  );
}

function environmentLabel(environment: EnvironmentOption): string {
  const status = environment.status && environment.status !== "ready" ? ` — ${environment.status}` : "";
  return `${environment.displayName
    ?? environment.incarnation.templateId
    ?? environment.incarnation.providerTargetId
    ?? environment.environmentId} (${environment.environmentId})${status}`;
}
