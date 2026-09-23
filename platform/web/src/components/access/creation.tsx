import { useQuery } from "@tanstack/react-query";
import type {
  AccessExecutionReadResponse,
  AccessInput,
  ExecutionInput,
} from "@lightspeed-ai/agent-client";
import { api } from "@/api";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { ReadError } from "@/components/read-error";
import { AccessSelect } from "./shared";

export type CreationAccess = {
  kind: "service" | "personal";
  visibility: "universe" | "restricted";
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
  const policy = useQuery({
    queryKey: ["execution-policy", universeId],
    enabled,
    queryFn: () =>
      api<AccessExecutionReadResponse>(
        "GET",
        `/api/v1/universes/${universeId}/access/execution`,
      ),
    retry: false,
  });
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
            { value: "service", label: "Universe service" },
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
      <Field>
        <FieldLabel>Who can read</FieldLabel>
        <AccessSelect
          label="Who can read"
          value={value.visibility}
          onChange={(visibility) =>
            onChange({
              ...value,
              visibility: visibility as CreationAccess["visibility"],
            })
          }
          options={[
            { value: "universe", label: "Universe members" },
            {
              value: "restricted",
              label: "Only me and people I share with",
            },
          ]}
        />
      </Field>
    </div>
  );
}
