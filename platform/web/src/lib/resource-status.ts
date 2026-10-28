import type { Environment, McpServer, SecretGrant, SecretProvider } from "@/api";
import { lifecycleMessage } from "@/lib/environments";

export interface ResourceStatusInfo {
  label: string;
  tone: "positive" | "neutral" | "warning" | "danger" | "pending";
  description?: string;
}

export function modelProviderStatus(provider: SecretProvider, grant?: Pick<SecretGrant, "status">): ResourceStatusInfo {
  if (provider.status === "disabled") return { label: "Disabled", tone: "neutral" };
  if (!provider.usableForModels) return { label: "Needs attention", tone: "warning", description: "Replace this legacy credential" };
  if (provider.config.type === "modelOAuth") {
    if (!grant || grant.status === "revoked") return { label: "Needs sign-in", tone: "warning", description: "OAuth connection is missing" };
    if (grant.status !== "active") return subscriptionStatus(grant);
  } else if (provider.config.type !== "modelEndpoint" && !provider.hasCredential) {
    return { label: "Needs API key", tone: "warning" };
  }
  if (provider.status !== "active") return { label: "Needs configuration", tone: "warning" };
  return { label: "Active", tone: "positive" };
}

export function subscriptionStatus(grant: Pick<SecretGrant, "status">): ResourceStatusInfo {
  switch (grant.status) {
    case "active": return { label: "Connected", tone: "positive" };
    case "needsReauth": return { label: "Needs sign-in", tone: "warning", description: "Sign in again to reconnect" };
    case "failed": return { label: "Connection failed", tone: "danger" };
    case "revoked": return { label: "Revoked", tone: "neutral" };
  }
}

export function mcpServerStatus(server: Pick<McpServer, "status" | "authPolicy">): ResourceStatusInfo {
  switch (server.status) {
    case "active": return { label: "Active", tone: "positive" };
    case "disabled": return { label: "Disabled", tone: "neutral" };
    case "unverified": return { label: "Unverified", tone: "warning" };
    case "needsAuthConfig": return {
      label: server.authPolicy.type.includes("OAuth") ? "Needs sign-in" : "Needs credential",
      tone: "warning",
    };
  }
}

const ENVIRONMENT_STATUS: Record<Environment["status"], Omit<ResourceStatusInfo, "description">> = {
  provisioning: { label: "Provisioning", tone: "pending" },
  ready: { label: "Ready", tone: "positive" },
  paused: { label: "Paused", tone: "neutral" },
  suspended: { label: "Suspended", tone: "neutral" },
  stopped: { label: "Stopped", tone: "neutral" },
  away: { label: "Disconnected", tone: "neutral" },
  unknown: { label: "Unknown", tone: "warning" },
  closing: { label: "Closing", tone: "pending" },
  closed: { label: "Closed", tone: "neutral" },
  failed: { label: "Failed", tone: "danger" },
};

/// The derived display status, explained by the lifecycle message (why it
/// failed, what provisioning or closing waits on, why it closed) or, for an
/// open environment, by what its source last reported.
export function environmentStatus(
  environment: Pick<Environment, "status" | "state" | "stateMessage" | "closeReason" | "source">,
): ResourceStatusInfo {
  const { source } = environment;
  const status = ENVIRONMENT_STATUS[environment.status] ?? { label: sentenceCase(environment.status), tone: "neutral" };
  const description = lifecycleMessage(environment)
    ?? (environment.state !== "open" ? undefined
      : source.type === "registered" && !source.connected ? `Last seen ${relativeResourceTime(source.lastSeenAtMs)}`
        : source.type === "provisioned" ? source.observed?.message ?? undefined
          : undefined);
  return description ? { ...status, description } : { ...status };
}

function sentenceCase(value: string) {
  return value.charAt(0).toUpperCase() + value.slice(1);
}

export function relativeResourceTime(ms: number): string {
  const seconds = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (seconds < 60) return "just now";
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  return `${Math.floor(seconds / 86400)}d ago`;
}

export function resourceTimestamp(ms: number): string {
  return new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(new Date(ms));
}
