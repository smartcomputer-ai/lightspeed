import { describe, expect, it } from "vitest";
import {
  credentialIdConflictMessage,
  environmentSecretGrantParams,
  gitHubAppProviderId,
  mcpOAuthFlowCompletionError,
  mcpServerInputWithOAuthGrant,
  modelProviderCredentialId,
  modelProviderCredentialView,
} from "./gateway.js";

describe("model provider credential ids", () => {
  it("namespaces friendly model provider ids exactly once", () => {
    expect(modelProviderCredentialId("openai")).toBe("model:openai");
    expect(modelProviderCredentialId("model:anthropic")).toBe("model:anthropic");
  });

  it("presents namespaced rows as friendly provider ids", () => {
    expect(
      modelProviderCredentialView({
        providerId: "model:openai",
        config: { type: "modelApiKey" },
      }),
    ).toMatchObject({
      providerId: "openai",
      credentialId: "model:openai",
      usableForModels: true,
    });
  });

  it("marks legacy unnamespaced rows as unusable", () => {
    expect(
      modelProviderCredentialView({
        providerId: "openai",
        config: { type: "modelApiKey" },
      }),
    ).toMatchObject({
      providerId: "openai",
      credentialId: "openai",
      usableForModels: false,
    });
  });
});

describe("custom access credential ids", () => {
  it("explains that revoked ids are terminal", () => {
    expect(credentialIdConflictMessage("deploy-token", "revoked")).toContain(
      "revoked access credential and cannot be reused",
    );
  });

  it("distinguishes an active duplicate", () => {
    expect(credentialIdConflictMessage("deploy-token", "active")).toContain(
      'already belongs to an access credential with status "active"',
    );
  });
});

describe("environment secrets", () => {
  it("preserves multiline values exactly and marks their purpose", () => {
    const privateKey = "-----BEGIN OPENSSH PRIVATE KEY-----\nline-1\nline-2\n-----END OPENSSH PRIVATE KEY-----\n";

    expect(
      environmentSecretGrantParams({
        grantId: "px-dev-ssh-key",
        displayName: "px-dev SSH key",
        value: privateKey,
      }),
    ).toEqual({
      grantId: "px-dev-ssh-key",
      providerId: "environment-secret",
      displayName: "px-dev SSH key",
      token: privateKey,
    });
  });
});

describe("github app provider ids", () => {
  it("derives a stable provider id from the numeric App ID", () => {
    expect(gitHubAppProviderId("123456")).toBe("github-app:123456");
    expect(gitHubAppProviderId(" 123456 ")).toBe("github-app:123456");
  });
});

describe("MCP OAuth completion", () => {
  const flow = {
    flowId: "authflow_1",
    clientId: "mcp:github",
    providerId: "github",
    status: "pending" as const,
    expiresAtMs: 2_000,
    createdAtMs: 1_000,
    updatedAtMs: 1_000,
  };

  it("requires a completed flow with a minted grant", () => {
    expect(mcpOAuthFlowCompletionError({ flow })).toContain("still pending");
    expect(mcpOAuthFlowCompletionError({
      flow: { ...flow, status: "failed", error: "access denied" },
    })).toContain("access denied");
    expect(mcpOAuthFlowCompletionError({
      flow: { ...flow, status: "completed", grantId: "authgrant_1" },
    })).toBeNull();
  });

  it("binds the grant without losing the latest server document", () => {
    const input = mcpServerInputWithOAuthGrant({
      serverId: "github",
      displayName: "GitHub",
      serverUrl: "https://api.githubcopilot.com/mcp",
      defaultServerLabel: "github",
      description: "Current description",
      allowedTools: ["search"],
      execution: "provider",
      exposure: "inject",
      approval: "never",
      deferLoading: true,
      allowPrivateNetwork: false,
      authPolicy: {
        type: "requiredOAuth",
        resource: "https://api.githubcopilot.com/mcp",
        scopes_default: ["repo"],
      },
      status: "needsAuthConfig",
      revision: 3,
      createdAtMs: 1_000,
      updatedAtMs: 2_000,
    }, "authgrant_1");

    expect(input).toMatchObject({
      description: "Current description",
      allowedTools: ["search"],
      credential: { type: "authGrant", grantId: "authgrant_1" },
      status: "active",
    });
    expect(input).not.toHaveProperty("revision");
  });
});

describe("external environment request ids", () => {
  it("derives a stable id-safe request id from the endpoint", async () => {
    const { externalEnvironmentRequestId } = await import("./gateway.js");
    expect(externalEnvironmentRequestId("ws://127.0.0.1:19091/")).toBe("external-127-0-0-1-19091");
    expect(externalEnvironmentRequestId("wss://envd.example.com/ws")).toBe(
      "external-envd-example-com-ws",
    );
    expect(externalEnvironmentRequestId("ws://127.0.0.1:19091")).toBe(
      externalEnvironmentRequestId("ws://127.0.0.1:19091/"),
    );
  });
});
