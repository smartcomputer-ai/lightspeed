import { useQuery } from "@tanstack/react-query";
import type {
  AccessExecutionReadResponse,
  AccessInput,
  CollectionListResponse,
  ExecutionInput,
} from "@lightspeed-ai/agent-client";
import { api } from "@/api";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { ReadError } from "@/components/read-error";
import { useActionPermissions } from "@/lib/permissions";
import { AccessSelect } from "./shared";

export type CreationAccess = {
  collectionId: string;
  kind: "service" | "personal";
  visibility: "universe" | "restricted";
};
export const defaultCreationAccess = (): CreationAccess => ({
  collectionId: "",
  kind: "service",
  visibility: "universe",
});
export function creationAccessInput(value: CreationAccess): {
  access: AccessInput;
  execution?: ExecutionInput;
} {
  return value.collectionId
    ? { access: { root: { kind: "collection", id: value.collectionId } } }
    : {
        access: { visibility: value.visibility },
        execution: { kind: value.kind },
      };
}

export function CreationAccessFields({
  universeId,
  value,
  onChange,
  enabled = true,
  allowCollection = true,
}: {
  universeId: string;
  value: CreationAccess;
  onChange: (value: CreationAccess) => void;
  enabled?: boolean;
  allowCollection?: boolean;
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
  const collections = useQuery({
    queryKey: ["collections", universeId],
    enabled: enabled && allowCollection,
    queryFn: () =>
      api<CollectionListResponse>(
        "GET",
        `/api/v1/universes/${universeId}/collections`,
      ),
    retry: false,
  });
  const permissions = useActionPermissions(
    enabled && allowCollection ? universeId : undefined,
    (collections.data?.collections ?? []).map((c) => ({
      kind: "collection",
      id: c.collectionId,
    })),
  );
  const available = (collections.data?.collections ?? []).filter((c) =>
    permissions.can("control_session", {
      kind: "collection",
      id: c.collectionId,
    }),
  );
  const selected = collections.data?.collections.find(
    (c) => c.collectionId === value.collectionId,
  );
  return (
    <div className="grid gap-4">
      {allowCollection && (
        <Field>
          <FieldLabel>Collection</FieldLabel>
          <AccessSelect
            label="Collection"
            value={value.collectionId}
            onChange={(collectionId) => onChange({ ...value, collectionId })}
            options={[
              { value: "", label: "No collection" },
              ...available.map((c) => ({
                value: c.collectionId,
                label: c.displayName,
              })),
            ]}
          />
          {collections.error && (
            <ReadError
              error={collections.error}
              prefix="Collections unavailable"
            />
          )}
          {permissions.error && (
            <ReadError
              error={permissions.error}
              prefix="Collection permissions unavailable"
            />
          )}
        </Field>
      )}
      {value.collectionId ? (
        <p className="text-sm text-muted-foreground">
          Access and execution come from{" "}
          {selected?.displayName ?? value.collectionId}.{" "}
          {selected &&
            `${selected.access.visibility === "restricted" ? "Restricted access" : "Visible to universe members"}; runs as ${selected.access.execution?.kind === "personal" ? "the collection owner" : "the universe service"}.`}
        </p>
      ) : (
        <>
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
              Fixed at creation. Personal work stops at its next model call if
              you lose universe access.
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
        </>
      )}
    </div>
  );
}
