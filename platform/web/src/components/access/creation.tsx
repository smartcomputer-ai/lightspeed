import type {
  AccessInput,
  ExecutionInput,
  Visibility,
} from "@lightspeed-ai/agent-client";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { ReadError } from "@/components/read-error";
import { AccessSelect, DEFAULT_AGENT_IDENTITY, useExecutionPolicy } from "./shared";

export type CreationAccess = {
  kind: "service" | "personal";
  visibility: Visibility;
};
export const defaultCreationAccess = (): CreationAccess => ({
  kind: "service",
  visibility: "universe",
});
export function creationAccessInput(value: CreationAccess): {
  access: AccessInput;
  execution: ExecutionInput;
} {
  return {
    access: { visibility: value.visibility },
    execution: { kind: value.kind },
  };
}

export function CreationAccessFields({
  universeId,
  value,
  onChange,
  enabled = true,
}: {
  universeId: string;
  value: CreationAccess;
  onChange: (value: CreationAccess) => void;
  enabled?: boolean;
}) {
  const policy = useExecutionPolicy(universeId, enabled);
  return (
    <div className="grid gap-4">
      <Field>
        <FieldLabel>Running as</FieldLabel>
        <AccessSelect
          label="Running as"
          value={value.kind}
          onChange={(kind) =>
            onChange({
              ...value,
              kind: kind as CreationAccess["kind"],
              visibility: kind === "personal" ? "restricted" : "universe",
            })
          }
          options={[
            { value: "service", label: DEFAULT_AGENT_IDENTITY },
            ...(policy.data?.policy.personalExecutionEnabled ||
            value.kind === "personal"
              ? [{ value: "personal", label: "Me" }]
              : []),
          ]}
        />
        <FieldDescription>
          Fixed at creation. Personal work stops at its next model call if you
          lose universe access.
        </FieldDescription>
        {policy.error && (
          <ReadError
            error={policy.error}
            prefix="Execution options unavailable"
          />
        )}
      </Field>
      <CreationVisibilityField
        label="Who can read"
        value={value.visibility}
        onChange={(visibility) => onChange({ ...value, visibility })}
      />
    </div>
  );
}

/// The audience of a new root. Grants are edited afterwards in the Access
/// dialog. Workspaces, environments and MCP servers pass their "Who can use"
/// label.
export function CreationVisibilityField({
  label,
  value,
  onChange,
  disabled,
}: {
  label: string;
  value: Visibility;
  onChange: (value: Visibility) => void;
  disabled?: boolean;
}) {
  return (
    <Field>
      <FieldLabel>{label}</FieldLabel>
      <AccessSelect
        label={label}
        value={value}
        disabled={disabled}
        onChange={(visibility) => onChange(visibility as Visibility)}
        options={[
          { value: "universe", label: "Universe members" },
          { value: "restricted", label: "Only me and people I share with" },
        ]}
      />
    </Field>
  );
}
