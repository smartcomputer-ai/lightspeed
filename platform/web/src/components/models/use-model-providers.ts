import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type SecretGrant, type SecretProvider, type SecretsInventory } from "@/api";
import { subscriptionAccountLabel, subscriptionProviderOf } from "@/lib/subscriptions";
import type { ModelProviderKind } from "./catalog";
import { modelProviderStatus, subscriptionStatus, type ResourceStatusInfo } from "@/lib/resource-status";

/// One connected model provider as the list shows it. Each variant keeps its
/// source record so the details dialog can render kind-specific content.
export type ConnectedModelProvider = {
  id: string;
  title: string;
  subtitle: string;
  subtitleMono: boolean;
  connection: string;
  connectionDescription?: string;
  status: ResourceStatusInfo;
  createdAtMs: number;
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

/// Every model provider row in the universe inventory, including legacy and
/// OAuth-configured rows no add form creates, and connected coding-agent
/// subscriptions, sorted by their displayed name.
export function connectedModelProviders(
  inventory: SecretsInventory | undefined,
  subscriptions: SecretGrant[] | undefined,
): ConnectedModelProvider[] {
  const grants = inventory?.grants ?? [];
  const providers = (inventory?.providers ?? [])
    .slice()
    .map((provider): ConnectedModelProvider => {
      const builtIn = provider.providerId === "openai" || provider.providerId === "anthropic";
      const config = provider.config;
      const oauthGrant = config.type === "modelOAuth"
        ? grants.find((grant) => grant.grantId === config.grantId)
        : undefined;
      const common = {
        id: `model-provider:${provider.credentialId}`,
        status: modelProviderStatus(provider, oauthGrant),
        connection: config.type === "modelOAuth" ? "OAuth" : config.type === "modelEndpoint" ? "No authentication" : "API key",
        subtitleMono: true,
        createdAtMs: provider.createdAtMs,
        updatedAtMs: provider.updatedAtMs,
        provider,
        oauthGrant,
      };
      if (builtIn && config.type === "modelApiKey") {
        const kind = provider.providerId === "openai" ? "openAiApiKey" : "anthropicApiKey";
        const name = provider.providerId === "openai" ? "OpenAI" : "Anthropic";
        return { ...common, kind, title: provider.displayName || name, subtitle: provider.providerId };
      }
      return {
        ...common,
        kind: "openAiCompatible",
        title: provider.displayName ?? provider.providerId,
        subtitle: provider.providerId,
      };
    });
  const connectedSubscriptions = (subscriptions ?? [])
    .filter((grant) => grant.status !== "revoked")
    .flatMap((grant): ConnectedModelProvider[] => {
      const provider = subscriptionProviderOf(grant);
      if (!provider) return [];
      const kind: ModelProviderKind =
        provider === "anthropic" ? "anthropicSubscription" : "openAiSubscription";
      const name = provider === "anthropic" ? "Claude Code" : "Codex";
      const account = subscriptionAccountLabel(grant);
      return [{
        kind,
        id: `subscription:${grant.grantId}`,
        title: grant.displayName || name,
        subtitle: account || grant.grantId,
        subtitleMono: !account,
        connection: "Subscription",
        connectionDescription: name,
        status: subscriptionStatus(grant),
        createdAtMs: grant.createdAtMs,
        updatedAtMs: grant.updatedAtMs,
        grant,
      }];
    });
  return [...providers, ...connectedSubscriptions].sort((a, b) => a.title.localeCompare(b.title) || a.id.localeCompare(b.id));
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
    hasData: subscriptions.data !== undefined || secrets.data !== undefined,
    isComplete: subscriptions.data !== undefined && secrets.data !== undefined,
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
