import { expect, it } from "vitest";
import type { Environment, McpServer, SecretProvider } from "@/api";
import { environmentStatus, mcpServerStatus, modelProviderStatus, subscriptionStatus } from "./resource-status";

const provider: SecretProvider = {
  providerId: "openai", credentialId: "model:openai", usableForModels: true, providerKind: "modelApiKey",
  config: { type: "modelApiKey" }, status: "active", hasCredential: true, createdAtMs: 1, updatedAtMs: 1,
};

it("explains missing model credentials and gives disabled state precedence", () => {
  expect(modelProviderStatus({ ...provider, hasCredential: false })).toMatchObject({ label: "Needs API key", tone: "warning" });
  expect(modelProviderStatus({ ...provider, hasCredential: false, status: "disabled" })).toEqual({ label: "Disabled", tone: "neutral" });
  expect(modelProviderStatus({ ...provider, config: { type: "modelEndpoint", endpoint: { baseUrl: "http://localhost:11434/v1", apiKinds: ["openai:completions"] } }, hasCredential: false })).toEqual({ label: "Active", tone: "positive" });
});

it("keeps configuration readiness distinct from failed or revoked OAuth connections", () => {
  const oauth: SecretProvider = { ...provider, config: { type: "modelOAuth", grantId: "login" } };
  expect(modelProviderStatus(oauth)).toMatchObject({ label: "Needs sign-in", tone: "warning" });
  expect(modelProviderStatus(oauth, { status: "needsReauth" })).toMatchObject({ label: "Needs sign-in", description: "Sign in again to reconnect" });
  expect(subscriptionStatus({ status: "failed" })).toEqual({ label: "Connection failed", tone: "danger" });
  expect(subscriptionStatus({ status: "revoked" })).toEqual({ label: "Revoked", tone: "neutral" });
});

it("labels environment display status and explains it with the lifecycle message", () => {
  const machine: Parameters<typeof environmentStatus>[0] = {
    source: { type: "provisioned", providerId: "incus", templateId: "dev" }, state: "open", status: "ready",
  };
  expect(environmentStatus(machine)).toEqual({ label: "Ready", tone: "positive" });
  expect(environmentStatus({ ...machine, status: "paused" })).toEqual({ label: "Paused", tone: "neutral" });
  expect(environmentStatus({ ...machine, state: "provisioning", status: "provisioning", stateMessage: "Waiting for the provider" }))
    .toEqual({ label: "Provisioning", tone: "pending", description: "Waiting for the provider" });
  expect(environmentStatus({ ...machine, state: "failed", status: "failed", stateMessage: "Image missing" }))
    .toEqual({ label: "Failed", tone: "danger", description: "Image missing" });
  expect(environmentStatus({ ...machine, state: "closed", status: "closed", closeReason: "requested" }))
    .toEqual({ label: "Closed", tone: "neutral", description: "Closed on request" });
  expect(environmentStatus({
    ...machine,
    status: "unknown",
    source: { ...machine.source, type: "provisioned", providerId: "incus", templateId: "dev", observed: { phase: "unknown", observedAtMs: 1, message: "Host unreachable" } },
  })).toEqual({ label: "Unknown", tone: "warning", description: "Host unreachable" });
});

it("shows when a disconnected registered daemon was last seen", () => {
  const source: Environment["source"] = {
    type: "registered", daemonId: "daemon", registrationKeyId: "key", identityMode: "persistent",
    connected: false, connectionChangedAtMs: Date.now() - 120_000, lastSeenAtMs: Date.now() - 120_000,
  };
  expect(environmentStatus({ source, state: "open", status: "away" }))
    .toEqual({ label: "Disconnected", tone: "neutral", description: "Last seen 2m ago" });
  expect(environmentStatus({ source: { ...source, connected: true }, state: "open", status: "ready" }))
    .toEqual({ label: "Ready", tone: "positive" });
});

it("distinguishes signing in from binding a bearer credential", () => {
  const server: Pick<McpServer, "status" | "authPolicy"> = { status: "needsAuthConfig", authPolicy: { type: "requiredOAuth" } };
  expect(mcpServerStatus(server)).toEqual({ label: "Needs sign-in", tone: "warning" });
  expect(mcpServerStatus({ ...server, authPolicy: { type: "requiredBearer" } })).toEqual({ label: "Needs credential", tone: "warning" });
});
