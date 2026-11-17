import { useId, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { platformApi, useAccounts } from "@/lib/platform-api";
import { useUniverses } from "@/lib/universes";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { ApiKeyRevokeDialog } from "@/components/api-key-revoke-dialog";
import { AddIcon } from "@/icons";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from "@/components/ui/dialog";
import { Badge } from "@/components/ui/badge";
import {
  Card,
  CardHeader,
  CardTitle,
  CardDescription,
  CardContent,
  CardAction,
} from "@/components/ui/card";

type Application = {
  accountId: string;
  applicationId: string;
  name?: string;
  enabled?: boolean;
};
type Choice = { id: string; name: string };
const split = (value: string) => [
  ...new Set(
    value
      .split(",")
      .map((v) => v.trim())
      .filter(Boolean),
  ),
];
const toggle = (selected: string[], id: string) =>
  selected.includes(id)
    ? selected.filter((value) => value !== id)
    : [...selected, id];

type ApiKeysCardProps = {
  application?: Application;
  createOpen?: boolean;
  onCreateOpenChange?: (open: boolean) => void;
};
export function ApiKeysCard({ application, ...dialogProps }: ApiKeysCardProps) {
  // Changing the owner discards any displayed secret and unfinished form.
  return (
    <ApiKeysContent
      key={application?.applicationId ?? "personal"}
      application={application}
      {...dialogProps}
    />
  );
}

function ScopeChoices({
  title,
  choices,
  selected,
  onChange,
  empty,
}: {
  title: string;
  choices: Choice[];
  selected: string[];
  onChange: (value: string[]) => void;
  empty: string;
}) {
  const [search, setSearch] = useState("");
  const visible = choices.filter((choice) =>
    choice.name.toLowerCase().includes(search.toLowerCase()),
  );
  return (
    <fieldset className="grid gap-2">
      <legend className="mb-2 text-sm font-medium">{title}</legend>
      {choices.length > 5 && (
        <Input
          aria-label={`Find ${title.toLowerCase()}`}
          placeholder={`Find ${title.toLowerCase()}…`}
          value={search}
          onChange={(event) => setSearch(event.target.value)}
        />
      )}
      <div className="grid max-h-48 gap-2 overflow-y-auto rounded-md border p-3">
        {visible.map((choice) => (
          <label key={choice.id} className="flex items-start gap-2 text-sm">
            <input
              type="checkbox"
              className="mt-1"
              checked={selected.includes(choice.id)}
              onChange={() => onChange(toggle(selected, choice.id))}
            />
            <span>{choice.name}</span>
          </label>
        ))}
        {!visible.length && (
          <p className="text-sm text-muted-foreground">
            {search ? "No matches." : empty}
          </p>
        )}
      </div>
      {!selected.length && (
        <p className="text-sm text-muted-foreground">Select at least one.</p>
      )}
    </fieldset>
  );
}

function ApiKeysContent({
  application,
  createOpen,
  onCreateOpenChange,
}: ApiKeysCardProps) {
  const [localOpen, setLocalOpen] = useState(false);
  const open = createOpen ?? localOpen;
  const setOpen = onCreateOpenChange ?? setLocalOpen;
  const client = useQueryClient();
  const id = useId();
  const queryKey = [
    "api-keys",
    application?.accountId ?? "personal",
    application?.applicationId ?? "personal",
  ];
  const accounts = useAccounts();
  const universes = useUniverses();
  const [revoking, setRevoking] = useState<{ id: string; name: string } | null>(
    null,
  );
  const [name, setName] = useState("");
  const [days, setDays] = useState("");
  const [limitAccounts, setLimitAccounts] = useState(false);
  const [accountIds, setAccountIds] = useState<string[]>([]);
  const [limitUniverses, setLimitUniverses] = useState(false);
  const [universeIds, setUniverseIds] = useState<string[]>([]);
  const [operations, setOperations] = useState("");
  const [readOnly, setReadOnly] = useState(false);
  const [allowCreation, setAllowCreation] = useState(false);
  const [secret, setSecret] = useState<{ id: string; value: string } | null>(
    null,
  );
  const [copied, setCopied] = useState(false);
  const [copyError, setCopyError] = useState(false);
  const owner = application?.name ?? "this application";
  const scope = application
    ? {
        accountId: application.accountId,
        applicationId: application.applicationId,
      }
    : undefined;
  const keys = useQuery({
    queryKey,
    queryFn: () =>
      scope
        ? platformApi("platform/applications/credentials/list", scope)
        : platformApi("platform/credentials/list", {}),
  });
  const accountChoices = (accounts.data?.accounts ?? []).map((account) => ({
    id: account.id,
    name: account.name,
  }));
  const allowedAccounts = application
    ? [application.accountId]
    : limitAccounts
      ? accountIds
      : undefined;
  const availableUniverses = (universes.data ?? []).filter(
    (universe) =>
      universe.status === "active" &&
      (!allowedAccounts ||
        (!!universe.accountId && allowedAccounts.includes(universe.accountId))),
  );
  const universeChoices = availableUniverses.map((universe) => ({
    id: universe.lightspeedUniverseId,
    name: `${universe.name} · ${universe.handle}`,
  }));
  const selectedUniverseIds = universeIds.filter((value) =>
    availableUniverses.some(
      (universe) => universe.lightspeedUniverseId === value,
    ),
  );
  const refresh = () => client.invalidateQueries({ queryKey });
  const create = useMutation({
    mutationFn: async () => {
      const input = {
        name: name.trim(),
        ...(days ? { expiresInDays: Number(days) } : {}),
        restrictions: {
          ...(application
            ? { accountIds: [application.accountId] }
            : limitAccounts
              ? { accountIds }
              : {}),
          ...(limitUniverses ? { universeIds: selectedUniverseIds } : {}),
          ...(operations.trim() ? { operations: split(operations) } : {}),
          readOnly,
          allowUniverseCreation: allowCreation && !limitUniverses && !readOnly,
        },
      };
      const result = scope
        ? await platformApi("platform/applications/credentials/create", {
            ...input,
            ...scope,
          })
        : await platformApi("platform/credentials/create", input);
      // Only local component state holds the one-time secret; mutation and query
      // caches must never retain the credential response.
      setSecret({ id: result.credential.id, value: result.secret });
      setCopied(false);
      setCopyError(false);
      setName("");
    },
    onSuccess: refresh,
  });
  const revoke = useMutation({
    mutationFn: (keyId: string) =>
      scope
        ? platformApi("platform/applications/credentials/revoke", {
            ...scope,
            keyId,
          })
        : platformApi("platform/credentials/revoke", { keyId }),
    onSuccess: async (_, keyId) => {
      if (secret?.id === keyId) setSecret(null);
      setRevoking(null);
      await refresh();
    },
  });
  const invalidScope =
    (!application && limitAccounts && !accountIds.length) ||
    (limitUniverses && !selectedUniverseIds.length);
  const accountName = (value: string) =>
    accountChoices.find((account) => account.id === value)?.name ?? value;
  const universeName = (value: string) => {
    const universe = universes.data?.find(
      (universe) => universe.lightspeedUniverseId === value,
    );
    return universe ? `${universe.name} (${universe.handle})` : value;
  };
  const disabled = application?.enabled === false;
  const close = () => {
    if (create.isPending) return;
    setOpen(false);
    setSecret(null);
    setName("");
    setDays("");
    setLimitAccounts(false);
    setAccountIds([]);
    setLimitUniverses(false);
    setUniverseIds([]);
    setOperations("");
    setReadOnly(false);
    setAllowCreation(false);
    create.reset();
  };
  return (
    <Card id={application ? undefined : "api-keys"} className="scroll-mt-6">
      <CardHeader>
        <CardTitle>
          {application ? `API keys for ${owner}` : "Personal API keys"}
        </CardTitle>
        <CardDescription>
          {application
            ? `These keys act as ${owner}, using its current account role and universe access.`
            : "These keys act as you, using your current account roles and universe access."}{" "}
          Restrictions can reduce that access, never add permissions. Keys are
          managed here even when restricted to one universe.
        </CardDescription>
        <CardAction>
          <Button disabled={disabled} onClick={() => setOpen(true)}>
            <AddIcon data-icon="inline-start" />
            New API key
          </Button>
        </CardAction>
      </CardHeader>
      <CardContent className="grid gap-6">
        {disabled && (
          <p className="rounded-md border p-3 text-sm">
            This application is disabled. Its keys cannot authenticate. You can
            revoke existing keys; enable the application before creating
            another.
          </p>
        )}
        <Dialog
          open={open}
          onOpenChange={(next) => {
            if (next) setOpen(true);
            else close();
          }}
        >
          <DialogContent
            className="max-h-[min(92dvh,800px)] overflow-y-auto sm:max-w-xl"
            showCloseButton={!create.isPending}
          >
            <DialogHeader>
              <DialogTitle>
                {secret ? "Save your API key" : "New API key"}
              </DialogTitle>
              <DialogDescription>
                {application
                  ? `This key acts as ${owner}.`
                  : "This key acts as you."}{" "}
                Its restrictions can only reduce the owner's current
                permissions.
              </DialogDescription>
            </DialogHeader>
            {secret && (
              <div className="grid gap-3 rounded-md border p-4" role="status">
                <p className="font-medium">
                  Copy this key now. It is shown only once.
                </p>
                <code className="break-all select-all text-sm">
                  {secret.value}
                </code>
                <div className="flex flex-wrap gap-2">
                  <Button
                    variant="outline"
                    onClick={async () => {
                      try {
                        await navigator.clipboard.writeText(secret.value);
                        setCopied(true);
                        setCopyError(false);
                      } catch {
                        setCopyError(true);
                      }
                    }}
                  >
                    {copied ? "Copied" : "Copy key"}
                  </Button>
                  <Button variant="outline" onClick={close}>
                    I have saved it
                  </Button>
                </div>
                {copyError && (
                  <p role="alert" className="text-sm text-destructive">
                    Could not copy the key. Select and copy it manually.
                  </p>
                )}
              </div>
            )}
            {!disabled && !secret && (
              <form
                className="grid max-w-xl gap-4"
                onSubmit={(event) => {
                  event.preventDefault();
                  if (!invalidScope && !secret) create.mutate();
                }}
              >
                <Field>
                  <FieldLabel htmlFor={`${id}-name`}>Name</FieldLabel>
                  <Input
                    id={`${id}-name`}
                    aria-label="API key name"
                    value={name}
                    onChange={(event) => setName(event.target.value)}
                    placeholder="For example, local development or deployment automation"
                    required
                    maxLength={120}
                  />
                </Field>
                <Field>
                  <FieldLabel htmlFor={`${id}-expiry`}>
                    Expiry in days
                  </FieldLabel>
                  <Input
                    id={`${id}-expiry`}
                    aria-label="API key expiry in days"
                    type="number"
                    min={1}
                    max={3650}
                    value={days}
                    onChange={(event) => setDays(event.target.value)}
                    placeholder="Deployment default"
                  />
                  <FieldDescription>
                    Leave blank to use the deployment's standard expiry.
                  </FieldDescription>
                </Field>
                <details>
                  <summary className="cursor-pointer text-sm font-medium">
                    Restrict access
                  </summary>
                  <div className="mt-4 grid gap-4">
                    {application ? (
                      <p className="text-sm">
                        Account:{" "}
                        <strong>{accountName(application.accountId)}</strong>.
                        The application cannot access other accounts.
                      </p>
                    ) : (
                      <>
                        <Field>
                          <FieldLabel htmlFor={`${id}-accounts`}>
                            Accounts
                          </FieldLabel>
                          <select
                            id={`${id}-accounts`}
                            aria-label="Account access"
                            className="rounded-md border bg-background p-2 text-sm"
                            value={limitAccounts ? "selected" : "all"}
                            onChange={(event) => {
                              setLimitAccounts(
                                event.target.value === "selected",
                              );
                              setUniverseIds([]);
                            }}
                          >
                            <option value="all">
                              All accounts I can access
                            </option>
                            <option value="selected">Selected accounts</option>
                          </select>
                        </Field>
                        {limitAccounts && (
                          <ScopeChoices
                            title="Allowed accounts"
                            choices={accountChoices}
                            selected={accountIds}
                            onChange={(value) => {
                              setAccountIds(value);
                              setUniverseIds([]);
                            }}
                            empty={
                              accounts.isPending
                                ? "Loading accounts…"
                                : "No accounts available."
                            }
                          />
                        )}
                      </>
                    )}
                    <Field>
                      <FieldLabel htmlFor={`${id}-universes`}>
                        Universes
                      </FieldLabel>
                      <select
                        id={`${id}-universes`}
                        aria-label="Universe access"
                        className="rounded-md border bg-background p-2 text-sm"
                        value={limitUniverses ? "selected" : "all"}
                        onChange={(event) => {
                          const limit = event.target.value === "selected";
                          setLimitUniverses(limit);
                          if (limit) setAllowCreation(false);
                        }}
                      >
                        <option value="all">
                          {application
                            ? "All universes this application can access"
                            : "All universes I can access in the allowed accounts"}
                        </option>
                        <option value="selected">Selected universes</option>
                      </select>
                    </Field>
                    {limitUniverses && (
                      <ScopeChoices
                        title="Allowed universes"
                        choices={universeChoices}
                        selected={selectedUniverseIds}
                        onChange={setUniverseIds}
                        empty={
                          universes.isPending
                            ? "Loading universes…"
                            : "No accessible universes in the selected accounts."
                        }
                      />
                    )}
                    <label className="flex gap-2 text-sm">
                      <input
                        type="checkbox"
                        checked={readOnly}
                        onChange={(event) => {
                          setReadOnly(event.target.checked);
                          if (event.target.checked) setAllowCreation(false);
                        }}
                      />
                      Read only
                    </label>
                    <details>
                      <summary className="cursor-pointer text-sm">
                        Advanced: restrict API operations
                      </summary>
                      <div className="mt-3">
                        <Field>
                          <FieldLabel htmlFor={`${id}-operations`}>
                            Operation IDs, comma separated
                          </FieldLabel>
                          <Input
                            id={`${id}-operations`}
                            aria-label="Allowed operations"
                            value={operations}
                            onChange={(event) =>
                              setOperations(event.target.value)
                            }
                            placeholder="For example: profiles/read, profiles/list"
                          />
                          <FieldDescription>
                            Leave blank to allow all operations permitted by the
                            roles and other restrictions.
                          </FieldDescription>
                        </Field>
                      </div>
                    </details>
                    {[accounts.error, universes.error]
                      .filter(Boolean)
                      .map((error, index) => (
                        <p
                          key={index}
                          role="alert"
                          className="text-sm text-destructive"
                        >
                          Could not load access choices: {error?.message}
                        </p>
                      ))}
                  </div>
                </details>
                <label className="flex gap-2 text-sm">
                  <input
                    type="checkbox"
                    checked={allowCreation}
                    disabled={limitUniverses || readOnly}
                    onChange={(event) => setAllowCreation(event.target.checked)}
                  />
                  Allow universe creation
                </label>
                <p className="-mt-2 text-sm text-muted-foreground">
                  {limitUniverses || readOnly
                    ? "Universe creation is unavailable for read-only keys or keys restricted to existing universes."
                    : application
                      ? "Requires the application's Creator or Admin role in this account."
                      : "Requires your Creator or Admin role in an allowed account."}
                </p>
                {create.error && (
                  <p role="alert" className="text-sm text-destructive">
                    {create.error.message}
                  </p>
                )}
                <DialogFooter>
                  <Button
                    type="button"
                    variant="outline"
                    disabled={create.isPending}
                    onClick={close}
                  >
                    Cancel
                  </Button>
                  <Button
                    type="submit"
                    disabled={
                      create.isPending ||
                      !!secret ||
                      !name.trim() ||
                      invalidScope
                    }
                  >
                    {create.isPending ? "Creating…" : "Create API key"}
                  </Button>
                </DialogFooter>
              </form>
            )}
          </DialogContent>
        </Dialog>
        <ApiKeyRevokeDialog
          name={revoking?.name}
          owner={application?.name}
          pending={revoke.isPending}
          error={revoke.error}
          onConfirm={() => {
            if (revoking) revoke.mutate(revoking.id);
          }}
          onClose={() => {
            setRevoking(null);
            revoke.reset();
          }}
        />
        {[keys.error].filter(Boolean).map((error, index) => (
          <p role="alert" className="text-destructive" key={index}>
            {error?.message}
          </p>
        ))}
        <section aria-label="Existing API keys" className="grid gap-3">
          <h3 className="text-sm font-medium">Existing keys</h3>
          <p className="text-sm text-muted-foreground">
            Creating a replacement leaves existing keys valid until you revoke
            them.
          </p>
          {keys.isPending && (
            <p className="text-sm text-muted-foreground">Loading keys…</p>
          )}
          {keys.data?.credentials.length === 0 && (
            <p className="text-sm text-muted-foreground">
              {application
                ? "This application has no API keys."
                : "You have no personal API keys."}
            </p>
          )}
          {keys.data?.credentials.map((key) => {
            const expired = new Date(key.expiresAt).getTime() <= Date.now();
            const status = !key.enabled
              ? "Revoked"
              : expired
                ? "Expired"
                : disabled
                  ? "Application disabled"
                  : "Active";
            return (
              <div
                className="flex flex-wrap items-start justify-between gap-3 border-t pt-4"
                key={key.id}
              >
                <div className="grid min-w-0 gap-1 text-sm">
                  <div className="flex flex-wrap items-center gap-2">
                    <p className="font-medium">{key.name}</p>
                    <Badge variant="secondary">{status}</Badge>
                  </div>
                  <p className="text-muted-foreground">
                    {key.start}… · {expired ? "Expired" : "Expires"}{" "}
                    {new Date(key.expiresAt).toLocaleDateString()}
                  </p>
                  <p>
                    Accounts:{" "}
                    {key.restrictions.accountIds
                      ? key.restrictions.accountIds
                          .map(accountName)
                          .join(", ") || "None"
                      : application
                        ? accountName(application.accountId)
                        : "All you can access"}
                  </p>
                  <p>
                    Universes:{" "}
                    {key.restrictions.universeIds
                      ? key.restrictions.universeIds
                          .map(universeName)
                          .join(", ") || "None"
                      : "All accessible in the allowed accounts"}
                  </p>
                  <p>
                    {key.restrictions.readOnly
                      ? "Read only"
                      : "Reads and writes permitted by roles"}{" "}
                    ·{" "}
                    {key.restrictions.allowUniverseCreation
                      ? "Universe creation allowed by roles"
                      : "No universe creation"}
                  </p>
                  {key.restrictions.operations && (
                    <p className="break-all text-muted-foreground">
                      Operations:{" "}
                      {key.restrictions.operations.join(", ") || "None"}
                    </p>
                  )}
                </div>
                <Button
                  variant="outline"
                  disabled={revoke.isPending || !key.enabled}
                  onClick={() => {
                    revoke.reset();
                    setRevoking({ id: key.id, name: key.name });
                  }}
                >
                  Revoke
                </Button>
              </div>
            );
          })}
        </section>
      </CardContent>
    </Card>
  );
}
