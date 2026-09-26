import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
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
}: {
  universeId: string;
  provider: ConnectedModelProvider | null;
  onOpenChange: (open: boolean) => void;
  onChanged: () => void;
}) {
  const definition = provider ? modelProviderDefinition(provider.kind) : null;
  return (
    <Dialog open={provider !== null} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-2xl">
        {provider && definition && (
          <>
            <DialogHeader>
              <DialogTitle className="flex items-center gap-2">
                <definition.Logo size={18} />
                {provider.title}
              </DialogTitle>
            </DialogHeader>
            <ModelProviderDetails
              universeId={universeId}
              provider={provider}
              onChanged={onChanged}
              onClose={() => onOpenChange(false)}
            />
          </>
        )}
      </DialogContent>
    </Dialog>
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
          oauthGrant={provider.oauthGrant}
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
