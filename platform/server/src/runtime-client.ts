import { AsyncLocalStorage } from "node:async_hooks";
import { LightspeedClient, type EffectiveAccess } from "@lightspeed-ai/agent-client";
import type { ServerEnv } from "./env.js";

export const requestIdentity = new AsyncLocalStorage<EffectiveAccess>();

export function actingPrincipal(): string {
  const identity = requestIdentity.getStore();
  if (!identity) throw new Error("authenticated Platform user context required");
  return identity.principal.id;
}

export class GatewayUnconfigured extends Error {
  constructor() { super("runtime endpoint and Platform service credential required"); }
}

/** Unasserted service authority is reserved for login-adapter provisioning. */
export function provisioningClient(env: ServerEnv) {
  return runtimeClient(env, null);
}

export function userClient(env: ServerEnv, principalId: string, endpoint?: string | null, universeId?: string) {
  if (!principalId) throw new Error("canonical user principal required");
  return runtimeClient(env, principalId, endpoint, universeId);
}

function runtimeClient(env: ServerEnv, principalId: string | null, endpoint?: string | null, universeId?: string) {
  if (!env.lightspeedApiUrl || !env.lightspeedApiKey) throw new GatewayUnconfigured();
  const resolved = new URL(endpoint ?? env.lightspeedApiUrl);
  if (resolved.href !== new URL(env.lightspeedApiUrl).href || resolved.username || resolved.password) throw new GatewayUnconfigured();
  return new LightspeedClient({
    endpoint: resolved.href,
    fetch: (input, init) => globalThis.fetch(input, { ...init, redirect: "error" }),
    headers: {
      authorization: `Bearer ${env.lightspeedApiKey}`,
      ...(principalId ? { "x-lightspeed-principal": `user:${principalId}` } : {}),
      ...(universeId ? { "x-lightspeed-universe": universeId } : {}),
    },
  });
}
