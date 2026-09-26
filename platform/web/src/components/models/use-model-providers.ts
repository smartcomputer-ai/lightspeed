import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type SecretGrant, type SecretProvider, type SecretsInventory } from "@/api";
import { subscriptionAccountLabel, subscriptionProviderOf } from "@/lib/subscriptions";
import { modelProviderDefinition, type ModelProviderKind } from "./catalog";

/// One connected model provider as the list shows it. Each variant keeps its
/// source record so the details dialog can render kind-specific content.
export type ConnectedModelProvider = {
  id: string;
  title: string;
  subtitle: string;
  /// The list's Type column.
  type: string;
  status: ModelProviderStatus;
  updatedAtMs: number;
} & (
  | {
      kind: "anthropicSubscription" | "openAiSubscription";
      grant: SecretGrant;
    }
  | {
      kind: "openAiApiKey" | "anthropicApiKey" | "openAiCompatible";
      provider: SecretProvider;
      /// The access credential behind an OAuth-configured provider.
      oauthGrant?: SecretGrant;
    }
);

export type ModelProviderStatus = "active" | "attention" | "disabled";

/// Every model provider row in the universe inventory, including legacy and
/// OAuth-configured rows no add form creates, followed by coding-agent
/// subscriptions that are still connected.
export function connectedModelProviders(
  inventory: SecretsInventory | undefined,
  subscriptions: SecretGrant[] | undefined,
): ConnectedModelProvider[] {
  const grants = inventory?.grants ?? [];
  const providers = (inventory?.providers ?? [])
    .slice()
    .sort((a, b) => a.providerId.localeCompare(b.providerId) || a.credentialId.localeCompare(b.credentialId))
    .map((provider): ConnectedModelProvider => {
      const builtIn = provider.providerId === "openai" || provider.providerId === "anthropic";
      const config = provider.config;
      const oauthGrant = config.type === "modelOAuth"
        ? grants.find((grant) => grant.grantId === config.grantId)
        : undefined;
      const common = {
        id: `model-provider:${provider.credentialId}`,
        status: providerStatus(provider, oauthGrant),
        updatedAtMs: provider.updatedAtMs,
        provider,
        oauthGrant,
      };
      if (builtIn && config.type === "modelApiKey") {
        const kind = provider.providerId === "openai" ? "openAiApiKey" : "anthropicApiKey";
        const name = modelProviderDefinition(kind).name;
        return { ...common, kind, title: name, subtitle: provider.displayName ?? provider.credentialId, type: name };
      }
      return {
        ...common,
        kind: "openAiCompatible",
        title: provider.displayName ?? provider.providerId,
        subtitle: provider.providerId,
        type: providerEndpoint(provider)
          ? modelProviderDefinition("openAiCompatible").name
          : config.type === "modelOAuth"
            ? "OAuth connection"
            : "API key",
      };
    });
  const connectedSubscriptions = (subscriptions ?? [])
    .filter((grant) => grant.status !== "revoked")
    .flatMap((grant): ConnectedModelProvider[] => {
      const provider = subscriptionProviderOf(grant);
      if (!provider) return [];
      const kind: ModelProviderKind =
        provider === "anthropic" ? "anthropicSubscription" : "openAiSubscription";
      const name = modelProviderDefinition(kind).name;
      return [{
        kind,
        id: `subscription:${grant.grantId}`,
        title: grant.displayName ?? name,
        subtitle: subscriptionAccountLabel(grant) || grant.grantId,
        type: name,
        status: grant.status === "active" ? "active" : "attention",
        updatedAtMs: grant.updatedAtMs,
        grant,
      }];
    });
  return [...providers, ...connectedSubscriptions];
}

/// Usable for model calls: an active row under the id the runtime reads,
/// with its credential when it needs one.
function providerStatus(provider: SecretProvider, oauthGrant: SecretGrant | undefined): ModelProviderStatus {
  if (provider.status === "disabled") return "disabled";
  const credentialed =
    provider.config.type === "modelEndpoint"
    || (provider.config.type === "modelOAuth" ? oauthGrant?.status === "active" : provider.hasCredential);
  return provider.status === "active" && provider.usableForModels && credentialed ? "active" : "attention";
}

export function providerEndpoint(provider: SecretProvider) {
  return provider.config.type === "githubApp" ? undefined : (provider.config.endpoint ?? undefined);
}

export function useModelProviders(universeId: string) {
  const subscriptions = useQuery({
    queryKey: ["integrations", "subscriptions", universeId],
    queryFn: () =>
      api<SecretGrant[]>("GET", `/api/v1/universes/${universeId}/integrations/subscriptions`),
  });
  const secrets = useQuery({
    queryKey: ["secrets", universeId],
    queryFn: () => api<SecretsInventory>("GET", `/api/v1/universes/${universeId}/secrets`),
  });
  return {
    connected: connectedModelProviders(secrets.data, subscriptions.data),
    isLoading: subscriptions.isLoading || secrets.isLoading,
    error: subscriptions.error ?? secrets.error ?? null,
  };
}

export function useInvalidateModelProviders(universeId: string) {
  const queryClient = useQueryClient();
  return () =>
    Promise.all([
      queryClient.invalidateQueries({ queryKey: ["integrations"] }),
      queryClient.invalidateQueries({ queryKey: ["secrets", universeId] }),
      queryClient.invalidateQueries({ queryKey: ["models", universeId] }),
    ]);
}
