import { LightspeedClient } from "@lightspeed-ai/agent-client";

export const UNIVERSE_HEADER = "x-lightspeed-universe";

export interface CoreClientOptions {
  /** Core JSON-RPC endpoint (`LIGHTSPEED_API_URL`). */
  endpoint: string;
  apiKey: string;
  fetch?: typeof fetch;
}

/**
 * The host's view of the core: one endpoint, one service key, and
 * per-call universe scoping. Discovery is deployment-scoped (`deployment/*`
 * never carries a universe header); everything an account does is stamped
 * with that account's universe.
 */
export class CoreClient {
  private readonly endpoint: string;
  private readonly apiKey: string;
  private readonly fetchImpl: typeof fetch | undefined;
  private readonly universes = new Map<string, LightspeedClient>();
  private deploymentClient: LightspeedClient | undefined;

  constructor(options: CoreClientOptions) {
    if (options.endpoint.length === 0) {
      throw new TypeError("core endpoint must not be empty");
    }
    this.endpoint = options.endpoint;
    if (!/^lsk_[A-Za-z0-9_-]+$/.test(options.apiKey)) throw new TypeError("a Lightspeed service API key is required");
    this.apiKey = options.apiKey;
    this.fetchImpl = options.fetch;
  }

  /** Deployment-scoped `deployment/*` calls: the gateway rejects a universe header on them. */
  deployment(): LightspeedClient {
    this.deploymentClient ??= this.create({});
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

      });
      this.universes.set(universeId, client);
    }
    return client;
  }

  private create(headers: Record<string, string>): LightspeedClient {
    return new LightspeedClient({
      endpoint: this.endpoint,
      headers: { ...headers, authorization: `Bearer ${this.apiKey}` },
      ...(this.fetchImpl === undefined ? {} : { fetch: this.fetchImpl }),
    });
  }
}
