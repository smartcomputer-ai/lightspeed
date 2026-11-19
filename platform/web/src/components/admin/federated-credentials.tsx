import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { platformApi } from "@/lib/platform-api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Field, FieldLabel } from "@/components/ui/field";
import { SettingsDisclosure } from "@/components/ui/settings-disclosure";
import { Switch } from "@/components/ui/switch";
import { ConfirmDangerButton } from "@/components/confirm-danger-button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";

export function FederatedCredentials(owner: { organizationId: string; serviceUserId: string }) {
  const client = useQueryClient();
  const key = ["federated-credentials", owner.organizationId, owner.serviceUserId];
  const list = useQuery({
    queryKey: key,
    queryFn: () => platformApi("platform/service-users/federated-credentials/list", owner),
  });
  const [selected, setSelected] = useState("");
  const [subject, setSubject] = useState("");
  const [uid, setUid] = useState("");
  const [readOnly, setReadOnly] = useState(false);
  const [operations, setOperations] = useState("");
  const [universeIds, setUniverseIds] = useState("");
  const ids = (input: string) =>
    input
      .split(",")
      .map((v) => v.trim())
      .filter(Boolean);
  const issuer = selected || list.data?.issuers[0] || "";
  const refresh = () => client.invalidateQueries({ queryKey: key });
  const put = useMutation({
    mutationFn: () =>
      platformApi("platform/service-users/federated-credentials/put", {
        ...owner,
        issuer,
        subject,
        serviceAccountUid: uid || null,
        createOnly: true,
        restrictions: {
          ...(readOnly && { readOnly }),
          ...(operations.trim() && { operations: ids(operations) }),
          ...(universeIds.trim() && { universeIds: ids(universeIds) }),
        },
      }),
    onSuccess: () => {
      setSubject("");
      setUid("");
      setReadOnly(false);
      setOperations("");
      setUniverseIds("");
      return refresh();
    },
  });
  const remove = useMutation({
    mutationFn: (credentialId: string) =>
      platformApi("platform/service-users/federated-credentials/delete", { ...owner, credentialId }),
    onSuccess: refresh,
  });
  const error = list.error ?? put.error ?? remove.error;
  if (error && !list.data) return <p role="alert">Could not load federated credentials. {error.message}</p>;
  if (!list.data) return null;
  const exists = list.data.credentials.some(
    (binding) => binding.issuer === issuer && binding.subject === subject,
  );
  return (
    <section className="grid gap-3">
      <h3 className="font-medium">Federated credentials</h3>
      {list.data.issuers.length === 0 && (
        <p className="text-sm text-muted-foreground">
          To add a workload credential, configure a trusted issuer in the Platform backend’s{" "}
          <code>LIGHTSPEED_PLATFORM_FEDERATION_FILE</code> and restart the backend.
        </p>
      )}
      {list.data.credentials.map((binding) => (
        <div key={binding.id} className="flex items-start justify-between gap-3">
          <div className="min-w-0 text-sm">
            <strong>{binding.issuer}</strong>
            <div className="break-all font-mono">{binding.subject}</div>
            {binding.serviceAccountUid && <p>ServiceAccount UID {binding.serviceAccountUid}</p>}
            {binding.restrictions.readOnly && <p>Read only</p>}
            {binding.restrictions.organizationIds && (
              <p>Organizations: {binding.restrictions.organizationIds.join(", ") || "None"}</p>
            )}
            {binding.restrictions.universeIds && (
              <p>Universes: {binding.restrictions.universeIds.join(", ") || "None"}</p>
            )}
            {binding.restrictions.operations && (
              <p>Operations: {binding.restrictions.operations.join(", ") || "None"}</p>
            )}
            {binding.restrictions.allowUniverseCreation === false && <p>Universe creation disabled</p>}
            {binding.restrictions.allowServiceUserKeys && <p>Can manage service-user credentials</p>}
            <p className="text-muted-foreground">
              {binding.lastUsedAt
                ? `Last used ${new Date(binding.lastUsedAt).toLocaleString()}`
                : "Never used"}
            </p>
          </div>
          <ConfirmDangerButton
            label="Revoke"
            title="Revoke federated credential?"
            pending={remove.isPending}
            description={`The workload ${binding.subject} will lose access through this binding.`}
            onConfirm={() => remove.mutate(binding.id)}
          />
        </div>
      ))}
      {list.data.issuers.length > 0 && (
        <form
          className="grid gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            if (subject && !exists) put.mutate();
          }}
        >
          <Field>
            <FieldLabel htmlFor="federated-issuer">Trusted issuer</FieldLabel>
            <Select value={issuer} onValueChange={(v) => setSelected(v ?? "")}>
              <SelectTrigger id="federated-issuer">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {list.data.issuers.map((name) => (
                  <SelectItem key={name} value={name}>
                    {name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </Field>
          <Field>
            <FieldLabel htmlFor="federated-subject">Workload subject</FieldLabel>
            <Input
              id="federated-subject"
              required
              value={subject}
              onChange={(e) => setSubject(e.target.value)}
            />
          </Field>
          <Field>
            <FieldLabel htmlFor="federated-uid">ServiceAccount UID (optional)</FieldLabel>
            <Input id="federated-uid" value={uid} onChange={(e) => setUid(e.target.value)} />
          </Field>
          <SettingsDisclosure action="Customize restrictions">
            <Field>
              <FieldLabel htmlFor="fed-read-only">Read only</FieldLabel>
              <Switch id="fed-read-only" checked={readOnly} onCheckedChange={setReadOnly} />
            </Field>
            <Field>
              <FieldLabel htmlFor="fed-operations">Allowed operations (comma-separated)</FieldLabel>
              <Input id="fed-operations" value={operations} onChange={(e) => setOperations(e.target.value)} />
            </Field>
            <Field>
              <FieldLabel htmlFor="fed-universes">Allowed universe IDs (comma-separated)</FieldLabel>
              <Input
                id="fed-universes"
                value={universeIds}
                onChange={(e) => setUniverseIds(e.target.value)}
              />
            </Field>
          </SettingsDisclosure>
          <Button type="submit" variant="outline" size="sm" disabled={!subject || exists || put.isPending}>
            Add credential
          </Button>
        </form>
      )}
      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error.message}
        </p>
      )}
    </section>
  );
}
