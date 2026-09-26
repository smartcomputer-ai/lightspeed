import { useActionPermissions } from "@/lib/permissions";
import { useState, type FormEvent } from "react";
import { useMutation } from "@tanstack/react-query";
import { Copy } from "lucide-react";
import { api, type SecretGrant, type SubscriptionImportResult } from "@/api";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { DialogFooter } from "@/components/ui/dialog";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { IdText } from "@/components/ui/table";
import { Textarea } from "@/components/ui/textarea";
import { formatExpiry } from "@/lib/integrations";
import {
  CODEX_AUTH_JSON_BOOTSTRAP,
  isCodexTokenSet,
  subscriptionAccountLabel,
  subscriptionBinding,
  type SubscriptionProvider,
} from "@/lib/subscriptions";
import { ConfirmDangerButton } from "@/components/confirm-danger-button";

const COPY: Record<
  SubscriptionProvider,
  { steps: string[]; placeholder: string; rows: number; namePlaceholder: string; apiKeyNote: string }
> = {
  anthropic: {
    steps: [
      "On your own machine, run `claude setup-token` and complete the browser login.",
      "Copy the token it prints (it starts with sk-ant-oat) and paste it below.",
      "Bind the credential to environments as CLAUDE_CODE_OAUTH_TOKEN (suggested automatically).",
    ],
    placeholder: "sk-ant-oat01-…",
    rows: 3,
    namePlaceholder: "Lukas · Max",
    apiKeyNote:
      "Do not bind ANTHROPIC_API_KEY next to the subscription token in an environment; Claude Code would prefer the key.",
  },
  openAi: {
    steps: [
      "Plus/Pro/Team: on your own machine, run `codex login`, then paste the contents of ~/.codex/auth.json below.",
      "ChatGPT Enterprise: create a Codex access token in your workspace and paste it instead.",
      "Bind the credential to environments as CODEX_AUTH_JSON (token set) or CODEX_ACCESS_TOKEN (Enterprise); the name is suggested automatically.",
    ],
    placeholder: '{ "auth_mode": "chatgpt", "tokens": { … } }  —  or a Codex access token',
    rows: 6,
    namePlaceholder: "Lukas · ChatGPT Pro",
    apiKeyNote: "Plus/Pro/Team token sets need the auth.json bootstrap line (shown in the subscription's details) in the environment.",
  },
};

/// Paste form for a Claude Code / Codex subscription credential. Rendered
/// inside the Add-model-provider dialog.
export function SubscriptionForm({
  universeId,
  provider,
  onConnected,
  onCancel,
}: {
  universeId: string;
  provider: SubscriptionProvider;
  onConnected: (result: SubscriptionImportResult) => void;
  onCancel: () => void;
}) {
  const copy = COPY[provider];
  const [displayName, setDisplayName] = useState("");
  const [credential, setCredential] = useState("");
  const [error, setError] = useState<string | null>(null);

  const connect = useMutation<SubscriptionImportResult, Error, void>({
    mutationFn: () =>
      api<SubscriptionImportResult>(
        "POST",
        `/api/v1/universes/${universeId}/integrations/subscriptions`,
        { provider, credential, ...(displayName.trim() ? { displayName: displayName.trim() } : {}) },
      ),
    onSuccess: (result) => {
      setCredential("");
      onConnected(result);
    },
    onError: (reason) => setError(reason.message),
  });

  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!credential.trim()) {
      setError("paste the credential first");
      return;
    }
    connect.mutate();
  };

  return (
    <form onSubmit={submit} className="grid gap-4">
      <p className="text-sm text-muted-foreground">
        {provider === "anthropic"
          ? "This token is only injected into environments so the Claude Code agent can run there on your subscription. Lightspeed's own sessions keep using model provider API keys."
          : "This credential is only injected into environments so the Codex agent can run there on your ChatGPT subscription. Lightspeed's own sessions keep using model provider API keys."}{" "}
        It is sent once to Lightspeed, encrypted, and never returned by an API.
      </p>
      <ol className="list-decimal space-y-1 pl-5 text-sm text-muted-foreground">
        {copy.steps.map((step) => (
          <li key={step}>{step}</li>
        ))}
      </ol>
      <Field>
        <FieldLabel htmlFor={`sub-name-${provider}`}>Display name</FieldLabel>
        <Input
          id={`sub-name-${provider}`}
          value={displayName}
          onChange={(event) => setDisplayName(event.target.value)}
          placeholder={copy.namePlaceholder}
        />
      </Field>
      <Field>
        <FieldLabel htmlFor={`sub-cred-${provider}`}>Credential</FieldLabel>
        <Textarea
          id={`sub-cred-${provider}`}
          value={credential}
          onChange={(event) => {
            setCredential(event.target.value);
            setError(null);
          }}
          autoComplete="off"
          spellCheck={false}
          rows={copy.rows}
          className="max-h-40 resize-y overflow-y-auto font-mono text-xs"
          placeholder={copy.placeholder}
          autoFocus
        />
      </Field>
      <p className="text-xs text-muted-foreground">{copy.apiKeyNote}</p>
      {error && <p className="text-sm text-destructive">{error}</p>}
      <DialogFooter>
        <Button type="button" variant="outline" onClick={onCancel}>
          Back
        </Button>
        <Button type="submit" disabled={connect.isPending}>
          {connect.isPending ? "Encrypting…" : "Connect"}
        </Button>
      </DialogFooter>
    </form>
  );
}

/// Details for a connected subscription: account, binding hint, expiry,
/// Codex bootstrap snippet, disconnect.
export function SubscriptionDetails({
  universeId,
  grant,
  onDisconnected,
}: {
  universeId: string;
  grant: SecretGrant;
  onDisconnected: () => void;
}) {
  const writable = useActionPermissions(universeId).can("configure_resource");
  const binding = subscriptionBinding(grant);
  const disconnect = useMutation({
    mutationFn: () =>
      api<SecretGrant>(
        "DELETE",
        `/api/v1/universes/${universeId}/integrations/subscriptions/${encodeURIComponent(grant.grantId)}`,
      ),
    onSuccess: onDisconnected,
  });

  return (
    <div className="grid gap-4">
      <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
        <dt className="text-muted-foreground">Account</dt>
        <dd>{subscriptionAccountLabel(grant) || "—"}</dd>
        <dt className="text-muted-foreground">Bind as</dt>
        <dd>{binding ? <IdText>{binding.envName}</IdText> : "—"}</dd>
        <dt className="text-muted-foreground">Expires</dt>
        <dd>{formatExpiry(grant.expiresAtMs)}</dd>
        <dt className="text-muted-foreground">Status</dt>
        <dd>
          <SubscriptionStatusBadge status={grant.status} />
        </dd>
        <dt className="text-muted-foreground">Credential ID</dt>
        <dd>
          <IdText>{grant.grantId}</IdText>
        </dd>
      </dl>
      <p className="text-sm text-muted-foreground">
        Used only inside environments (Environments → Assign credential; the variable name above is
        suggested automatically). Lightspeed's own sessions do not use this credential; they use
        model provider API keys.
      </p>
      {isCodexTokenSet(grant) && <CodexBootstrapNote />}
      {disconnect.error && <p className="text-sm text-destructive">{disconnect.error.message}</p>}
      {writable && (
        <DialogFooter>
          <ConfirmDangerButton
            label="Disconnect"
            title="Disconnect this subscription?"
            description={
              <>
                Environments bound to <span className="font-mono text-xs">{grant.grantId}</span> stop
                receiving the credential on their next job. The subscription itself is unaffected;
                revoke the token with the provider if it leaked.
              </>
            }
            pending={disconnect.isPending}
            onConfirm={() => disconnect.mutate()}
          />
        </DialogFooter>
      )}
    </div>
  );
}

export function CodexBootstrapNote() {
  const [copied, setCopied] = useState(false);
  return (
    <div className="rounded-xl border bg-muted/15 p-3 text-sm">
      <div className="mb-2 flex items-center justify-between gap-3">
        <p className="text-muted-foreground">
          Codex reads the token set from <span className="font-mono">$CODEX_HOME/auth.json</span>.
          Run this in the environment before Codex (image entrypoint or job pre-command):
        </p>
        <Button
          variant="outline"
          size="sm"
          onClick={() => {
            void navigator.clipboard?.writeText(CODEX_AUTH_JSON_BOOTSTRAP).then(() => {
              setCopied(true);
              window.setTimeout(() => setCopied(false), 1500);
            });
          }}
        >
          <Copy />
          {copied ? "Copied" : "Copy"}
        </Button>
      </div>
      <pre className="overflow-x-auto rounded-md bg-background p-3 font-mono text-xs">
        {CODEX_AUTH_JSON_BOOTSTRAP}
      </pre>
    </div>
  );
}

function SubscriptionStatusBadge({ status }: { status: SecretGrant["status"] }) {
  if (status === "active") return <Badge variant="secondary">connected</Badge>;
  if (status === "needsReauth" || status === "failed") {
    return (
      <Badge variant="outline" className="border-destructive/50 text-destructive">
        {status === "needsReauth" ? "reconnect" : status}
      </Badge>
    );
  }
  return <Badge variant="outline">revoked</Badge>;
}
