import type { Environment, McpServer, SecretGrant, SecretProvider } from "@/api";

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

export function environmentStatus(environment: Pick<Environment, "status" | "source" | "desiredPower" | "lastSeenAtMs">): ResourceStatusInfo {
  const { status, source, desiredPower } = environment;
  const observed = status === "ready" ? "running" : status === "offline" ? "stopped"
    : status === "paused" || status === "suspended" ? status : null;
  if (source.type === "provisioned" && observed && observed !== desiredPower) {
    return {
      label: desiredPower === "running" ? "Waking" : "Changing power",
      tone: "pending",
      description: `${sentenceCase(status)} → ${desiredPower}`,
    };
  }
  const description = source.type === "registered"
    ? environment.lastSeenAtMs ? `Last seen ${relativeResourceTime(environment.lastSeenAtMs)}` : "Never seen"
    : undefined;
  return {
    label: status === "booting" ? "Starting" : sentenceCase(status),
    tone: status === "ready" ? "positive" : status === "failed" ? "danger"
      : status === "unknown" ? "warning"
        : ["provisioning", "booting", "closing"].includes(status) ? "pending" : "neutral",
    description,
  };
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
