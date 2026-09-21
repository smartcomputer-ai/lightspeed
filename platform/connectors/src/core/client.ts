import { LightspeedClient } from "@lightspeed-ai/agent-client";

/** The service principal the host stamps on every core call (trusted-header mode). */
export const CONNECTOR_PRINCIPAL = "service_account:lightspeed-connectors";
export const UNIVERSE_HEADER = "x-lightspeed-universe";
export const PRINCIPAL_HEADER = "x-lightspeed-principal";

export interface CoreClientOptions {
  /** Core JSON-RPC endpoint (`LIGHTSPEED_API_URL`). */
  endpoint: string;
  principal?: string;
  fetch?: typeof fetch;
}

/**
 * The host's view of the core: one endpoint, one service principal, and
 * per-call universe scoping. Discovery is deployment-scoped (`deployment/*`
 * never carries a universe header); everything an account does is stamped
 * with that account's universe.
 */
export class CoreClient {
  private readonly endpoint: string;
  private readonly principal: string;
  private readonly fetchImpl: typeof fetch | undefined;
  private readonly universes = new Map<string, LightspeedClient>();
  private deploymentClient: LightspeedClient | undefined;

  constructor(options: CoreClientOptions) {
    if (options.endpoint.length === 0) {
      throw new TypeError("core endpoint must not be empty");
    }
    this.endpoint = options.endpoint;
    this.principal = options.principal ?? CONNECTOR_PRINCIPAL;
    this.fetchImpl = options.fetch;
  }

  /** Deployment-scoped `deployment/*` calls: the gateway rejects a universe header on them. */
  deployment(): LightspeedClient {
    this.deploymentClient ??= this.create({ [PRINCIPAL_HEADER]: this.principal });
    return this.deploymentClient;
  }

  /** Universe-scoped calls for one account's universe. */
  forUniverse(universeId: string): LightspeedClient {
    if (universeId.length === 0) {
      throw new TypeError("universeId must not be empty");
    }
    let client = this.universes.get(universeId);
    if (client === undefined) {
      client = this.create({
        [UNIVERSE_HEADER]: universeId,
        [PRINCIPAL_HEADER]: this.principal,
      });
      this.universes.set(universeId, client);
    }
    return client;
  }

  private create(headers: Record<string, string>): LightspeedClient {
    return new LightspeedClient({
      endpoint: this.endpoint,
      headers,
      ...(this.fetchImpl === undefined ? {} : { fetch: this.fetchImpl }),
    });
  }
}
