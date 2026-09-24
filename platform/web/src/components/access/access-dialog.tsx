import { PrivilegedReadMarker } from "@/components/access/privileged-read";
import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { Users, X } from "lucide-react";
import type {
  AccessGrantInput,
  AccessPolicyPutResponse,
  AccessPolicyReadResponse,
  AccessPolicyView,
  ResourceAccessSummary,
  ResourceRef,
} from "@lightspeed-ai/agent-client";
import { api, ApiError } from "@/api";
import { useActionPermissions, usePermissionIdentity } from "@/lib/permissions";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogFooter,
} from "@/components/ui/dialog";
import { ReadError } from "@/components/read-error";
import {
  AccessSelect,
  DEFAULT_AGENT_IDENTITY,
  ExecutionLabel,
  invalidateAccess,
  isOperational,
  operationalNoun,
  type OperationalKind,
  RestrictedMarker,
  resourceHref,
  useAccessSubjects,
  useExecutionPolicy,
} from "./shared";

/// What `use` covers on each operational kind; configuring stays with the
/// owner, Operators and Admins.
const useCovers: Record<OperationalKind, string> = {
  workspace: "Can use lets a person or session read and change its files.",
  environment:
    "Can use covers its files, commands, jobs and the credentials bound to it.",
  mcp_server: "Can use lets a person or session call its allowed tools.",
};

export function AccessButton({
  universeId,
  slug,
  resource,
  access,
  compact = false,
}: {
  universeId: string;
  slug: string;
  resource: ResourceRef;
  access?: ResourceAccessSummary;
  /** Icon only, for dense rows. */
  compact?: boolean;
}) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <Button
        variant="ghost"
        size={compact ? "icon-sm" : "sm"}
        className="shrink-0 text-muted-foreground"
        onClick={() => setOpen(true)}
        aria-label="Access and sharing"
        title={compact ? "Access" : undefined}
      >
        {access?.visibility === "restricted" ? (
          <RestrictedMarker access={access} />
        ) : (
          <Users />
        )}
        {!compact && <span className="hidden sm:inline">Access</span>}
      </Button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-lg">
          <DialogHeader>
            <DialogTitle>Access</DialogTitle>
            <DialogDescription>
              {isOperational(resource.kind)
                ? `Who can see and use this ${operationalNoun[resource.kind]}, and who owns it.`
                : "Who can read this work, who can control it, and who it runs as."}
            </DialogDescription>
          </DialogHeader>
          {open && (
            <AccessDialogBody
              universeId={universeId}
              slug={slug}
              resource={resource}
              onClose={() => setOpen(false)}
            />
          )}
        </DialogContent>
      </Dialog>
    </>
  );
}

function AccessDialogBody({
  universeId,
  slug,
  resource,
  onClose,
}: {
  universeId: string;
  slug: string;
  resource: ResourceRef;
  onClose: () => void;
}) {
  const identity = usePermissionIdentity();
  const policy = useQuery({
    queryKey: [
      "access-policy",
      universeId,
      resource.kind,
      resource.id,
      identity,
    ],
    queryFn: () =>
      api<AccessPolicyReadResponse>(
        "POST",
        `/api/v1/universes/${universeId}/access/policy/read`,
        { resource },
      ),
    retry: false,
    refetchOnWindowFocus: false,
  });
  if (policy.error)
    return <ReadError error={policy.error} prefix="Access unavailable" />;
  if (!policy.data)
    return <p className="text-sm text-muted-foreground">Loading access…</p>;
  return (
    <><PrivilegedReadMarker privileged={policy.data.privilegedRead} />
    <PolicyEditor
      key={policy.data.policy.revision}
      universeId={universeId}
      slug={slug}
      policy={policy.data.policy}
      onClose={onClose}
      reload={() => void policy.refetch()}
    /></>
  );
}

function PolicyEditor({
  universeId,
  slug,
  policy,
  onClose,
  reload,
}: {
  universeId: string;
  slug: string;
  policy: AccessPolicyView;
  onClose: () => void;
  reload: () => void;
}) {
  const client = useQueryClient();
  const permissions = useActionPermissions(universeId, [policy.root]);
  const directory = useAccessSubjects(universeId);
  const [search, setSearch] = useState("");
  const [query, setQuery] = useState("");
  useEffect(() => {
    const timer = setTimeout(() => setQuery(search), 200);
    return () => clearTimeout(timer);
  }, [search]);
  const matches = useAccessSubjects(universeId, query, query.length > 0);
  const [selectedLabels, setSelectedLabels] = useState<Record<string, string>>(
    {},
  );
  const labels = new Map([
    ...Object.entries(selectedLabels),
    ...[
      ...(directory.data?.subjects ?? []),
      ...(matches.data?.subjects ?? []),
    ].map(
      (s) =>
        [`${s.subject.kind}:${s.subject.id}`, s.displayName] as [
          string,
          string,
        ],
    ),
  ]);
  const name = (id: string) => labels.get(`principal:${id}`) ?? id;
  const owner = directory.data?.principalId === policy.owner;
  const writable = permissions.can("share_resource", policy.root);
  const personal = policy.execution?.kind === "personal";
  const kind = policy.root.kind;
  const operational = isOperational(kind);
  const executionPolicy = useExecutionPolicy(universeId);
  const agent = executionPolicy.data?.policy.executionPrincipalId;
  const [visibility, setVisibility] = useState(policy.visibility);
  const [grants, setGrants] = useState<AccessGrantInput[]>(
    policy.grants.map(({ subject, permission }) => ({ subject, permission })),
  );
  const [newOwner, setNewOwner] = useState("");
  const inherited =
    policy.resource.kind !== policy.root.kind ||
    policy.resource.id !== policy.root.id;
  const rootHref = resourceHref(slug, policy.root);
  const dirty =
    visibility !== policy.visibility ||
    newOwner !== "" ||
    JSON.stringify(grants) !==
      JSON.stringify(
        policy.grants.map(({ subject, permission }) => ({
          subject,
          permission,
        })),
      );
  const save = useMutation({
    mutationFn: () =>
      api<AccessPolicyPutResponse>(
        "PUT",
        `/api/v1/universes/${universeId}/access/policy`,
        {
          resource: policy.root,
          visibility,
          grants,
          expectedRevision: policy.revision,
          ...(newOwner ? { owner: newOwner } : {}),
        },
      ),
    onSuccess: async () => {
      await invalidateAccess(client, universeId);
      onClose();
    },
  });
  const conflict = save.error instanceof ApiError && save.error.status === 409;
  const choices =
    (query ? matches.data?.subjects : directory.data?.subjects) ?? [];
  // Granting the default agent identity, or leaving the resource visible to
  // the universe, lets every session running as it use the resource.
  const agentUses =
    operational &&
    agent !== undefined &&
    (visibility === "universe" ||
      grants.some(
        (g) => g.subject.kind === "principal" && g.subject.id === agent,
      ));
  // Under universe visibility grants add nothing: everyone reads a session or
  // bot and every Contributor works in it (personal work: only its owner), and
  // everyone whose role allows it uses a resource. Grants are kept for a later
  // switch back, but only restricted work lists and adds people.
  const everyone = visibility === "universe";
  const listed = !everyone;
  const canAdd = writable && !everyone;
  const addedPermission = operational ? "use" : "read";
  const everyoneSentence = operational
    ? "Everyone in the universe can use this."
    : personal
      ? "Everyone in the universe can read this. It runs as its owner, so only the owner works in it."
      : "Everyone in the universe can read this, and Contributors can work in it.";
  return (
    <div className="grid gap-5">
      {inherited && (
        <div className="rounded-lg border bg-muted/40 p-3 text-sm">
          {rootHref ? (
            <>
              Shared through{" "}
              <Link
                className="underline underline-offset-4"
                to={rootHref}
                onClick={onClose}
              >
                {policy.root.kind} {policy.root.id}
              </Link>
              .
            </>
          ) : (
            "Shared access."
          )}
          <p className="mt-1 text-muted-foreground">
            Changes apply to everything sharing this access.
          </p>
        </div>
      )}
      <div className="grid gap-1 text-sm">
        <span>
          Owned by{" "}
          <span className="font-medium">
            {owner ? "you" : name(policy.owner)}
          </span>
        </span>
        <ExecutionLabel
          universeId={universeId}
          access={{
            root: policy.root,
            owner: policy.owner,
            visibility: policy.visibility,
            execution: policy.execution,
          }}
        />
      </div>
      {permissions.error && (
        <ReadError
          error={permissions.error}
          prefix="Sharing permissions unavailable"
        />
      )}
      <Field>
        <FieldLabel>{operational ? "Who can use" : "Who can read"}</FieldLabel>
        <AccessSelect
          label={operational ? "Who can use" : "Who can read"}
          value={visibility}
          disabled={!writable || save.isPending}
          onChange={(v) => setVisibility(v as typeof visibility)}
          options={[
            { value: "restricted", label: "Only the owner and members below" },
            { value: "universe", label: "Universe members" },
          ]}
        />
      </Field>
      <div className="grid gap-2">
        {listed && <h3 className="text-sm font-medium">Members</h3>}
        {!listed && (
          <p className="text-sm text-muted-foreground">
            {everyoneSentence}
            {writable && (
              <>
                {" "}Choose &ldquo;Only the owner and members below&rdquo; to pick
                who.
              </>
            )}
          </p>
        )}
        {listed && grants.length === 0 && (
          <p className="text-sm text-muted-foreground">
            No additional people or groups.
          </p>
        )}
        {listed && grants.map((grant, index) => {
          const key = `${grant.subject.kind}:${grant.subject.id}`;
          const canEdit =
            writable && (operational || owner || grant.permission !== "write");
          return (
            <div
              key={key}
              className="flex items-center gap-2 rounded-lg border px-3 py-2"
            >
              <div className="min-w-0 flex-1">
                <p className="truncate text-sm" title={grant.subject.id}>
                  {labels.get(key) ?? grant.subject.id}
                </p>
                <p className="text-xs text-muted-foreground">
                  {grant.subject.kind === "group"
                    ? "Group"
                    : grant.subject.id === agent
                      ? "Agent identity"
                      : "Person or service"}
                </p>
              </div>
              {operational ? (
                <span className="text-xs text-muted-foreground">Can use</span>
              ) : canEdit && owner && !personal ? (
                <div className="w-28">
                  <AccessSelect
                    label={`Permission for ${labels.get(key) ?? grant.subject.id}`}
                    value={grant.permission}
                    disabled={save.isPending}
                    onChange={(permission) =>
                      setGrants(
                        grants.map((g, i) =>
                          i === index
                            ? {
                                ...g,
                                permission: permission as "read" | "write",
                              }
                            : g,
                        ),
                      )
                    }
                    options={[
                      { value: "read", label: "Can read" },
                      { value: "write", label: "Can control" },
                    ]}
                  />
                </div>
              ) : (
                <span className="text-xs text-muted-foreground">
                  {grant.permission === "write" ? "Can control" : "Can read"}
                </span>
              )}
              {canEdit && (
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-sm"
                  disabled={save.isPending}
                  aria-label={`Remove ${labels.get(key) ?? grant.subject.id}`}
                  onClick={() =>
                    setGrants(grants.filter((_, i) => i !== index))
                  }
                >
                  <X />
                </Button>
              )}
            </div>
          );
        })}
        {agentUses && (
          <p className="text-sm text-muted-foreground">
            Every session and bot running as {DEFAULT_AGENT_IDENTITY} can use
            this.
          </p>
        )}
        {operational && (
          <p className="text-xs text-muted-foreground">
            {useCovers[kind]} Configuring it stays with its owner, Operators
            and Admins.
          </p>
        )}
      </div>
      {canAdd && (
        <Field>
          <FieldLabel htmlFor="share-search">Add people or groups</FieldLabel>
          <Input
            id="share-search"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Search universe members…"
            maxLength={100}
            disabled={save.isPending}
          />
          <div className="max-h-36 overflow-y-auto rounded-lg border">
            {choices
              .filter(
                (s) =>
                  s.subject.id !== policy.owner &&
                  // The agent identity only ever uses resources; it never
                  // reads or controls sessions and bots.
                  (operational || s.subject.id !== agent) &&
                  !grants.some(
                    (g) =>
                      g.subject.kind === s.subject.kind &&
                      g.subject.id === s.subject.id,
                  ),
              )
              .map((s) => (
                <button
                  type="button"
                  disabled={save.isPending}
                  className="flex w-full items-center justify-between gap-2 px-3 py-2 text-left text-sm hover:bg-muted disabled:opacity-50"
                  key={`${s.subject.kind}:${s.subject.id}`}
                  onClick={() => {
                    setSelectedLabels({
                      ...selectedLabels,
                      [`${s.subject.kind}:${s.subject.id}`]: s.displayName,
                    });
                    setGrants([
                      ...grants,
                      {
                        subject: s.subject,
                        permission: addedPermission,
                      },
                    ]);
                    setSearch("");
                  }}
                >
                  <span>{s.displayName}</span>
                  <span className="text-xs text-muted-foreground">
                    {s.subject.kind === "group" ? "Group · Add" : "Add"}
                  </span>
                </button>
              ))}
            {choices.length === 0 && (
              <p className="p-3 text-sm text-muted-foreground">
                {directory.isLoading || matches.isFetching
                  ? "Searching…"
                  : "No matching members."}
              </p>
            )}
          </div>
          {(directory.error || matches.error) && (
            <ReadError
              error={directory.error ?? matches.error}
              prefix="Member search unavailable"
            />
          )}
          {!operational && (
            <FieldDescription>
              {personal
                ? "Personal work is shared as read-only here."
                : "Can control allows starting runs under this work's execution identity."}
            </FieldDescription>
          )}
        </Field>
      )}
      {owner && !personal && writable && (
        <details className="text-sm">
          <summary className="cursor-pointer text-muted-foreground">
            Transfer ownership
          </summary>
          <div className="mt-3 grid gap-2">
            <AccessSelect
              label="New owner"
              value={newOwner}
              disabled={save.isPending}
              onChange={setNewOwner}
              options={[
                { value: "", label: "Keep current owner" },
                ...choices
                  .filter(
                    (s) =>
                      s.subject.kind === "principal" &&
                      s.subject.id !== policy.owner &&
                      s.subject.id !== agent,
                  )
                  .map((s) => ({ value: s.subject.id, label: s.displayName })),
              ]}
            />
            <p className="text-xs text-muted-foreground">
              {operational
                ? "You retain only explicit grants. Search above to find another member."
                : "Ownership changes for everything sharing this access. You retain only explicit grants. Execution stays unchanged. Search above to find another member."}
            </p>
          </div>
        </details>
      )}
      {save.error && (
        <div role="alert" className="grid gap-2 text-sm text-destructive">
          {conflict
            ? "Access changed while you were editing. Reload the current policy before saving again."
            : save.error.message}
          {conflict && (
            <Button variant="outline" onClick={reload}>
              Discard edits and reload
            </Button>
          )}
        </div>
      )}
      <DialogFooter>
        <Button variant="outline" onClick={onClose} disabled={save.isPending}>
          {writable ? "Cancel" : "Done"}
        </Button>
        {writable && (
          <Button
            disabled={!dirty || save.isPending || conflict}
            onClick={() => save.mutate()}
          >
            {save.isPending ? "Saving…" : "Save"}
          </Button>
        )}
      </DialogFooter>
    </div>
  );
}
