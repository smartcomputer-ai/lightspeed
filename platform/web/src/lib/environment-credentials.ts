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

/// A process in the environment receives the plain value, so only a
/// credential created as retrievable can be bound. The runtime refuses the
/// rest; the picker leaves them out and says so.
export function isBindableGrant(grant: Pick<SecretGrant, "status" | "exposure">): boolean {
  return grant.status === "active" && grant.exposure === "retrievable";
}

/// Why brokered credentials are missing from the picker, or null when none
/// are: their value stays inside Lightspeed and cannot be handed to a machine.
export function brokeredCredentialNote(secrets: SecretsInventory | undefined): string | null {
  const count = (secrets?.grants ?? []).filter(
    (grant) => grant.status === "active" && grant.exposure !== "retrievable",
  ).length;
  if (count === 0) return null;
  const subject = count === 1 ? "1 credential is" : `${count} credentials are`;
  return `${subject} not listed: only credentials created as retrievable can be assigned, because the process receives the plain value. Exposure is fixed at creation, so add a new retrievable credential to use one here.`;
}

/// Of a provider's stored credentials, only a model API key may be bound.
function isBindableProvider(provider: SecretProvider): boolean {
  return provider.status === "active"
    && provider.hasCredential
    && provider.config.type === "modelApiKey";
}

export function environmentCredentialOptions(
  secrets: SecretsInventory | undefined,
): EnvironmentCredentialOption[] {
  if (!secrets) return [];
  return [
    ...secrets.grants
      .filter(isBindableGrant)
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
      .filter(isBindableProvider)
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
  throw new Error("invalid environment credential source");
}

/// Inverse of `environmentCredentialSourceFromValue` for editing saved sources.
export function environmentCredentialSourceValue(source: EnvironmentCredentialSource): string {
  if (source.type === "authGrant") return `grant:${source.grantId}`;
  return `provider:${source.providerId}`;
}

export function environmentCredentialSourceLabel(
  source: EnvironmentCredentialSource,
  secrets: SecretsInventory | undefined,
): string {
  if (source.type === "authGrant") {
    const grant = secrets?.grants.find((candidate) => candidate.grantId === source.grantId);
    return grant ? accessGrantName(grant) : source.grantId;
  }
  const provider = secrets?.providers.find(
    (candidate) => candidate.credentialId === source.providerId,
  );
  return provider ? modelProviderName(provider) : source.providerId;
}

export function environmentCredentialAvailable(
  source: EnvironmentCredentialSource,
  secrets: SecretsInventory | undefined,
): boolean {
  if (!secrets) return true;
  if (source.type === "authGrant") {
    return secrets.grants.some(
      (grant) => grant.grantId === source.grantId && isBindableGrant(grant),
    );
  }
  return secrets.providers.some(
    (provider) => provider.credentialId === source.providerId && isBindableProvider(provider),
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
