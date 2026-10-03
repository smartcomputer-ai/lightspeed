import type { ChannelProvider } from "@lightspeed-ai/sdk";
import type { IngressHealth } from "../providers/connector.js";

export type AccountHealthState =
  | "starting"
  | "ready"
  | "disconnected"
  | "failed"
  | "stopping"
  | "stopped";

/** One account's runner as reported on the host's health endpoints. */
export interface AccountHealth {
  version: 1;
  universeId: string;
  accountId: string;
  provider: ChannelProvider;
  state: AccountHealthState;
  ingressConnected: boolean;
  activityWorkerReady: boolean;
  reconnectAttempts: number;
  detail?: string;
  lastError?: string;
  lastErrorAtMs?: number;
  changedAtMs: number;
}

export interface AccountIdentity {
  universeId: string;
  accountId: string;
  provider: ChannelProvider;
}

export class ConnectorHealthTracker implements IngressHealth {
  private workerReady = false;
  private ingressConnected = false;
  private reconnectAttempts = 0;
  private failed = false;
  private stopping = false;
  private stopped = false;
  private lastError: string | undefined;
  private lastErrorAtMs: number | undefined;
  private current: AccountHealth;

  constructor(
    identity: AccountIdentity,
    private readonly now: () => number = Date.now,
  ) {
    this.current = {
      version: 1,
      universeId: identity.universeId,
      accountId: identity.accountId,
      provider: identity.provider,
      state: "starting",
      ingressConnected: false,
      activityWorkerReady: false,
      reconnectAttempts: 0,
      changedAtMs: now(),
    };
  }

  markActivityWorkerReady(): void {
    this.workerReady = true;
    this.refresh(undefined);
  }

  markIngressConnected(): void {
    this.ingressConnected = true;
    this.reconnectAttempts = 0;
    this.refresh(undefined);
  }

  markIngressDisconnected(detail: string): void {
    this.ingressConnected = false;
    this.refresh(detail, true);
  }

  markReconnectScheduled(detail: string): void {
    this.ingressConnected = false;
    this.reconnectAttempts += 1;
    this.refresh(detail, true);
  }

  /** The runner's run loop died; the host restarts it on a later discovery pass. */
  markFailed(detail: string): void {
    this.failed = true;
    this.ingressConnected = false;
    this.workerReady = false;
    this.refresh(detail, true);
  }

  markStopping(detail: string): void {
    this.stopping = true;
    this.refresh(detail);
  }

  markStopped(detail?: string): void {
    this.stopped = true;
    this.stopping = false;
    this.ingressConnected = false;
    this.workerReady = false;
    this.refresh(detail, detail !== undefined);
  }

  health(): AccountHealth {
    return { ...this.current };
  }

  private refresh(detail: string | undefined, recordError = false): void {
    const changedAtMs = this.now();
    if (recordError && detail !== undefined) {
      this.lastError = detail;
      this.lastErrorAtMs = changedAtMs;
    }
    const state: AccountHealthState = this.stopped
      ? "stopped"
      : this.stopping
        ? "stopping"
        : this.failed
          ? "failed"
          : this.ingressConnected && this.workerReady
            ? "ready"
            : this.workerReady
              ? "disconnected"
              : "starting";
    this.current = {
      version: 1,
      universeId: this.current.universeId,
      accountId: this.current.accountId,
      provider: this.current.provider,
      state,
      ingressConnected: this.ingressConnected,
      activityWorkerReady: this.workerReady,
      reconnectAttempts: this.reconnectAttempts,
      ...(detail === undefined ? {} : { detail }),
      ...(this.lastError === undefined ? {} : { lastError: this.lastError }),
      ...(this.lastErrorAtMs === undefined ? {} : { lastErrorAtMs: this.lastErrorAtMs }),
      changedAtMs,
    };
  }
}
