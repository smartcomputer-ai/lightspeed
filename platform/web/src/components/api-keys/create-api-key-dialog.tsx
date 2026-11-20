import { useId, useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { useOrganizations } from "@/lib/platform-api";
import { useUniverses } from "@/lib/universes";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { SettingsDisclosure } from "@/components/ui/settings-disclosure";
import { Switch } from "@/components/ui/switch";
import { createApiKey, useInvalidateApiKeys, type ManagedApiKeyOwner } from "./api-keys";

type Choice = { id: string; name: string };
type Expiry = "default" | "30" | "90" | "365" | "never";
const EXPIRY_LABELS: Record<Expiry, string> = {
  default: "Deployment default",
  "30": "30 days",
  "90": "90 days",
  "365": "1 year",
  never: "No expiry",
};

const split = (value: string) => [...new Set(value.split(",").map((part) => part.trim()).filter(Boolean))];

/// A new key in one dialog: what it is called, how long it lives and what it
/// may reach, then the secret, shown once. Restrictions only narrow what the
/// owner may already do.
export function CreateApiKeyDialog({ owner, open, onOpenChange }: {
  owner: ManagedApiKeyOwner;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  // A fresh form, and no leftover secret, every time it opens.
  return (
    <Dialog open={open} onOpenChange={(next) => { if (!next) onOpenChange(false); }}>
      {open && <CreateApiKeyForm owner={owner} close={() => onOpenChange(false)} />}
    </Dialog>
  );
}

function CreateApiKeyForm({ owner, close }: { owner: ManagedApiKeyOwner; close: () => void }) {
  const id = useId();
  const invalidate = useInvalidateApiKeys(owner);
  const organizations = useOrganizations();
  const universes = useUniverses();
  const organizationChoices: Choice[] = (organizations.data?.organizations ?? []).map(({ id, name }) => ({ id, name }));
  const [name, setName] = useState("");
  const [expiry, setExpiry] = useState<Expiry>("default");
  // A person's key starts limited to one organization; every organization is an explicit choice.
  const [allOrganizations, setAllOrganizations] = useState(false);
  const [organizationIds, setOrganizationIds] = useState<string[]>(organizationChoices[0] ? [organizationChoices[0].id] : []);
  const [limitUniverses, setLimitUniverses] = useState(false);
  const [universeIds, setUniverseIds] = useState<string[]>([]);
  const [readOnly, setReadOnly] = useState(false);
  const [allowCreation, setAllowCreation] = useState(false);
  const [allowKeys, setAllowKeys] = useState(false);
  const [operations, setOperations] = useState("");
  const [secret, setSecret] = useState<string | null>(null);
  const [copied, setCopied] = useState<"yes" | "failed" | null>(null);

  const allowedOrganizations = owner.kind === "serviceUser" ? [owner.organizationId] : allOrganizations ? undefined : organizationIds;
  const universeChoices: Choice[] = (universes.data ?? [])
    .filter((universe) => universe.status === "active" && (!allowedOrganizations || (!!universe.organizationId && allowedOrganizations.includes(universe.organizationId))))
    .map((universe) => ({ id: universe.lightspeedUniverseId, name: `${universe.name} · ${universe.handle}` }));
  const selectedUniverseIds = universeIds.filter((value) => universeChoices.some((choice) => choice.id === value));
  const creationUnavailable = readOnly || limitUniverses;
  const invalid = !name.trim() || (owner.kind === "personal" && !allOrganizations && !organizationIds.length) || (limitUniverses && !selectedUniverseIds.length);

  const create = useMutation({
    mutationFn: async () => {
      const result = await createApiKey(owner, {
        name: name.trim(),
        ...(expiry === "never" ? { expiresInDays: null } : expiry !== "default" ? { expiresInDays: Number(expiry) } : {}),
        restrictions: {
          ...(owner.kind === "serviceUser" ? { organizationIds: [owner.organizationId] } : allOrganizations ? {} : { organizationIds }),
          ...(limitUniverses ? { universeIds: selectedUniverseIds } : {}),
          ...(operations.trim() ? { operations: split(operations) } : {}),
          readOnly,
          allowUniverseCreation: allowCreation && !creationUnavailable,
          ...(allowKeys && !creationUnavailable ? { allowServiceUserKeys: true } : {}),
        },
      });
      // Only this component's state holds the one-time secret; query and
      // mutation caches never retain the credential response.
      setSecret(result.secret);
    },
    onSuccess: () => void invalidate(),
  });

  if (secret) {
    return (
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>Save your API key</DialogTitle>
          <DialogDescription>Copy it now. It is shown only once and cannot be recovered.</DialogDescription>
        </DialogHeader>
        <code className="block break-all rounded-lg border bg-muted/40 p-3 text-sm select-all" role="status">{secret}</code>
        {copied === "failed" && <p role="alert" className="text-sm text-destructive">Could not copy the key. Select and copy it manually.</p>}
        <DialogFooter>
          <Button variant="outline" onClick={async () => {
            try { await navigator.clipboard.writeText(secret); setCopied("yes"); } catch { setCopied("failed"); }
          }}>{copied === "yes" ? "Copied" : "Copy key"}</Button>
          <Button onClick={close}>Done</Button>
        </DialogFooter>
      </DialogContent>
    );
  }

  return (
    <DialogContent className="max-h-[min(92dvh,800px)] overflow-y-auto sm:max-w-xl" showCloseButton={!create.isPending}>
      <DialogHeader>
        <DialogTitle>New API key</DialogTitle>
        <DialogDescription>
          {owner.kind === "serviceUser" ? `It acts as ${owner.name}, in its organization only.` : "It acts as you."}{" "}
          Restrictions can only narrow that access.
        </DialogDescription>
      </DialogHeader>
      <form className="grid gap-5" onSubmit={(event) => { event.preventDefault(); if (!invalid) create.mutate(); }}>
        <Field>
          <FieldLabel htmlFor={`${id}-name`}>Name</FieldLabel>
          <Input id={`${id}-name`} aria-label="API key name" value={name} onChange={(event) => setName(event.target.value)}
            placeholder="For example, deployment automation" required maxLength={120} autoFocus />
        </Field>
        <Field>
          <FieldLabel htmlFor={`${id}-expiry`}>Expires</FieldLabel>
          <Select value={expiry} onValueChange={(value) => value !== null && setExpiry(value as Expiry)}>
            <SelectTrigger id={`${id}-expiry`} aria-label="API key expiry"><SelectValue /></SelectTrigger>
            <SelectContent>
              {(Object.keys(EXPIRY_LABELS) as Expiry[]).map((value) => <SelectItem key={value} value={value}>{EXPIRY_LABELS[value]}</SelectItem>)}
            </SelectContent>
          </Select>
          {expiry === "never" && <FieldDescription>It works until revoked. A deployment with a maximum key lifetime refuses it.</FieldDescription>}
        </Field>
        {owner.kind === "personal" && (
          <Field>
            <FieldLabel htmlFor={`${id}-organizations`}>Organizations</FieldLabel>
            <Select value={allOrganizations ? "all" : "selected"} onValueChange={(value) => {
              if (value === null) return;
              setAllOrganizations(value === "all");
              setUniverseIds([]);
            }}>
              <SelectTrigger id={`${id}-organizations`} aria-label="Organization access"><SelectValue /></SelectTrigger>
              <SelectContent>
                <SelectItem value="selected">Selected organizations</SelectItem>
                <SelectItem value="all">All my organizations</SelectItem>
              </SelectContent>
            </Select>
            {!allOrganizations && (
              <Checklist label="Allowed organizations" choices={organizationChoices} selected={organizationIds}
                onChange={(value) => { setOrganizationIds(value); setUniverseIds([]); }}
                empty={organizations.isPending ? "Loading organizations…" : "No organizations available."} />
            )}
          </Field>
        )}
        <Field>
          <FieldLabel htmlFor={`${id}-universes`}>Universes</FieldLabel>
          <Select value={limitUniverses ? "selected" : "all"} onValueChange={(value) => {
            if (value === null) return;
            setLimitUniverses(value === "selected");
          }}>
            <SelectTrigger id={`${id}-universes`} aria-label="Universe access"><SelectValue /></SelectTrigger>
            <SelectContent>
              <SelectItem value="all">{owner.kind === "serviceUser" ? "Every universe it can access" : "Every universe I can access there"}</SelectItem>
              <SelectItem value="selected">Selected universes</SelectItem>
            </SelectContent>
          </Select>
          {limitUniverses && (
            <Checklist label="Allowed universes" choices={universeChoices} selected={selectedUniverseIds} onChange={setUniverseIds}
              empty={universes.isPending ? "Loading universes…" : "No accessible universes in the allowed organizations."} />
          )}
        </Field>
        <SwitchRow id={`${id}-read-only`} label="Read only" description="The key cannot change anything."
          checked={readOnly} onChange={setReadOnly} />
        <SwitchRow id={`${id}-creation`} label="Allow creating universes"
          description={creationUnavailable ? "Not available for read-only keys or keys limited to existing universes."
            : owner.kind === "serviceUser" ? "Needs the service user's Creator or Admin role." : "Needs your Creator or Admin role in an allowed organization."}
          checked={allowCreation && !creationUnavailable} disabled={creationUnavailable} onChange={setAllowCreation} />
        <SwitchRow id={`${id}-service-user-keys`} label="Allow declaring service-user keys"
          description={creationUnavailable ? "Not available for read-only keys or keys limited to existing universes."
            : "For a pipeline that declares service-user keys from a secret store. Needs the Organization Admin role; other key changes still need a person."}
          checked={allowKeys && !creationUnavailable} disabled={creationUnavailable} onChange={setAllowKeys} />
        <SettingsDisclosure summary={operations.trim() ? `${split(operations).length} operations allowed` : "Every operation its roles allow"}
          action="Restrict operations" label="Restrict API operations">
          <Field>
            <FieldLabel htmlFor={`${id}-operations`}>Operation IDs, comma separated</FieldLabel>
            <Input id={`${id}-operations`} aria-label="Allowed operations" value={operations} onChange={(event) => setOperations(event.target.value)}
              placeholder="For example: profiles/read, profiles/list" />
          </Field>
        </SettingsDisclosure>
        {[organizations.error, universes.error].filter(Boolean).map((error, index) => (
          <p key={index} role="alert" className="text-sm text-destructive">Could not load access choices: {error?.message}</p>
        ))}
        {create.error && <p role="alert" className="text-sm text-destructive">{create.error.message}</p>}
        <DialogFooter>
          <Button type="button" variant="outline" disabled={create.isPending} onClick={close}>Cancel</Button>
          <Button type="submit" disabled={create.isPending || invalid}>{create.isPending ? "Creating…" : "Create key"}</Button>
        </DialogFooter>
      </form>
    </DialogContent>
  );
}

function SwitchRow({ id, label, description, checked, disabled, onChange }: {
  id: string; label: string; description: string; checked: boolean; disabled?: boolean; onChange: (checked: boolean) => void;
}) {
  return (
    <div className="flex items-start justify-between gap-4">
      <div className="grid gap-0.5">
        <label htmlFor={id} className="text-sm font-medium">{label}</label>
        <p className="text-sm text-muted-foreground">{description}</p>
      </div>
      <Switch id={id} checked={checked} disabled={disabled} onCheckedChange={onChange} />
    </div>
  );
}

function Checklist({ label, choices, selected, onChange, empty }: {
  label: string; choices: Choice[]; selected: string[]; onChange: (value: string[]) => void; empty: string;
}) {
  const [search, setSearch] = useState("");
  const visible = choices.filter((choice) => choice.name.toLowerCase().includes(search.toLowerCase()));
  return (
    <fieldset className="grid gap-2">
      <legend className="sr-only">{label}</legend>
      {choices.length > 5 && (
        <Input aria-label={`Find ${label.toLowerCase()}`} placeholder="Find…" value={search} onChange={(event) => setSearch(event.target.value)} />
      )}
      <div className="grid max-h-48 gap-2 overflow-y-auto rounded-lg border p-3">
        {visible.map((choice) => (
          <label key={choice.id} className="flex items-start gap-2 text-sm">
            <input type="checkbox" className="mt-1" checked={selected.includes(choice.id)}
              onChange={() => onChange(selected.includes(choice.id) ? selected.filter((value) => value !== choice.id) : [...selected, choice.id])} />
            <span>{choice.name}</span>
          </label>
        ))}
        {!visible.length && <p className="text-sm text-muted-foreground">{search ? "No matches." : empty}</p>}
      </div>
      {!selected.length && <p className="text-sm text-muted-foreground">Select at least one.</p>}
    </fieldset>
  );
}
