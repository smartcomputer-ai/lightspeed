import { describe, expect, it, vi } from "vitest";
import {
  ConnectorHealthTracker,
  ConnectorLifecycle,
  connectorHealthResponse,
  parseHealthPort,
} from "../src/runtime/connector-lifecycle.js";
import { ConnectorMetrics } from "../src/runtime/connector-metrics.js";

describe("connector lifecycle", () => {
  it("derives structured readiness and reconnect state", () => {
    let now = 1;
    const health = new ConnectorHealthTracker("whatsapp", "primary", () => now++);
    expect(health.health()).toMatchObject({ state: "starting", changedAtMs: 1 });
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
  });

  it("runs stop request and asynchronous cleanup exactly once", async () => {
    const health = new ConnectorHealthTracker("telegram", "primary");
    const requested = vi.fn();
    const cleanup = vi.fn(async () => undefined);
    const lifecycle = new ConnectorLifecycle(health, requested);
    lifecycle.requestStop("SIGTERM");
    lifecycle.requestStop("SIGINT");
    await Promise.all([lifecycle.finish(cleanup), lifecycle.finish(cleanup)]);
    expect(requested).toHaveBeenCalledOnce();
    expect(cleanup).toHaveBeenCalledOnce();
    expect(health.health().state).toBe("stopped");
  });

  it("serves liveness and gates readiness", () => {
    const health = new ConnectorHealthTracker("telegram", "primary");
    expect(connectorHealthResponse(health.health(), "/healthz").status).toBe(200);
    expect(connectorHealthResponse(health.health(), "/readyz").status).toBe(503);
    health.markActivityWorkerReady();
    health.markIngressConnected();
    expect(connectorHealthResponse(health.health(), "/readyz")).toMatchObject({
      status: 200,
      body: { state: "ready" },
    });
    expect(connectorHealthResponse(health.health(), "/unknown")).toEqual({
      status: 404,
      body: { error: "not found" },
    });
  });

  it("validates configured health ports", () => {
    expect(parseHealthPort(undefined, 8091)).toBe(8091);
    expect(parseHealthPort("0", 8091)).toBe(0);
    expect(() => parseHealthPort("nope", 8091)).toThrow("health port");
  });

  it("renders connector availability and admission counters", () => {
    const health = new ConnectorHealthTracker("telegram", 'primary"bot');
    const metrics = new ConnectorMetrics();
    health.markActivityWorkerReady();
    health.markIngressConnected();
    metrics.recordInbound("admitted");
    metrics.recordInbound("admitted");
    metrics.recordInbound("rate_limited");

    const rendered = metrics.render(health.health());
    expect(rendered).toContain(
      'channels_connector_ready{provider="telegram",account_id="primary\\"bot"} 1',
    );
    expect(rendered).toContain('outcome="admitted"} 2');
    expect(rendered).toContain('outcome="rate_limited"} 1');
  });
});
