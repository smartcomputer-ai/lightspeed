import type { RefObject } from "react";
import { ResourceDetail, ResourceDetailsDialog, ResourceDetailsGrid } from "@/components/resource-details-dialog";
import { ResourceStatus } from "@/components/resource-table";
import { resourceTimestamp } from "@/lib/resource-status";
import { formatExpiry } from "@/lib/integrations";
import { subscriptionAccountLabel, subscriptionBinding } from "@/lib/subscriptions";
import { modelProviderDefinition } from "./catalog";
import { ModelApiKeyDetails, OpenAiCompatibleDetails } from "./model-api-key";
import { SubscriptionDetails } from "./subscription";
import type { ConnectedModelProvider } from "./use-model-providers";

/// Details/configuration for one connected model provider; content is
/// dispatched on the provider kind.
export function ModelProviderDetailsDialog({
  universeId,
  provider,
  onOpenChange,
  onChanged,
  finalFocus,
}: {
  universeId: string;
  provider: ConnectedModelProvider | null;
  onOpenChange: (open: boolean) => void;
  onChanged: () => void;
  finalFocus?: RefObject<HTMLElement | null>;
}) {
  const definition = provider ? modelProviderDefinition(provider.kind) : null;
  return (
    <ResourceDetailsDialog open={provider !== null} onOpenChange={onOpenChange}
      title={provider?.title ?? "Model provider"} description={provider?.connection === "Subscription" ? "Account access for coding agents running in environments." : "Connection settings and available models."}
      icon={definition && <definition.Logo size={18} />} finalFocus={finalFocus}>
      {provider && (
        <>
          <ResourceDetailsGrid>
            {"provider" in provider
              ? <ResourceDetail label="Provider ID" mono>{provider.provider.providerId}</ResourceDetail>
              : <ResourceDetail label="Account">{subscriptionAccountLabel(provider.grant) || "—"}</ResourceDetail>}
            <ResourceDetail label="Status"><ResourceStatus status={provider.status} /></ResourceDetail>
            <ResourceDetail label="Credential ID" mono>{"provider" in provider ? provider.provider.credentialId : provider.grant.grantId}</ResourceDetail>
            <ResourceDetail label="Connection">{provider.connectionDescription ? `${provider.connection} · ${provider.connectionDescription}` : provider.connection}</ResourceDetail>
            {"provider" in provider && provider.provider.config.type === "modelOAuth" && (
              <ResourceDetail label="OAuth grant" mono>{provider.provider.config.grantId}</ResourceDetail>
            )}
            {"grant" in provider && <>
              <ResourceDetail label="Environment variable" mono>{subscriptionBinding(provider.grant)?.envName ?? "—"}</ResourceDetail>
              <ResourceDetail label="Expires">{formatExpiry(provider.grant.expiresAtMs)}</ResourceDetail>
            </>}
            <ResourceDetail label="Created">{resourceTimestamp(provider.createdAtMs)}</ResourceDetail>
            <ResourceDetail label="Updated">{resourceTimestamp(provider.updatedAtMs)}</ResourceDetail>
          </ResourceDetailsGrid>
          <ModelProviderDetails key={provider.id} universeId={universeId} provider={provider}
            onChanged={onChanged} onClose={() => onOpenChange(false)} />
        </>
      )}
    </ResourceDetailsDialog>
  );
}

function ModelProviderDetails({
  universeId,
  provider,
  onChanged,
  onClose,
}: {
  universeId: string;
  provider: ConnectedModelProvider;
  onChanged: () => void;
  onClose: () => void;
}) {
  const removed = () => {
    onChanged();
    onClose();
  };
  switch (provider.kind) {
    case "openAiApiKey":
    case "anthropicApiKey":
      return (
        <ModelApiKeyDetails
          universeId={universeId}
          provider={provider.provider}
          onChanged={onChanged}
          onRemoved={removed}
        />
      );
    case "openAiCompatible":
      return (
        <OpenAiCompatibleDetails
          universeId={universeId}
          provider={provider.provider}
          onChanged={onChanged}
          onRemoved={removed}
        />
      );
    case "anthropicSubscription":
    case "openAiSubscription":
      return (
        <SubscriptionDetails
          universeId={universeId}
          grant={provider.grant}
          onDisconnected={removed}
        />
      );
  }
}
