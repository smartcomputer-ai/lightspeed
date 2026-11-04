import { describe, expect, it } from "vitest";
import { hostHealth, hostHealthResponse } from "../src/host/health.js";
import { ConnectorHealthTracker } from "../src/host/lifecycle.js";
import { ConnectorMetrics, renderConnectorMetrics } from "../src/host/metrics.js";
import { parsePort } from "../src/host/config.js";
import { UNIVERSE_A } from "./fixtures.js";

const identity = { universeId: UNIVERSE_A, accountId: "primary", provider: "whatsapp" as const };

describe("account lifecycle", () => {
  it("derives structured readiness and reconnect state", () => {
    let now = 1;
    const health = new ConnectorHealthTracker(identity, () => now++);
    expect(health.health()).toMatchObject({ state: "starting", changedAtMs: 1, universeId: UNIVERSE_A });
    health.markActivityWorkerReady();
    expect(health.health()).toMatchObject({ state: "disconnected" });
    health.markReconnectScheduled("socket closed");
    expect(health.health()).toMatchObject({
      state: "disconnected",
      reconnectAttempts: 1,
      detail: "socket closed",
    });
    health.markIngressConnected();
    expect(health.health()).toMatchObject({
      state: "ready",
      ingressConnected: true,
      activityWorkerReady: true,
      reconnectAttempts: 0,
      lastError: "socket closed",
      lastErrorAtMs: 3,
    });
    health.markFailed("worker died");
    expect(health.health()).toMatchObject({ state: "failed", lastError: "worker died" });
    health.markStopping("stop requested");
    expect(health.health().state).toBe("stopping");
    health.markStopped();
    expect(health.health().state).toBe("stopped");
  });

  it("gates host readiness on discovery and every served account", () => {
    const ready = new ConnectorHealthTracker(identity);
    ready.markActivityWorkerReady();
    ready.markIngressConnected();
    const starting = new ConnectorHealthTracker({ ...identity, accountId: "second" });
    const base = { startedAtMs: 1, stopping: false, stopped: false };

    expect(hostHealth({ ...base, discovery: { passes: 0 }, accounts: [] }).state).toBe("starting");
    expect(
      hostHealth({ ...base, discovery: { passes: 1, lastSuccessAtMs: 2 }, accounts: [] }).state,
    ).toBe("ready");
    expect(
      hostHealth({
        ...base,
        discovery: { passes: 1, lastSuccessAtMs: 2 },
        accounts: [ready.health(), starting.health()],
      }).state,
    ).toBe("degraded");
    expect(
      hostHealth({
        ...base,
        discovery: { passes: 1, lastSuccessAtMs: 2 },
        accounts: [ready.health()],
      }).state,
    ).toBe("ready");
    expect(
      hostHealth({ ...base, stopping: true, discovery: { passes: 1, lastSuccessAtMs: 2 }, accounts: [] })
        .state,
    ).toBe("stopping");
  });

  it("serves liveness and gates readiness", () => {
    const degraded = hostHealth({
      startedAtMs: 1,
      stopping: false,
      stopped: false,
      discovery: { passes: 1, lastSuccessAtMs: 2 },
      accounts: [new ConnectorHealthTracker(identity).health()],
    });
    expect(hostHealthResponse(degraded, "/healthz").status).toBe(200);
    expect(hostHealthResponse(degraded, "/readyz").status).toBe(503);
    const ready = { ...degraded, state: "ready" as const };
    expect(hostHealthResponse(ready, "/readyz")).toMatchObject({ status: 200, body: { state: "ready" } });
    expect(hostHealthResponse(ready, "/unknown")).toEqual({
      status: 404,
      body: { error: "not found" },
    });
  });

  it("validates configured ports", () => {
    expect(parsePort(undefined, 8090, "P")).toBe(8090);
    expect(parsePort("0", 8090, "P")).toBe(0);
    expect(() => parsePort("nope", 8090, "P")).toThrow("P must be an integer");
  });

  it("renders per-account availability and admission counters with universe labels", () => {
    const health = new ConnectorHealthTracker({
      universeId: UNIVERSE_A,
      accountId: 'primary"bot',
      provider: "telegram",
    });
    const metrics = new ConnectorMetrics();
    health.markActivityWorkerReady();
    health.markIngressConnected();
    metrics.recordInbound("bound");
    metrics.recordInbound("bound");
    metrics.recordInbound("rateLimited");

    const rendered = metrics.render(health.health());
    expect(rendered).toContain(
      `channels_connector_ready{universe_id="${UNIVERSE_A}",provider="telegram",account_id="primary\\"bot"} 1`,
    );
    expect(rendered).toContain('outcome="bound"} 2');
    expect(rendered).toContain('outcome="rateLimited"} 1');

    const host = hostHealth({
      startedAtMs: 1,
      stopping: false,
      stopped: false,
      discovery: { passes: 3, lastSuccessAtMs: 5_000 },
      accounts: [health.health()],
    });
    const combined = renderConnectorMetrics([{ health: health.health(), inbound: metrics }], host);
    expect(combined).toContain("connector_host_ready 1");
    expect(combined).toContain("connector_host_accounts 1");
    expect(combined).toContain("connector_host_discovery_passes_total 3");
    expect(combined.match(/# TYPE channels_connector_ready gauge/g)).toHaveLength(1);
  });
});
