import { describe, expect, it } from "vitest";
import type { MessagingBridgeConfig } from "../src/config.js";
import { buildGatewayConnections, gatewayHeaders } from "../src/gateway_auth.js";

function config(overrides: Partial<MessagingBridgeConfig["lightspeed"]>, bindings: MessagingBridgeConfig["bindings"]): MessagingBridgeConfig {
  return {
    lightspeed: {
      endpoint: "http://127.0.0.1:18080/rpc",
      waitMs: 1000,
      eventLimit: 16,
      sessionPrefix: "test",
      apiKey: null,
      universe: null,
      ...overrides,
    },
    runtime: {
      debounceMs: 1,
      turnMaxBatch: 1,
      turnMaxWaitMs: 1,
      roomRetentionHigh: 0,
      roomRetentionLow: 0,
    },
    store: { path: "unused" },
    bindings,
  };
}

describe("buildGatewayConnections", () => {
  it("shares one connection (and thus one outbox tailer) per distinct credential", () => {
    // Two rules with identical keys MUST share a connection: two outbox
    // tailers on the same universe would double-deliver every message.
    const connections = buildGatewayConnections(
      config({}, [
        { id: "a", match: { channel: "telegram" }, auth: { apiKey: "lsk_shared" } },
        { id: "b", match: { channel: "telegram" }, auth: { apiKey: "lsk_shared" } },
        { id: "c", match: { channel: "telegram" }, auth: { universe: "6f3a1a52-58c1-4f0e-9c2d-1a2b3c4d5e6f" } },
      ]),
    );
    expect(connections.byBindingId.get("a")).toBe(connections.byBindingId.get("b"));
    expect(connections.byBindingId.get("c")).not.toBe(connections.byBindingId.get("a"));
    // default + shared key + universe = 3 distinct connections.
    expect(connections.distinct).toHaveLength(3);
  });

  it("reuses the default connection for a rule whose credentials match it", () => {
    const connections = buildGatewayConnections(
      config({ apiKey: "lsk_default" }, [
        { id: "same", match: { channel: "telegram" }, auth: { apiKey: "lsk_default" } },
      ]),
    );
    expect(connections.byBindingId.get("same")).toBe(connections.default);
    expect(connections.distinct).toHaveLength(1);
  });

  it("builds bearer and universe headers", () => {
    expect(gatewayHeaders({ apiKey: "lsk_x" })).toEqual({ authorization: "Bearer lsk_x" });
    expect(gatewayHeaders({ universe: "u-1" })).toEqual({ "x-lightspeed-universe": "u-1" });
    expect(gatewayHeaders({})).toEqual({});
  });
});
