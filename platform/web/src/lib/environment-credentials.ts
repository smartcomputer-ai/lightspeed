import { useQuery } from "@tanstack/react-query";
import {
  api,
  type EnvironmentCredentialSource,
  type SecretGrant,
  type SecretProvider,
  type SecretsInventory,
} from "@/api";
import { subscriptionBinding } from "@/lib/subscriptions";

/// Shared helpers for choosing an environment credential source from the
/// universe secrets inventory (Environments → Assign credential, and profile
/// provision credentials). Option values are stable strings so `<Select>`
/// can round-trip them.

export interface EnvironmentCredentialOption {
  value: string;
  label: string;
  suggestedEnvName?: string;
}

export function environmentCredentialOptions(
  secrets: SecretsInventory | undefined,
): EnvironmentCredentialOption[] {
  if (!secrets) return [];
  return [
    ...secrets.grants
      .filter((grant) => grant.status === "active")
      .map((grant) => {
        const subscription = subscriptionBinding(grant);
        if (subscription) {
          return {
            value: `grant:${grant.grantId}`,
            label: `${accessGrantName(grant)} · ${subscription.label}`,
            suggestedEnvName: subscription.envName,
          };
        }
        return {
          value: `grant:${grant.grantId}`,
          label: grant.providerId === "environment-secret"
            ? `${accessGrantName(grant)} · Environment secret`
            : `${accessGrantName(grant)} · Access credential`,
          suggestedEnvName: undefined as string | undefined,
        };
      }),
    ...secrets.providers
      .filter((provider) => provider.status === "active" && provider.hasCredential)
      .map((provider) => ({
        value: `provider:${provider.credentialId}`,
        label: `${modelProviderName(provider)} · Model provider API key`,
        suggestedEnvName: undefined as string | undefined,
      })),
  ];
}

export function environmentCredentialSourceFromValue(value: string): EnvironmentCredentialSource {
  if (value.startsWith("grant:")) {
    return { type: "authGrant", grantId: value.slice("grant:".length) };
  }
  if (value.startsWith("provider:")) {
    return { type: "authProviderCredential", providerId: value.slice("provider:".length) };
  }
  if (value.startsWith("secret:")) {
    return { type: "directSecret", secretId: value.slice("secret:".length) };
  }
  throw new Error("invalid environment credential source");
}

/// Inverse of `environmentCredentialSourceFromValue` for editing saved sources.
export function environmentCredentialSourceValue(source: EnvironmentCredentialSource): string {
  if (source.type === "authGrant") return `grant:${source.grantId}`;
  if (source.type === "authProviderCredential") return `provider:${source.providerId}`;
  return `secret:${source.secretId}`;
}

export function environmentCredentialSourceLabel(
  source: EnvironmentCredentialSource,
  secrets: SecretsInventory | undefined,
): string {
  if (source.type === "authGrant") {
    const grant = secrets?.grants.find((candidate) => candidate.grantId === source.grantId);
    return grant ? accessGrantName(grant) : source.grantId;
  }
  if (source.type === "authProviderCredential") {
    const provider = secrets?.providers.find(
      (candidate) => candidate.credentialId === source.providerId,
    );
    return provider ? modelProviderName(provider) : source.providerId;
  }
  return `Direct secret (${source.secretId})`;
}

export function environmentCredentialAvailable(
  source: EnvironmentCredentialSource,
  secrets: SecretsInventory | undefined,
): boolean {
  if (!secrets || source.type === "directSecret") return true;
  if (source.type === "authGrant") {
    return secrets.grants.some(
      (grant) => grant.grantId === source.grantId && grant.status === "active",
    );
  }
  return secrets.providers.some(
    (provider) =>
      provider.credentialId === source.providerId
      && provider.status === "active"
      && provider.hasCredential,
  );
}

export function accessGrantName(grant: SecretGrant): string {
  return grant.displayName ?? grant.subjectHint ?? grant.grantId;
}

export function modelProviderName(provider: SecretProvider): string {
  return provider.displayName ?? provider.providerId;
}


/// The universe secrets inventory (shared query key with the Models and
/// Credentials pages).
export function useSecretsInventory(universeId: string, enabled = true) {
  return useQuery({
    queryKey: ["secrets", universeId],
    queryFn: () => api<SecretsInventory>("GET", `/api/v1/universes/${universeId}/secrets`),
    enabled,
  });
}
