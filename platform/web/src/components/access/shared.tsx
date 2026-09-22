import { usePermissionIdentity } from "@/lib/permissions";
import { useQuery, type QueryClient } from "@tanstack/react-query";
import type {
  AccessSubjectsResponse,
  ResourceRef,
  ResourceAccessSummary,
} from "@lightspeed-ai/agent-client";
import { Lock } from "lucide-react";
import { api } from "@/api";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

export function AccessSelect({
  label,
  value,
  options,
  onChange,
  disabled,
}: {
  label: string;
  value: string;
  options: { value: string; label: string }[];
  onChange: (value: string) => void;
  disabled?: boolean;
}) {
  return (
    <Select
      value={value}
      onValueChange={(next) => {
        if (next !== null) onChange(next);
      }}
      disabled={disabled}
    >
      <SelectTrigger aria-label={label} className="w-full">
        <SelectValue>
          {options.find((option) => option.value === value)?.label ?? value}
        </SelectValue>
      </SelectTrigger>
      <SelectContent>
        {options.map((option) => (
          <SelectItem key={option.value} value={option.value}>
            {option.label}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}

export function useAccessSubjects(
  universeId: string,
  query = "",
  enabled = true,
) {
  const identity = usePermissionIdentity();
  return useQuery({
    queryKey: ["access-subjects", universeId, query, identity],
    enabled,
    queryFn: () =>
      api<AccessSubjectsResponse>(
        "GET",
        `/api/v1/universes/${universeId}/access/subjects?q=${encodeURIComponent(query)}`,
      ),
    staleTime: 30_000,
    retry: false,
  });
}

export function RestrictedMarker({
  access,
}: {
  access?: ResourceAccessSummary;
}) {
  return access?.visibility === "restricted" ? (
    <Lock
      className="size-3 shrink-0 text-muted-foreground"
      aria-label="Restricted access"
      role="img"
    >
      <title>Restricted access</title>
    </Lock>
  ) : null;
}

export function ExecutionLabel({
  access,
  universeId,
}: {
  access?: ResourceAccessSummary;
  universeId: string;
}) {
  const execution = access?.execution;
  const subjects = useAccessSubjects(
    universeId,
    execution?.runAs ?? "",
    !!execution && execution.kind === "personal",
  );
  if (!execution) return null;
  const name =
    execution.kind === "service"
      ? "universe service"
      : (subjects.data?.subjects.find((s) => s.subject.id === execution.runAs)
          ?.displayName ?? execution.runAs);
  return (
    <span
      className="truncate text-xs text-muted-foreground"
      title={`Running as ${name}`}
    >
      Running as {name}
    </span>
  );
}

export function resourceHref(slug: string, resource: ResourceRef): string {
  const section =
    resource.kind === "collection"
      ? "collections"
      : resource.kind === "bot"
        ? "bots"
        : "sessions";
  return `/u/${slug}/${section}/${encodeURIComponent(resource.id)}`;
}

export async function invalidateAccess(
  client: QueryClient,
  universeId: string,
) {
  await Promise.all(
    [
      "access-policy",
      "action-permissions",
      "sessions",
      "session",
      "session-children",
      "bots",
      "bot",
      "bot-state",
      "collections",
      "collection",
    ].map((name) =>
      client.invalidateQueries({
        predicate: (query) =>
          query.queryKey[0] === name && query.queryKey.includes(universeId),
      }),
    ),
  );
}
