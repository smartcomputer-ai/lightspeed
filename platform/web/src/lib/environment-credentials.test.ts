import { describe, expect, it } from "vitest";
import type { SecretGrant, SecretProvider, SecretsInventory } from "@/api";
import {
  brokeredCredentialNote,
  environmentCredentialAvailable,
  environmentCredentialOptions,
  environmentCredentialSourceFromValue,
  environmentCredentialSourceValue,
} from "./environment-credentials";

function grant(
  grantId: string,
  overrides: Partial<Pick<SecretGrant, "exposure" | "status" | "providerId">> = {},
): SecretGrant {
  return {
    grantId,
    providerId: "environment-secret",
    providerKind: "staticToken",
    status: "active",
    exposure: "retrievable",
    hasAccessToken: true,
    hasRefreshToken: false,
    leaseCount: 0,
    createdAtMs: 1,
    updatedAtMs: 1,
    ...overrides,
  } as SecretGrant;
}

function provider(credentialId: string, config: SecretProvider["config"]): SecretProvider {
  return {
    providerId: credentialId.replace(/^model:/, ""),
    credentialId,
    usableForModels: true,
    providerKind: config.type === "githubApp" ? "gitHubApp" : config.type,
    config,
    hasCredential: true,
    status: "active",
    createdAtMs: 1,
    updatedAtMs: 1,
  };
}

const inventory = (
  grants: SecretGrant[],
  providers: SecretProvider[] = [],
): SecretsInventory => ({ grants, providers });

describe("environment credential sources", () => {
  it("offers only active retrievable credentials", () => {
    const options = environmentCredentialOptions(
      inventory([
        grant("release-token"),
        grant("tracker-token", { exposure: "brokered", providerId: "static" }),
        grant("old-token", { status: "revoked" }),
      ]),
    );
    expect(options.map((option) => option.value)).toEqual(["grant:release-token"]);
  });

  it("offers a provider credential only for a model API key", () => {
    const options = environmentCredentialOptions(
      inventory(
        [],
        [
          provider("model:openai", { type: "modelApiKey" }),
          provider("github", { type: "githubApp", appId: "1", apiBaseUrl: "https://api.github.com" }),
          provider("model:chatgpt", { type: "modelOAuth", grantId: "g" }),
        ],
      ),
    );
    expect(options.map((option) => option.value)).toEqual(["provider:model:openai"]);
  });

  it("says why brokered credentials are missing from the picker", () => {
    expect(brokeredCredentialNote(inventory([grant("release-token")]))).toBeNull();
    expect(brokeredCredentialNote(undefined)).toBeNull();

    const one = brokeredCredentialNote(
      inventory([grant("release-token"), grant("tracker-token", { exposure: "brokered" })]),
    );
    expect(one).toContain("1 credential is not listed");
    expect(one).toContain("retrievable");

    const two = brokeredCredentialNote(
      inventory([
        grant("a", { exposure: "brokered" }),
        grant("b", { exposure: "brokered" }),
        grant("revoked", { exposure: "brokered", status: "revoked" }),
      ]),
    );
    expect(two).toContain("2 credentials are not listed");
  });

  it("marks a binding to a brokered credential as unavailable", () => {
    const secrets = inventory([
      grant("release-token"),
      grant("tracker-token", { exposure: "brokered" }),
    ]);
    expect(
      environmentCredentialAvailable({ type: "authGrant", grantId: "release-token" }, secrets),
    ).toBe(true);
    expect(
      environmentCredentialAvailable({ type: "authGrant", grantId: "tracker-token" }, secrets),
    ).toBe(false);
  });

  it("round-trips the two sources and has no other", () => {
    for (const value of ["grant:release-token", "provider:model:openai"]) {
      expect(environmentCredentialSourceValue(environmentCredentialSourceFromValue(value))).toBe(
        value,
      );
    }
    expect(() => environmentCredentialSourceFromValue("secret:authsec_1")).toThrow();
  });
});
