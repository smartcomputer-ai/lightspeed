import {
  LightspeedClient,
  LightspeedRpcError,
  type CallOptions,
  type Method,
  type MethodParams,
  type MethodResult,
} from "@lightspeed-ai/agent-client";
import { roleAtLeast, type UniverseRole } from "@lightspeed/platform-shared";
import type { ServerEnv } from "./env.js";
import { METHOD_ROLES, SESSION_TARGET_METHODS } from "./routes/method-roles.js";

export class GatewayUnconfigured extends Error {
  constructor() { super("runtime endpoint and Platform deployment key required"); }
}

/// A refusal the Platform makes itself, before the request reaches core.
export class GateRefusal extends Error {
  constructor(readonly status: 403 | 404, message: string) { super(message); }
}

/// Who a universe request is for: the signed-in user and the role they hold
/// in the universe. Platform admins act as `admin` everywhere.
export interface Member {
  userId: string;
  role: UniverseRole;
}

/// Methods only a session's creator, or an admin, may call.
const CREATOR_METHODS: ReadonlySet<string> = new Set(["session/share", "session/delete"]);
/// Methods that may name a session that does not exist yet.
const CREATION_METHODS: ReadonlySet<string> = new Set(["session/start", "session/managed/start"]);

/// Deployment methods, with the Platform's deployment key and no universe or
/// actor. Callers check that the user is a platform admin.
export function deploymentClient(env: ServerEnv, endpoint?: string | null): LightspeedClient {
  return new LightspeedClient(clientOptions(env, endpoint, {}));
}

/// Universe methods on a member's behalf: the deployment key names the
/// universe and asserts the user as the actor, and every call passes the
/// member gate first.
export function memberClient(
  env: ServerEnv,
  universe: { lightspeedUniverseId: string; gatewayUrl?: string | null },
  member: Member,
): LightspeedClient {
  return new MemberClient(
    clientOptions(env, universe.gatewayUrl, {
      "x-lightspeed-universe": universe.lightspeedUniverseId,
      "x-lightspeed-actor": member.userId,
    }),
    member,
  );
}

function clientOptions(env: ServerEnv, endpoint: string | null | undefined, headers: Record<string, string>) {
  if (!env.lightspeedApiUrl || !env.lightspeedApiKey) throw new GatewayUnconfigured();
  // The key only ever goes to the configured runtime.
  const resolved = new URL(endpoint ?? env.lightspeedApiUrl);
  if (resolved.href !== new URL(env.lightspeedApiUrl).href || resolved.username || resolved.password) throw new GatewayUnconfigured();
  return {
    endpoint: resolved.href,
    fetch: (input: Parameters<typeof fetch>[0], init?: RequestInit) => globalThis.fetch(input, { ...init, redirect: "error" }),
    headers: { authorization: `Bearer ${env.lightspeedApiKey}`, ...headers },
  };
}

/// The member gate, in order: the member's role meets the method's role; a
/// session a method names is shared with the universe or was created by the
/// member (creator-only methods need the creator), unless the member is an
/// admin; a member's session list is narrowed to what they may see.
class MemberClient extends LightspeedClient {
  constructor(options: ConstructorParameters<typeof LightspeedClient>[0], private readonly member: Member) {
    super(options);
  }

  override async call<M extends Method>(method: M, params: MethodParams<M>, options: CallOptions = {}): Promise<MethodResult<M>> {
    const required = METHOD_ROLES[method];
    if (!required) throw new GateRefusal(403, `${method} is not a member method`);
    if (!roleAtLeast(this.member.role, required)) throw new GateRefusal(403, `${required} role required`);
    const admin = this.member.role === "admin";
    if (!admin && SESSION_TARGET_METHODS.has(method)) {
      await this.requireSession(method, (params as { sessionId?: string }).sessionId);
    }
    if (!admin && method === "session/list") {
      params = { ...params, visibleTo: this.member.userId } as MethodParams<M>;
    }
    return super.call(method, params, options);
  }

  private async requireSession(method: string, sessionId: string | undefined): Promise<void> {
    if (!sessionId) {
      if (CREATION_METHODS.has(method)) return;
      throw new GateRefusal(404, "session not found");
    }
    let access;
    try {
      access = (await super.call("session/read", { sessionId, runLimit: 1 })).result.session.access;
    } catch (error) {
      if (error instanceof LightspeedRpcError && error.kind === "not_found" && CREATION_METHODS.has(method)) return;
      throw error;
    }
    const creator = access.createdBy?.kind === "actor" && access.createdBy.id === this.member.userId;
    const visible = CREATOR_METHODS.has(method) ? creator : creator || access.visibility === "universe";
    if (!visible) throw new GateRefusal(404, "session not found");
  }
}
