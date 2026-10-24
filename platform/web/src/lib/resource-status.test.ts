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

it("explains power convergence even when the current machine is still ready", () => {
  const machine: Pick<Environment, "source" | "status" | "desiredPower"> = {
    source: { type: "provisioned", bindingId: "binding", providerId: "incus" }, status: "ready", desiredPower: "paused",
  };
  expect(environmentStatus(machine)).toMatchObject({ label: "Changing power", tone: "pending", description: "Ready → paused" });
  expect(environmentStatus({ ...machine, status: "paused", desiredPower: "running" })).toMatchObject({ label: "Waking", tone: "pending" });
  expect(environmentStatus({ ...machine, status: "paused" })).toMatchObject({ label: "Paused", tone: "neutral" });
});

it("distinguishes signing in from binding a bearer credential", () => {
  const server: Pick<McpServer, "status" | "authPolicy"> = { status: "needsAuthConfig", authPolicy: { type: "requiredOAuth" } };
  expect(mcpServerStatus(server)).toEqual({ label: "Needs sign-in", tone: "warning" });
  expect(mcpServerStatus({ ...server, authPolicy: { type: "requiredBearer" } })).toEqual({ label: "Needs credential", tone: "warning" });
});
