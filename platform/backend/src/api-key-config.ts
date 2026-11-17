import type { ApiKeyConfigurationOptions } from "@better-auth/api-key";
import type { ServerEnv } from "./env.js";

export const PERSONAL_KEY_CONFIG = "personal";
export const SERVICE_USER_KEY_CONFIG = "serviceUser";
export const PERSONAL_KEY_PREFIX = "lsp_";
export const SERVICE_USER_KEY_PREFIX = "lsa_";

export function apiKeyConfigurations(env: Pick<ServerEnv, "apiKeyDefaultExpiryDays" | "apiKeyMaxExpiryDays">): ApiKeyConfigurationOptions[] {
  const maximum = env.apiKeyMaxExpiryDays ?? 365;
  const lifetime = Math.min(env.apiKeyDefaultExpiryDays ?? 365, maximum);
  const common = {
    requireName: true, maximumNameLength: 120,
    enableSessionForAPIKeys: false, enableMetadata: false,
    startingCharactersConfig: { shouldStore: true, charactersLength: 12 },
    // This plugin version consumes seconds in createApiKey for both explicit and default expiry.
    keyExpiration: { defaultExpiresIn: lifetime * 86400, minExpiresIn: 1, maxExpiresIn: maximum },
    rateLimit: { enabled: true, timeWindow: 60_000, maxRequests: 1000 },
  };
  return [
    { ...common, configId: PERSONAL_KEY_CONFIG, references: "user", defaultPrefix: PERSONAL_KEY_PREFIX },
    { ...common, configId: SERVICE_USER_KEY_CONFIG, references: "organization", defaultPrefix: SERVICE_USER_KEY_PREFIX },
  ];
}
