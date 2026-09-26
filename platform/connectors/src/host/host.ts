import type { ChannelProvider, DeploymentChannelAccountView } from "@lightspeed-ai/agent-client";
import type { CoreClient } from "../core/client.js";
import type { AccountSelector } from "../core/identity.js";
import type { AccountRunnerLike } from "./account-runner.js";
import { planReconciliation, selectAccounts } from "./discovery.js";
import {
  hostHealth,
  startHostHealthServer,
  type ConnectorHostHealth,
  type HostHealthServer,
} from "./health.js";
import { renderConnectorMetrics } from "./metrics.js";

type Log = Pick<Console, "log" | "warn" | "error">;

export interface ConnectorHostOptions {
  providers: readonly ChannelProvider[];
  accounts: readonly AccountSelector[] | null;
  discoveryIntervalMs: number;
  /** Health/metrics listener; null runs without one (tests). */
  health: { host: string; port: number } | null;
}

export interface ConnectorHostDeps {
  core: CoreClient;
  createRunner: (account: DeploymentChannelAccountView) => AccountRunnerLike;
  log?: Log;
  now?: () => number;
}

/**
 * One process serving many accounts across many universes. Every
 * `discoveryIntervalMs` the host asks the core for the enabled accounts
 * (`deployment/channels/accounts/list`), narrows them to its providers and
 * account list, and reconciles the running set: new accounts start, missing
 * or disabled ones stop, a changed revision or a dead runner restarts.
 */
export class ConnectorHost {
  private readonly runners = new Map<string, AccountRunnerLike>();
  private readonly log: Log;
  private readonly now: () => number;
  private readonly discovery: ConnectorHostHealth["discovery"] = { passes: 0 };
  private startedAtMs = 0;
  private timer: NodeJS.Timeout | undefined;
  private inflight: Promise<void> | undefined;
  private server: HostHealthServer | undefined;
  private stopping = false;
  private stopped = false;

  constructor(
    private readonly options: ConnectorHostOptions,
    private readonly deps: ConnectorHostDeps,
  ) {
    this.log = deps.log ?? console;
    this.now = deps.now ?? Date.now;
  }

  /** Listening port of the health server, once started. */
  get healthPort(): number | undefined {
    return this.server?.port;
  }

  /** Bind health, run the first discovery pass, and keep discovering until `stop()`. */
  async start(): Promise<void> {
    this.startedAtMs = this.now();
    if (this.options.health !== null) {
      this.server = await startHostHealthServer({
        host: this.options.health.host,
        port: this.options.health.port,
        snapshot: () => this.snapshot(),
        metrics: () => this.metrics(),
      });
    }
    await this.discover();
    this.schedule();
  }

  /** One discovery and reconciliation pass; concurrent calls share the pass. */
  discover(): Promise<void> {
    this.inflight ??= this.pass().finally(() => {
      this.inflight = undefined;
    });
    return this.inflight;
  }

  async stop(): Promise<void> {
    if (this.stopping) return;
    this.stopping = true;
    if (this.timer !== undefined) {
      clearTimeout(this.timer);
      this.timer = undefined;
    }
    await this.inflight;
    const runners = [...this.runners.values()];
    this.runners.clear();
    await Promise.all(
      runners.map((runner) =>
        runner.stop().catch((error: unknown) => {
          this.log.error(`connectors: ${runner.key} stop failed`, error);
        }),
      ),
    );
    await this.server?.close();
    this.server = undefined;
    this.stopped = true;
  }

  /** Keys of the accounts currently served. */
  served(): string[] {
    return [...this.runners.keys()];
  }

  snapshot(): ConnectorHostHealth {
    return hostHealth({
      startedAtMs: this.startedAtMs,
      stopping: this.stopping,
      stopped: this.stopped,
      discovery: this.discovery,
      accounts: [...this.runners.values()].map((runner) => runner.health()),
    });
  }

  metrics(): string {
    return renderConnectorMetrics(
      [...this.runners.values()].map((runner) => ({
        health: runner.health(),
        inbound: runner.metrics,
      })),
      this.snapshot(),
    );
  }

  private schedule(): void {
    if (this.stopping) return;
    this.timer = setTimeout(() => {
      this.timer = undefined;
      void this.discover().finally(() => this.schedule());
    }, this.options.discoveryIntervalMs);
  }

  private async pass(): Promise<void> {
    if (this.stopping) return;
    this.discovery.passes += 1;
    let listed: DeploymentChannelAccountView[];
    try {
      const response = await this.deps.core
        .deployment()
        .call("deployment/channels/accounts/list", { includeDisabled: false });
      listed = response.result.accounts ?? [];
    } catch (error) {
      this.discovery.lastError = errorMessage(error);
      this.discovery.lastErrorAtMs = this.now();
      this.log.error("connectors: account discovery failed", error);
      return;
    }
    const desired = selectAccounts(listed, {
      providers: this.options.providers,
      accounts: this.options.accounts,
    });
    const plan = planReconciliation(
      [...this.runners.values()].map((runner) => ({
        key: runner.key,
        revision: runner.account.revision,
        failed: runner.failed(),
      })),
      desired,
    );
    for (const key of plan.stop) {
      await this.retire(key, "account disappeared or was disabled");
    }
    for (const account of plan.restart) {
      await this.retire(keyOf(account), "account changed or its runner failed");
      this.launch(account);
    }
    for (const account of plan.start) {
      this.launch(account);
    }
    this.discovery.lastSuccessAtMs = this.now();
  }

  private launch(account: DeploymentChannelAccountView): void {
    if (this.stopping) return;
    const runner = this.deps.createRunner(account);
    this.runners.set(runner.key, runner);
    runner.start();
    this.log.log(
      `connectors: serving ${account.provider} account ${runner.key} (${account.displayName}, revision ${account.revision})`,
    );
  }

  private async retire(key: string, reason: string): Promise<void> {
    const runner = this.runners.get(key);
    if (runner === undefined) return;
    this.runners.delete(key);
    this.log.log(`connectors: stopping ${key}: ${reason}`);
    try {
      await runner.stop();
    } catch (error) {
      this.log.error(`connectors: ${key} stop failed`, error);
    }
  }
}

function keyOf(account: DeploymentChannelAccountView): string {
  return `${account.universeId.toLowerCase()}/${account.accountId}`;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
