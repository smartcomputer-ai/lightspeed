import type { LightspeedClient, DeploymentChannelAccountView } from "@lightspeed-ai/agent-client";
import {
  CHANNEL_CONNECTOR_ACTIVITIES,
  connectorTaskQueue,
} from "@lightspeed-ai/agent-client/workflow";
import { Worker, type NativeConnection } from "@temporalio/worker";
import type { CoreClient } from "../core/client.js";
import { accountKey } from "../core/identity.js";
import { GrantLease } from "../core/leases.js";
import type { InboundGate, ProviderConnector } from "../providers/connector.js";
import { createTelegramConnector } from "../providers/telegram/connector.js";
import { createWhatsAppConnector } from "../providers/whatsapp/connector.js";
import { createInboundGate } from "./admission.js";
import { ConnectorHealthTracker, type AccountHealth } from "./lifecycle.js";
import { ConnectorMetrics } from "./metrics.js";
import { FixedWindowRateLimiter } from "./rate-limit.js";

type Log = Pick<Console, "log" | "warn" | "error">;

/** What the host needs from a runner; `AccountRunner` is the real one. */
export interface AccountRunnerLike {
  readonly key: string;
  readonly account: DeploymentChannelAccountView;
  readonly metrics: ConnectorMetrics;
  /** Start in the background; failures land in `health()` and `failed()`. */
  start(): void;
  stop(): Promise<void>;
  failed(): boolean;
  health(): AccountHealth;
}

/** The subset of a Temporal `Worker` the runner drives. */
export interface WorkerLike {
  run(): Promise<void>;
  shutdown(): void;
  getState(): string;
}

export interface WorkerOptionsLike {
  connection: NativeConnection;
  namespace: string;
  taskQueue: string;
  activities: Record<string, (input: never) => Promise<unknown>>;
}

export interface WhatsAppHostSettings {
  /** Root of the per-account session directories (`<authDir>/<universeId>/<accountId>`). */
  authDir: string;
  mediaLocatorKey: Uint8Array;
}

export interface ProviderConnectorContext {
  /** Universe-scoped core client of the account. */
  universe: LightspeedClient;
  gate: InboundGate;
  health: ConnectorHealthTracker;
  whatsapp: WhatsAppHostSettings | null;
  log: Log;
}

export interface AccountRunnerDeps {
  core: CoreClient;
  temporal: { connection: NativeConnection; namespace: string };
  whatsapp: WhatsAppHostSettings | null;
  ingressMaxPerMinute: number;
  log?: Log;
  /** Test seam: build the provider connector for an account. */
  createConnector?: (
    account: DeploymentChannelAccountView,
    context: ProviderConnectorContext,
  ) => ProviderConnector;
  /** Test seam: create the per-account activity worker. */
  createWorker?: (options: WorkerOptionsLike) => Promise<WorkerLike>;
}

/**
 * One served account: the provider's ingress plus one Temporal activity
 * worker on the account's derived task queue, sharing the host's Temporal
 * connection. Both run until `stop()`; if either dies the runner halts the
 * other, reports `failed`, and the next discovery pass restarts it.
 */
export class AccountRunner implements AccountRunnerLike {
  readonly key: string;
  readonly taskQueue: string;
  readonly metrics = new ConnectorMetrics();
  private readonly tracker: ConnectorHealthTracker;
  private readonly log: Log;
  private stopRequested = false;
  private hasFailed = false;
  private running: Promise<void> | undefined;
  private connector: ProviderConnector | undefined;
  private worker: WorkerLike | undefined;

  constructor(
    readonly account: DeploymentChannelAccountView,
    private readonly deps: AccountRunnerDeps,
  ) {
    this.key = accountKey(account.universeId, account.accountId);
    this.taskQueue = connectorTaskQueue(account.universeId, account.provider, account.accountId);
    this.tracker = new ConnectorHealthTracker({
      universeId: account.universeId,
      accountId: account.accountId,
      provider: account.provider,
    });
    this.log = deps.log ?? console;
  }

  start(): void {
    if (this.running !== undefined) return;
    this.running = this.run().catch((error: unknown) => {
      this.hasFailed = true;
      this.tracker.markFailed(errorMessage(error));
      this.log.error(`connectors: ${this.key} runner failed`, error);
    });
  }

  async stop(): Promise<void> {
    if (!this.stopRequested) {
      this.stopRequested = true;
      this.tracker.markStopping("stop requested");
      this.halt();
    }
    await this.running;
    this.tracker.markStopped();
  }

  failed(): boolean {
    return this.hasFailed;
  }

  health(): AccountHealth {
    return this.tracker.health();
  }

  private async run(): Promise<void> {
    const universe = this.deps.core.forUniverse(this.account.universeId);
    const gate = createInboundGate({
      client: universe,
      accountId: this.account.accountId,
      rateLimit: new FixedWindowRateLimiter({
        limit: this.deps.ingressMaxPerMinute,
        windowMs: 60_000,
        maxKeys: 10_000,
      }),
      metrics: this.metrics,
      log: this.log,
    });
    const connector = (this.deps.createConnector ?? createProviderConnector)(this.account, {
      universe,
      gate,
      health: this.tracker,
      whatsapp: this.deps.whatsapp,
      log: this.log,
    });
    this.connector = connector;
    if (this.stopRequested) return;

    const activities = connector.activities;
    const worker = await (this.deps.createWorker ?? createTemporalWorker)({
      connection: this.deps.temporal.connection,
      namespace: this.deps.temporal.namespace,
      taskQueue: this.taskQueue,
      activities: {
        [CHANNEL_CONNECTOR_ACTIVITIES.deliverChannelMessage]: (command) =>
          activities.deliverChannelMessage(command),
        [CHANNEL_CONNECTOR_ACTIVITIES.prepareChannelMedia]: (input) =>
          activities.prepareChannelMedia(input),
        [CHANNEL_CONNECTOR_ACTIVITIES.maintainChannelTyping]: (input) =>
          activities.maintainChannelTyping(input),
      },
    });
    this.worker = worker;
    const workerRun = worker.run().then(() => {
      if (!this.stopRequested) throw new Error("activity worker stopped unexpectedly");
    });
    if (this.stopRequested) {
      this.halt();
      await workerRun;
      return;
    }
    this.tracker.markActivityWorkerReady();
    this.log.log(
      `connectors: ${this.key} serving ${this.account.provider} activities on ${this.deps.temporal.namespace}/${this.taskQueue}`,
    );
    const ingressRun = connector.run().then(() => {
      if (!this.stopRequested) throw new Error("provider ingress stopped unexpectedly");
    });
    try {
      await Promise.all([workerRun, ingressRun]);
    } catch (error) {
      this.halt();
      await Promise.allSettled([workerRun, ingressRun]);
      throw error;
    }
  }

  private halt(): void {
    if (this.worker !== undefined && this.worker.getState() === "RUNNING") {
      this.worker.shutdown();
    }
    void this.connector?.stop().catch((error: unknown) => {
      this.log.error(`connectors: ${this.key} provider stop failed`, error);
    });
  }
}

export function createProviderConnector(
  account: DeploymentChannelAccountView,
  context: ProviderConnectorContext,
): ProviderConnector {
  switch (account.provider) {
    case "telegram": {
      const grantId = account.credentialGrantId;
      if (grantId === undefined || grantId === null || grantId.length === 0) {
        throw new TypeError(`Telegram account ${account.accountId} has no credential grant`);
      }
      return createTelegramConnector({
        universeId: account.universeId,
        accountId: account.accountId,
        token: new GrantLease(context.universe, grantId),
        gate: context.gate,
        health: context.health,
        core: context.universe,
        log: context.log,
      });
    }
    case "whatsapp": {
      if (context.whatsapp === null) {
        throw new TypeError(
          "WhatsApp accounts need LIGHTSPEED_CONNECTOR_WHATSAPP_AUTH_DIR and LIGHTSPEED_CONNECTOR_WHATSAPP_MEDIA_LOCATOR_KEY",
        );
      }
      return createWhatsAppConnector({
        universeId: account.universeId,
        accountId: account.accountId,
        providerAccountId: account.providerAccountId,
        authDir: whatsAppAuthDir(context.whatsapp.authDir, account),
        printQr: account.settings?.printQr ?? true,
        mediaLocatorKey: context.whatsapp.mediaLocatorKey,
        gate: context.gate,
        health: context.health,
        core: context.universe,
        log: context.log,
      });
    }
    default:
      // Providers are open names in the core; this host only bridges the
      // two it implements, and discovery filters on them already.
      throw new TypeError(
        `unsupported channel provider ${account.provider} (account ${account.accountId})`,
      );
  }
}

/** `<authDir>/<universeId>/<accountId>`: one Baileys session directory per served account. */
export function whatsAppAuthDir(
  root: string,
  account: Pick<DeploymentChannelAccountView, "universeId" | "accountId">,
): string {
  if (account.accountId.includes("/") || account.accountId.includes("\\") || account.accountId.startsWith(".")) {
    throw new TypeError(`WhatsApp account id ${JSON.stringify(account.accountId)} is not a directory name`);
  }
  return `${root}/${account.universeId.toLowerCase()}/${account.accountId}`;
}

async function createTemporalWorker(options: WorkerOptionsLike): Promise<WorkerLike> {
  return Worker.create({
    connection: options.connection,
    namespace: options.namespace,
    taskQueue: options.taskQueue,
    activities: options.activities,
  });
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
