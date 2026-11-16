import { LightspeedClient } from "@lightspeed-ai/sdk";


export interface CoreClientOptions {
  /** Core HTTP API endpoint (`LIGHTSPEED_API_URL`). */
  endpoint: string;
  apiToken: string;
  fetch?: typeof fetch;
}

/**
 * The host's view of the core: one endpoint, one runtime token, and
 * per-call universe scoping. Discovery addresses the deployment; account
 * operations carry their universe in the resource URL.
 */
export class CoreClient {
  private readonly endpoint: string;
  private readonly apiToken: string;
  private readonly fetchImpl: typeof fetch | undefined;
  private readonly universes = new Map<string, LightspeedClient>();
  private deploymentClient: LightspeedClient | undefined;

  constructor(options: CoreClientOptions) {
    if (options.endpoint.length === 0) {
      throw new TypeError("core endpoint must not be empty");
    }
    this.endpoint = options.endpoint;
    if (!/^[\x21-\x7e]{32,}$/.test(options.apiToken)) throw new TypeError("a Lightspeed runtime API token is required");
    this.apiToken = options.apiToken;
    this.fetchImpl = options.fetch;
  }

  /** Deployment-scoped discovery uses the same configured runtime token. */
  deployment(): LightspeedClient {
    this.deploymentClient ??= this.create();
    return this.deploymentClient;
  }

  /** Universe-scoped calls for one account's universe. */
  forUniverse(universeId: string): LightspeedClient {
    if (universeId.length === 0) {
      throw new TypeError("universeId must not be empty");
    }
    let client = this.universes.get(universeId);
    if (client === undefined) {
      client = this.create(universeId);
      this.universes.set(universeId, client);
    }
    return client;
  }

  private create(universe?: string): LightspeedClient {
    return new LightspeedClient({
      endpoint: this.endpoint,
      headers: { authorization: `Bearer ${this.apiToken}` },
      ...(universe ? { universe } : {}),
      ...(this.fetchImpl === undefined ? {} : { fetch: this.fetchImpl }),
    });
  }
}
