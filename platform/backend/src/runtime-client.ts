import {
  LightspeedClient,
  LightspeedApiError,
  type CallOptions,
  type ClientOperationId,
  type ClientOperationParams,
  type ClientOperationResult,
  type OperationParams,
  type OperationResult,
  type UniverseWatchParams,
  type WatchKind,
} from "@lightspeed-ai/sdk";
import {
  FEATURES,
  canCloseSession,
  canDeleteSession,
  effectiveFeatures,
  featureOfMethod,
  type FeatureKey,
  type FeatureStates,
  type UniverseAction,
} from "@lightspeed-ai/platform-shared";
import type { ServerEnv } from "./env.js";
import { METHOD_ACTIONS, SESSION_TARGET_OPERATIONS } from "./routes/method-actions.js";

export class RuntimeUnconfigured extends Error {
  constructor() { super("runtime endpoint and Platform runtime token required"); }
}

/// A refusal the Platform makes itself, before the request reaches core.
export class GateRefusal extends Error {
  constructor(readonly status: 400 | 403 | 404 | 409, message: string) { super(message); }
}

/// The admitted human or service user actor and the universe actions it
/// holds: the union of every role it has there, including Owner through
/// organization or deployment administration.
export interface UniverseCaller {
  actorId: string;
  actions: ReadonlySet<UniverseAction>;
}

/// Methods only a session's creator or a universe Owner may call.
const CREATOR_OPERATIONS: ReadonlySet<string> = new Set(["session/share"]);
/// Methods that may name a session that does not exist yet.
const CREATION_OPERATIONS: ReadonlySet<string> = new Set(["session/start"]);

/// Deployment methods, with the Platform's runtime token and no universe or
/// actor. Callers must authorize the deployment operation first.
export function deploymentClient(env: ServerEnv): LightspeedClient {
  return new LightspeedClient(clientOptions(env, {}));
}

/// The refusal for a feature that is off in the universe, or null while it
/// is on. `features` is what the universe stored. The universe gate applies
/// this to every call; a route asks directly when it does work of its own
/// (an outbound request, a stored credential) before its first gated call.
export function featureRefusal(features: unknown, feature: FeatureKey): GateRefusal | null {
  return featureOff(effectiveFeatures(features), feature);
}

function featureOff(features: FeatureStates, feature: FeatureKey): GateRefusal | null {
  return features[feature] ? null : new GateRefusal(403, `${FEATURES[feature].label} are turned off in this universe`);
}

/// Universe operations on a principal's behalf: the runtime token selects the
/// universe and asserts its actor identity; every call passes the role gate.
/// `features` contains stored overrides; omitted features use their defaults.
export function universeClient(
  env: ServerEnv,
  universe: { lightspeedUniverseId: string; features?: unknown },
  caller: UniverseCaller,
): UniverseClient {
  return new UniverseClient(
    { ...clientOptions(env, {"x-lightspeed-actor": caller.actorId}), universe: universe.lightspeedUniverseId },
    caller,
    effectiveFeatures(universe.features),
  );
}

export function clientOptions(env: ServerEnv, headers: Record<string, string>) {
  if (!env.lightspeedApiUrl || !env.lightspeedApiToken) throw new RuntimeUnconfigured();
  // The token only ever goes to the configured runtime.
  const resolved = new URL(env.lightspeedApiUrl);
  if (resolved.username || resolved.password) throw new RuntimeUnconfigured();
  return {
    endpoint: resolved.href,
    fetch: (input: Parameters<typeof fetch>[0], init?: RequestInit) => globalThis.fetch(input, { ...init, redirect: "error" }),
    headers: { authorization: `Bearer ${env.lightspeedApiToken}`, ...headers },
  };
}

/// Admission checks feature switches, the method's universe action, and
/// session visibility. A caller holding `accessPrivateSessions` reaches every
/// session; others see their own or shared sessions, with creator-only
/// operations restricted to the creator.
export class UniverseClient extends LightspeedClient {
  constructor(
    options: ConstructorParameters<typeof LightspeedClient>[0],
    private readonly caller: UniverseCaller,
    private readonly features: FeatureStates,
  ) {
    super(options);
  }

  authorizeWatch(params: UniverseWatchParams = {}): UniverseWatchParams {
    if (!this.can("read")) throw new GateRefusal(403, "universe permission required: read");
    const requested = params.kinds ?? ["session", "bot", "environment", "workspace"];
    const kinds: WatchKind[] = requested.filter(kind => kind !== "bot" || this.features.bots);
    if (!kinds.length) throw new GateRefusal(403, "no enabled collections requested");
    return {
      kinds,
      ...(this.can("accessPrivateSessions") ? (params.visibleTo ? { visibleTo: params.visibleTo } : {}) : { visibleTo: this.caller.actorId }),
    };
  }

  override async openWatch(params: UniverseWatchParams = {}, options: CallOptions = {}): Promise<Response> {
    return super.openWatch(this.authorizeWatch(params), options);
  }

  private can(action: UniverseAction): boolean {
    return this.caller.actions.has(action);
  }

  needsPolicyBody(operation: string): boolean {
    return (operation === "session/start" && !(this.can("setSessionRetention") && this.can("configureSession")))
      || (operation === "session/runs/start" && !this.can("configureSession"));
  }

  override async openDownload(operation: "blobs/read" | "vfs/workspaces/files/read", params: OperationParams<typeof operation>, options: CallOptions = {}): Promise<Response> {
    await this.authorizeOperation(operation, params as unknown as Record<string, unknown>, options);
    return super.openDownload(operation, params, options);
  }

  override async call<M extends ClientOperationId>(method: M, params: ClientOperationParams<M>, options: CallOptions = {}): Promise<ClientOperationResult<M>> {
    if (method.startsWith("platform/")) throw new GateRefusal(403, "Platform operations require the native organization authorization gate");
    const authorized = await this.authorizeOperation(method, params as unknown as Record<string, unknown>, options);
    return authorized.response !== undefined ? authorized.response as ClientOperationResult<M> : super.call(method, authorized.params as ClientOperationParams<M>, options);
  }

  async authorizeOperation(method: string, params: Record<string, unknown>, options: CallOptions = {}): Promise<{ params: Record<string, unknown>; response?: OperationResult<"transcriptions/read"> }> {
    const required = METHOD_ACTIONS[method];
    if (!required) throw new GateRefusal(403, `${method} is not a universe operation`);
    const feature = featureOfMethod(method);
    const off = feature && featureOff(this.features, feature);
    if (off) throw off;
    if (!this.can(required)) throw new GateRefusal(403, `universe permission required: ${required}`);
    if (!this.can("configureSession")) {
      if (method === "session/start") requirePreparedSession(params as unknown as OperationParams<"session/start">);
      if (method === "session/runs/start" && (params as unknown as OperationParams<"session/runs/start">).config != null) {
        throw new GateRefusal(403, "universe permission required for run configuration overrides: configureSession");
      }
    }
    // Draft transcriptions belong only to their creator, including for Owners.
    if (method === "transcriptions/read" || method === "transcriptions/cancel") {
      const transcriptionId = (params as unknown as OperationParams<"transcriptions/read">).transcriptionId;
      const response = await super.call("transcriptions/read", { transcriptionId }, options);
      const creator = response.transcription.createdBy;
      if (creator?.kind !== "actor" || creator.id !== this.caller.actorId) {
        throw new GateRefusal(403, "transcription belongs to another actor");
      }
      if (method === "transcriptions/read") return { params, response };
    }
    const privileged = this.can("accessPrivateSessions");
    if (method === "session/delete") {
      // Permanent deletion must use the separately authorized, audited purge route.
      params = { ...params, softDelete: true, ...(!privileged ? { sharedOnly: true } : {}) };
    }
    if (!this.can("setSessionRetention") && CREATION_OPERATIONS.has(method)) {
      const start = params as { deleteAfterCloseMs?: number | null };
      if (start.deleteAfterCloseMs != null) throw new GateRefusal(403, "universe permission required for deletion retention: setSessionRetention");
      // Explicit null also overrides profile-derived deletion schedules.
      params = { ...params, deleteAfterCloseMs: null };
    }
    if (!privileged && SESSION_TARGET_OPERATIONS.has(method)) {
      await this.requireSession(method, (params as { sessionId?: string }).sessionId);
    }
    if (!privileged && method === "session/list") {
      params = { ...params, visibleTo: this.caller.actorId };
    }
    return { params };
  }

  private async requireSession(method: string, sessionId: string | undefined): Promise<void> {
    if (!sessionId) {
      if (CREATION_OPERATIONS.has(method)) return;
      throw new GateRefusal(404, "session not found");
    }
    let access;
    try {
      access = (await super.call("session/read", { sessionId, runLimit: 1 })).session.access;
    } catch (error) {
      if (error instanceof LightspeedApiError && error.kind === "notFound" && CREATION_OPERATIONS.has(method)) return;
      throw error;
    }
    const creator = access.createdBy?.kind === "actor" && access.createdBy.id === this.caller.actorId;
    const visible = CREATOR_OPERATIONS.has(method) ? creator : creator || access.visibility === "universe";
    if (!visible) throw new GateRefusal(404, "session not found");
    if (method === "session/delete" && !canDeleteSession(this.caller.actions, access.visibility === "universe")) {
      throw new GateRefusal(403, "deleting an unshared session requires accessPrivateSessions");
    }
    if (method === "session/close" && !canCloseSession(this.caller.actions, creator, access.visibility === "universe")) {
      throw new GateRefusal(403, "closing a shared session requires closeSharedSessions");
    }
  }
}

/** Without configureSession, a session starts from a saved profile or the
 * universe defaults, without overrides, and unshared. */
function requirePreparedSession(params: OperationParams<"session/start">): void {
  if (params.integration !== undefined) {
    throw new GateRefusal(403, "universe permission required to create integrated sessions: configureSession");
  }
  const profile = params.profile;
  const defaultProfile = profile == null || (profile.kind === "inline"
    && profile.profile != null && Object.keys(profile.profile).length === 0
    && Object.keys(profile).every((key) => key === "kind" || key === "profile"));
  const namedProfile = profile?.kind === "named"
    && Object.keys(profile).every((key) => key === "kind" || key === "profileId");
  const allowedFields = new Set(["sessionId", "displayName", "profile", "access"]);
  const overrides = Object.entries(params).some(([key, value]) =>
    !allowedFields.has(key) && value !== undefined,
  );
  if ((!defaultProfile && !namedProfile) || overrides || params.access?.visibility === "universe") {
    throw new GateRefusal(403, "without configureSession, a session may only use the universe defaults or an existing profile, without overrides");
  }
}
