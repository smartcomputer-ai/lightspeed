import type {
  AccessInput,
  ExecutionInput,
  Visibility,
} from "@lightspeed-ai/agent-client";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { SettingsDisclosure } from "@/components/ui/settings-disclosure";
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

/// Sessions and bots grant reading; workspaces, environments and MCP servers
/// grant use. The words match the Access dialog.
export type CreationAudience = "read" | "use";

const audienceLabel: Record<CreationAudience, string> = {
  read: "Who can read",
  use: "Who can use",
};

/// The audience of a new root in words, e.g. "Universe members can use it".
export function creationAudienceSentence(
  visibility: Visibility,
  audience: CreationAudience,
): string {
  return `${visibility === "universe" ? "Universe members" : "Only you and people you share with"} can ${audience} it`;
}

/// A new root's access, closed to one line above the form's submit buttons:
/// "Universe members can use it · Change". Sessions and bots (`read`) also
/// choose the identity they run as. Grants are edited afterwards in the
/// Access dialog.
export function CreationAccessSummary(
  props:
    | {
        audience: "read";
        universeId: string;
        value: CreationAccess;
        onChange: (value: CreationAccess) => void;
        /** Whether to read the universe's execution policy yet. */
        enabled?: boolean;
      }
    | {
        audience: "use";
        value: Visibility;
        onChange: (value: Visibility) => void;
      },
) {
  if (props.audience === "use") {
    return (
      <SettingsDisclosure
        summary={creationAudienceSentence(props.value, "use")}
        action="Change"
        label="Change who can use it"
      >
        <CreationVisibilityField
          audience="use"
          value={props.value}
          onChange={props.onChange}
        />
      </SettingsDisclosure>
    );
  }
  return <ExecutionAccessSummary {...props} />;
}

function ExecutionAccessSummary({
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
  // Personal execution is offered when the universe enables it, and kept
  // visible once chosen.
  const personalOffered =
    policy.data?.policy.personalExecutionEnabled === true ||
    value.kind === "personal";
  return (
    <SettingsDisclosure
      summary={`${creationAudienceSentence(value.visibility, "read")} · Runs as ${
        value.kind === "personal" ? "you" : DEFAULT_AGENT_IDENTITY
      }`}
      action="Change"
      label="Change access and identity"
    >
      <Field>
        <FieldLabel>Running as</FieldLabel>
        {personalOffered ? (
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
              { value: "personal", label: "Me" },
            ]}
          />
        ) : (
          <p className="text-sm">{DEFAULT_AGENT_IDENTITY}</p>
        )}
        <FieldDescription>
          {personalOffered
            ? "Fixed at creation. Personal work stops at its next model call if you lose universe access."
            : "Fixed at creation."}
        </FieldDescription>
        {policy.error && (
          <ReadError
            error={policy.error}
            prefix="Execution options unavailable"
          />
        )}
      </Field>
      <CreationVisibilityField
        audience="read"
        value={value.visibility}
        onChange={(visibility) => onChange({ ...value, visibility })}
      />
    </SettingsDisclosure>
  );
}

/// The audience select itself. Forms whose access is part of a larger
/// disclosure (MCP servers) place it there instead of a summary line.
export function CreationVisibilityField({
  audience,
  value,
  onChange,
}: {
  audience: CreationAudience;
  value: Visibility;
  onChange: (value: Visibility) => void;
}) {
  return (
    <Field>
      <FieldLabel>{audienceLabel[audience]}</FieldLabel>
      <AccessSelect
        label={audienceLabel[audience]}
        value={value}
        onChange={(visibility) => onChange(visibility as Visibility)}
        options={[
          { value: "universe", label: "Universe members" },
          { value: "restricted", label: "Only me and people I share with" },
        ]}
      />
    </Field>
  );
}
