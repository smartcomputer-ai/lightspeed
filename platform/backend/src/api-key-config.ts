import type { ApiKeyConfigurationOptions } from "@better-auth/api-key";
import type { ServerEnv } from "./env.js";

export const PERSONAL_KEY_CONFIG = "personal";
export const SERVICE_USER_KEY_CONFIG = "serviceUser";
export const PERSONAL_KEY_PREFIX = "lsp_";
export const SERVICE_USER_KEY_PREFIX = "lsa_";

/// Expiry policy lives in the credential routes: the plugin gets no default
/// (a null expiry would otherwise silently become it) and only bounds an
/// explicit expiry by the deployment maximum.
export function apiKeyConfigurations(env: Pick<ServerEnv, "apiKeyMaxExpiryDays">): ApiKeyConfigurationOptions[] {
  const common = {
    requireName: true, maximumNameLength: 120,
    enableSessionForAPIKeys: false, enableMetadata: false,
    startingCharactersConfig: { shouldStore: true, charactersLength: 12 },
    keyExpiration: { defaultExpiresIn: null, minExpiresIn: 1, maxExpiresIn: env.apiKeyMaxExpiryDays ?? 3650 },
    rateLimit: { enabled: true, timeWindow: 60_000, maxRequests: 1000 },
  };
  return [
    { ...common, configId: PERSONAL_KEY_CONFIG, references: "user", defaultPrefix: PERSONAL_KEY_PREFIX },
    { ...common, configId: SERVICE_USER_KEY_CONFIG, references: "organization", defaultPrefix: SERVICE_USER_KEY_PREFIX },
  ];
}
