import { LightspeedClient } from "@lightspeed/agent-client";
import type { BindingAuthConfig, MessagingBridgeConfig } from "./config.js";
import { LightspeedSessionBridge } from "./lightspeed.js";

/// One authenticated gateway connection: the HTTP client with credential
/// headers baked in, and the session bridge wrapping it.
export interface GatewayConnection {
  client: LightspeedClient;
  lightspeed: LightspeedSessionBridge;
}

/// All gateway connections of a bridge process: the default connection plus
/// one per distinct per-binding credential.
export interface GatewayConnections {
  default: GatewayConnection;
  /// Connection per binding-rule id that configures `auth`. Rules sharing
  /// identical credentials share one connection (and therefore one outbox
  /// tailer — two tailers on the same universe outbox would double-deliver).
  byBindingId: ReadonlyMap<string, GatewayConnection>;
  /// Every distinct connection, deduplicated by credential identity. One
  /// outbox tailer runs per entry.
  distinct: readonly GatewayConnection[];
}

export function gatewayHeaders(auth: BindingAuthConfig): Record<string, string> {
  const headers: Record<string, string> = {};
  if (auth.apiKey) {
    headers["authorization"] = `Bearer ${auth.apiKey}`;
  }
  if (auth.universe) {
    headers["x-lightspeed-universe"] = auth.universe;
  }
  return headers;
}

/// Credential identity: rules (and the default config) with the same
/// credential share one connection.
function credentialKey(auth: BindingAuthConfig): string {
  if (auth.apiKey) {
    return `key:${auth.apiKey}`;
  }
  if (auth.universe) {
    return `universe:${auth.universe}`;
  }
  return "default";
}

export function buildGatewayConnections(config: MessagingBridgeConfig): GatewayConnections {
  const byCredential = new Map<string, GatewayConnection>();
  const connectionFor = (auth: BindingAuthConfig): GatewayConnection => {
    const key = credentialKey(auth);
    const existing = byCredential.get(key);
    if (existing) {
      return existing;
    }
    const client = new LightspeedClient({
      endpoint: config.lightspeed.endpoint,
      headers: gatewayHeaders(auth),
    });
    const connection: GatewayConnection = {
      client,
      lightspeed: new LightspeedSessionBridge(client, config.lightspeed),
    };
    byCredential.set(key, connection);
    return connection;
  };

  const defaultConnection = connectionFor({
    apiKey: config.lightspeed.apiKey ?? null,
    universe: config.lightspeed.universe ?? null,
  });
  const byBindingId = new Map<string, GatewayConnection>();
  for (const rule of config.bindings) {
    if (!rule.auth) {
      continue;
    }
    if (rule.id === undefined) {
      // parseBindings enforces this; keep the invariant local too.
      throw new Error("binding rules with auth must have an id");
    }
    byBindingId.set(rule.id, connectionFor(rule.auth));
  }
  return {
    default: defaultConnection,
    byBindingId,
    distinct: [...byCredential.values()],
  };
}
