import type { ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { AddIcon, DeleteIcon } from "@/icons";
import { api, type AuthGrantOption } from "@/api";
import { Button } from "@/components/ui/button";
import { FieldDescription } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  emptyRequestHeaderRow,
  requestHeaderNeedsCredential,
  type RequestHeaderOwner,
  type RequestHeaderRow,
} from "@/lib/request-headers";

export function headerCredentialLabel(grant: AuthGrantOption): string {
  const name = grant.displayName || grant.subjectHint || grant.grantId;
  return `${name} (${grant.providerId})`;
}

/// The credentials a header may name: active token credentials. The engine
/// refuses any other kind in a header.
export function headerCredentialOptions(grants: AuthGrantOption[]): AuthGrantOption[] {
  return grants
    .filter((grant) => grant.providerKind === "staticToken" && grant.status === "active")
    .slice()
    .sort((left, right) => headerCredentialLabel(left).localeCompare(headerCredentialLabel(right)));
}

/// The universe's header credentials, for a form that has not already read
/// its credentials.
export function useHeaderCredentials(universeId: string): { grants: AuthGrantOption[]; loading: boolean } {
  const authGrants = useQuery({
    queryKey: ["auth-grants", universeId],
    queryFn: () => api<AuthGrantOption[]>("GET", `/api/v1/universes/${universeId}/auth-grants`),
  });
  return { grants: headerCredentialOptions(authGrants.data ?? []), loading: authGrants.isLoading };
}

/// Extra request headers of an MCP server or a model endpoint: each is a
/// plain value or a token credential of this universe. `grants` are the
/// credentials offered, already narrowed by `headerCredentialOptions`.
export function RequestHeaderEditor({
  owner,
  rows,
  grants,
  loading,
  onChange,
  description,
  note,
  namePlaceholder,
}: {
  owner: RequestHeaderOwner;
  rows: RequestHeaderRow[];
  grants: AuthGrantOption[];
  loading: boolean;
  onChange: (rows: RequestHeaderRow[]) => void;
  /// What the headers are sent with.
  description: ReactNode;
  /// Anything only this owner needs to say, after the shared guidance.
  note?: ReactNode;
  namePlaceholder: string;
}) {
  const update = (key: number, patch: Partial<RequestHeaderRow>) =>
    onChange(rows.map((row) => (row.key === key ? { ...row, ...patch } : row)));
  return (
    <div className="grid gap-3">
      <p className="text-xs text-muted-foreground">
        {description} Plain-text values are stored and shown as written, so put secrets in a
        credential; Api-Key and X-Api-Key accept only a credential.{note && <> {note}</>}
      </p>
      {rows.map((row, index) => {
        const position = index + 1;
        const boundAvailable = grants.some((grant) => grant.grantId === row.value);
        const secretName = requestHeaderNeedsCredential(owner, row.name);
        return (
          <div key={row.key} className="grid gap-2 rounded-md border p-3" data-slot="request-header-row">
            <div className="flex items-center gap-2">
              <Input
                aria-label={`Header ${position} name`}
                value={row.name}
                onChange={(event) => update(row.key, { name: event.target.value })}
                placeholder={namePlaceholder}
                className="font-mono"
              />
              <Button
                type="button"
                variant="ghost"
                size="icon"
                aria-label={`Remove header ${position}`}
                onClick={() => onChange(rows.filter((candidate) => candidate.key !== row.key))}
              >
                <DeleteIcon />
              </Button>
            </div>
            <div className="grid gap-2 sm:grid-cols-[10rem_minmax(0,1fr)]">
              <Select
                value={row.source}
                onValueChange={(next) => update(row.key, { source: next as RequestHeaderRow["source"], value: "" })}
              >
                <SelectTrigger className="w-full" aria-label={`Header ${position} source`}>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="literal">Plain text</SelectItem>
                  <SelectItem value="credential">Credential</SelectItem>
                </SelectContent>
              </Select>
              {row.source === "literal" ? (
                <Input
                  aria-label={`Header ${position} value`}
                  value={row.value}
                  onChange={(event) => update(row.key, { value: event.target.value })}
                  placeholder="Not secret"
                  className="font-mono"
                />
              ) : (
                <Select
                  value={row.value || "none"}
                  onValueChange={(next) => update(row.key, { value: next === "none" ? "" : next as string })}
                >
                  <SelectTrigger className="w-full" aria-label={`Header ${position} credential`}>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="none">Choose a credential</SelectItem>
                    {row.value && !boundAvailable && (
                      <SelectItem value={row.value}>{row.value} (unavailable)</SelectItem>
                    )}
                    {grants.map((grant) => (
                      <SelectItem key={grant.grantId} value={grant.grantId}>
                        {headerCredentialLabel(grant)}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              )}
            </div>
            {row.source === "literal" && secretName && (
              <FieldDescription>
                This header carries a secret. Choose Credential so the value is not stored in plain text.
              </FieldDescription>
            )}
            {row.source === "credential" && !loading && grants.length === 0 && !row.value && (
              <FieldDescription>
                Create a token credential on the Credentials page, then return here.
              </FieldDescription>
            )}
          </div>
        );
      })}
      <div>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => onChange([...rows, emptyRequestHeaderRow(rows)])}
        >
          <AddIcon data-icon="inline-start" />Add header
        </Button>
      </div>
    </div>
  );
}
