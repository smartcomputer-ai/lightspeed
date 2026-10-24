import { expect, it } from "vitest";
import type { SecretGrant, SecretProvider } from "@/api";
import { connectedModelProviders } from "./use-model-providers";

function provider(providerId: string, config: SecretProvider["config"], extra: Partial<SecretProvider> = {}): SecretProvider {
  return {
    providerId, credentialId: `model:${providerId}`, usableForModels: true,
    providerKind: config.type === "githubApp" ? "gitHubApp" : config.type,
    config, hasCredential: config.type === "modelApiKey", status: "active", createdAtMs: 1, updatedAtMs: 2, ...extra,
  };
}
function grant(grantId: string, extra: Partial<SecretGrant> = {}): SecretGrant {
  return {
    grantId, providerId: grantId, providerKind: "staticBearer", status: "active", exposure: "brokered", createdBy: { kind: "local" },
    hasAccessToken: true, hasRefreshToken: false, leaseCount: 0, createdAtMs: 1, updatedAtMs: 3, ...extra,
  };
}
const endpoint = { baseUrl: "https://llm.example/v1", apiKinds: ["openai:completions" as const] };

it("lists every model provider row, including legacy and OAuth rows no add form creates", () => {
  const rows = connectedModelProviders(
    {
      providers: [
        provider("openai", { type: "modelApiKey" }, { displayName: "Production" }),
        provider("anthropic", { type: "modelApiKey" }, { credentialId: "anthropic", usableForModels: false }),
        provider("deepseek", { type: "modelApiKey", endpoint }),
        provider("ollama", { type: "modelEndpoint", endpoint }),
        provider("vendor", { type: "modelOAuth", grantId: "vendor-login" }),
      ],
      grants: [grant("vendor-login", { providerKind: "modelOAuth", status: "needsReauth" })],
    },
    [
      grant("claude", { displayName: "Lukas · Max", metadata: { subscription: "claudeCode" } }),
      grant("codex", { status: "revoked", metadata: { subscription: "codex" } }),
      grant("plain-token"),
    ],
  );
  expect(rows.map((row) => [row.kind, row.title, row.connection, row.status.label])).toEqual([
    ["anthropicApiKey", "Anthropic", "API key", "Needs attention"],
    ["openAiCompatible", "deepseek", "API key", "Active"],
    ["anthropicSubscription", "Lukas · Max", "Subscription", "Connected"],
    ["openAiCompatible", "ollama", "No authentication", "Active"],
    ["openAiApiKey", "Production", "API key", "Active"],
    ["openAiCompatible", "vendor", "OAuth", "Needs sign-in"],
  ]);
  expect(rows.find((row) => row.title === "Production")).toMatchObject({ subtitle: "openai", createdAtMs: 1, updatedAtMs: 2 });
  expect(rows.find((row) => row.title === "vendor")).toMatchObject({ oauthGrant: { grantId: "vendor-login" } });
});
