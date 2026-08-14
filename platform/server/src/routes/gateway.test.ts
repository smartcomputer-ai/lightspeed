import { describe, expect, it } from "vitest";
import {
  credentialIdConflictMessage,
  environmentSecretGrantParams,
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
