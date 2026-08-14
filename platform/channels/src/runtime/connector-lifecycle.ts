import { createServer, type Server } from "node:http";
import type {
  ChannelProvider,
} from "../contracts/channel.js";
import type {
  ConnectorHealthState,
  ConnectorHealthV1,
} from "../contracts/connector.js";
import type { ConnectorMetrics } from "./connector-metrics.js";

export class ConnectorHealthTracker {
  private workerReady = false;
  private ingressConnected = false;
  private reconnectAttempts = 0;
  private stopping = false;
  private stopped = false;
  private lastError: string | undefined;
  private lastErrorAtMs: number | undefined;
  private current: ConnectorHealthV1;

  constructor(
    provider: ChannelProvider,
    accountId: string,
    private readonly now: () => number = Date.now,
  ) {
    this.current = {
      version: 1,
      provider,
      accountId,
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

  markStopping(detail: string): void {
    this.stopping = true;
    this.refresh(detail, detail !== undefined);
  }

  markStopped(detail?: string): void {
    this.stopped = true;
    this.stopping = false;
    this.ingressConnected = false;
    this.workerReady = false;
    this.refresh(detail);
  }

  health(): ConnectorHealthV1 {
    return { ...this.current };
  }

  private refresh(detail: string | undefined, recordError = false): void {
    const changedAtMs = this.now();
    if (recordError && detail !== undefined) {
      this.lastError = detail;
      this.lastErrorAtMs = changedAtMs;
    }
    const state: ConnectorHealthState = this.stopped
      ? "stopped"
      : this.stopping
        ? "stopping"
        : this.ingressConnected && this.workerReady
          ? "ready"
          : this.workerReady
            ? "disconnected"
            : "starting";
    this.current = {
      version: 1,
      provider: this.current.provider,
      accountId: this.current.accountId,
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

export class ConnectorLifecycle {
  private stopRequested = false;
  private finishPromise: Promise<void> | undefined;

  constructor(
    private readonly health: ConnectorHealthTracker,
    private readonly onStopRequested: () => void,
  ) {}

  requestStop(detail: string): void {
    if (this.stopRequested) {
      return;
    }
    this.stopRequested = true;
    this.health.markStopping(detail);
    this.onStopRequested();
  }

  finish(cleanup: () => Promise<void>): Promise<void> {
    this.requestStop("runtime exited");
    this.finishPromise ??= cleanup().then(
      () => this.health.markStopped(),
      (error: unknown) => {
        this.health.markStopped(errorMessage(error));
        throw error;
      },
    );
    return this.finishPromise;
  }
}

export interface ConnectorHealthServer {
  port: number;
  close(): Promise<void>;
}

export async function startConnectorHealthServer(
  health: ConnectorHealthTracker,
  options: { host: string; port: number; metrics?: ConnectorMetrics },
): Promise<ConnectorHealthServer> {
  const server = createServer((request, response) => {
    if (request.url === "/metrics" && options.metrics !== undefined) {
      response.writeHead(200, {
        "content-type": "text/plain; version=0.0.4; charset=utf-8",
      });
      response.end(options.metrics.render(health.health()));
      return;
    }
    const result = connectorHealthResponse(health.health(), request.url ?? "");
    const status = result.status;
    response.writeHead(status, { "content-type": "application/json; charset=utf-8" });
    response.end(JSON.stringify(result.body));
  });
  await listen(server, options);
  const address = server.address();
  if (address === null || typeof address === "string") {
    await closeServer(server);
    throw new Error("connector health server did not bind a TCP address");
  }
  return {
    port: address.port,
    close: () => closeServer(server),
  };
}

export function connectorHealthResponse(
  snapshot: ConnectorHealthV1,
  path: string,
): { status: number; body: ConnectorHealthV1 | { error: "not found" } } {
  if (path !== "/healthz" && path !== "/readyz") {
    return { status: 404, body: { error: "not found" } };
  }
  return {
    status: path === "/readyz" && snapshot.state !== "ready" ? 503 : 200,
    body: snapshot,
  };
}

export function parseHealthPort(value: string | undefined, fallback: number): number {
  const port = value === undefined ? fallback : Number(value);
  if (!Number.isSafeInteger(port) || port < 0 || port > 65_535) {
    throw new TypeError("connector health port must be an integer between 0 and 65535");
  }
  return port;
}

function listen(server: Server, options: { host: string; port: number }): Promise<void> {
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(options.port, options.host, () => {
      server.off("error", reject);
      resolve();
    });
  });
}

function closeServer(server: Server): Promise<void> {
  return new Promise((resolve, reject) => {
    server.close((error) => (error === undefined ? resolve() : reject(error)));
  });
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
