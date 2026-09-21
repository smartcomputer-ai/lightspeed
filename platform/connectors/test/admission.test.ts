import { describe, expect, it, vi } from "vitest";
import type { ChannelInbound, ChannelInboundDecision } from "@lightspeed-ai/agent-client";
import { CoreClient, PRINCIPAL_HEADER, UNIVERSE_HEADER } from "../src/core/client.js";
import {
  PAIRING_CONFIRMED_REPLY,
  PAIRING_REQUIRED_REPLY,
  createInboundGate,
  pairingReplyFor,
} from "../src/host/admission.js";
import { ConnectorMetrics } from "../src/host/metrics.js";
import { FixedWindowRateLimiter } from "../src/host/rate-limit.js";
import { UNIVERSE_A, fakeRpc } from "./fixtures.js";

const inbound: ChannelInbound = {
  messageId: "42",
  chatId: "123",
  senderId: "7",
  senderName: "Lukas",
  timestampMs: 1_700_000_000_000,
  text: "hello",
  isDirect: true,
  mentionedBot: false,
  isReplyToBot: false,
};

describe("inbound admission", () => {
  it("maps every core decision to the reply the host sends itself", () => {
    expect(pairingReplyFor("paired")).toBe(PAIRING_CONFIRMED_REPLY);
    expect(pairingReplyFor("pairing_required")).toBe(PAIRING_REQUIRED_REPLY);
    expect(pairingReplyFor("pairing_pending")).toBeNull();
    expect(pairingReplyFor("bound")).toBeNull();
    expect(pairingReplyFor("unbound")).toBeNull();
  });

  it("calls channels/inbound/admit with the account's universe and answers per decision", async () => {
    const decisions: ChannelInboundDecision[] = ["bound", "paired", "pairing_required", "pairing_pending", "unbound"];
    const rpc = fakeRpc((method) => {
      expect(method).toBe("channels/inbound/admit");
      return { decision: decisions.shift() };
    });
    const core = new CoreClient({ apiKey: "lsk_test_connector", endpoint: "http://core.test/rpc", fetch: rpc.fetch });
    const metrics = new ConnectorMetrics();
    const gate = createInboundGate({
      client: core.forUniverse(UNIVERSE_A),
      accountId: "tg-main",
      rateLimit: new FixedWindowRateLimiter({ limit: 100, windowMs: 60_000, maxKeys: 10 }),
      metrics,
      log: { warn: vi.fn(), error: vi.fn() },
    });

    await expect(gate.admit(inbound)).resolves.toEqual({ outcome: "bound", reply: null });
    await expect(gate.admit(inbound)).resolves.toEqual({ outcome: "paired", reply: PAIRING_CONFIRMED_REPLY });
    await expect(gate.admit(inbound)).resolves.toEqual({
      outcome: "pairing_required",
      reply: PAIRING_REQUIRED_REPLY,
    });
    await expect(gate.admit(inbound)).resolves.toEqual({ outcome: "pairing_pending", reply: null });
    await expect(gate.admit(inbound)).resolves.toEqual({ outcome: "unbound", reply: null });

    expect(rpc.calls).toHaveLength(5);
    expect(rpc.calls[0]?.params).toEqual({ accountId: "tg-main", inbound });
    expect(rpc.calls[0]?.headers.get(UNIVERSE_HEADER)).toBe(UNIVERSE_A);
    expect(rpc.calls[0]?.headers.get("authorization")).toBe("Bearer lsk_test_connector");
    expect(metrics.inboundTotal("bound")).toBe(1);
    expect(metrics.inboundTotal("paired")).toBe(1);
    expect(metrics.inboundTotal("unbound")).toBe(1);
  });

  it("rate limits per chat and sender before contacting the core", async () => {
    const rpc = fakeRpc(() => ({ decision: "bound" }));
    const core = new CoreClient({ apiKey: "lsk_test_connector", endpoint: "http://core.test/rpc", fetch: rpc.fetch });
    const metrics = new ConnectorMetrics();
    const gate = createInboundGate({
      client: core.forUniverse(UNIVERSE_A),
      accountId: "tg-main",
      rateLimit: new FixedWindowRateLimiter({ limit: 1, windowMs: 60_000, maxKeys: 10 }),
      metrics,
      log: { warn: vi.fn(), error: vi.fn() },
    });
    await expect(gate.admit(inbound)).resolves.toMatchObject({ outcome: "bound" });
    await expect(gate.admit(inbound)).resolves.toEqual({ outcome: "rate_limited", reply: null });
    await expect(gate.admit({ ...inbound, senderId: "8" })).resolves.toMatchObject({ outcome: "bound" });
    expect(rpc.calls).toHaveLength(2);
    expect(metrics.inboundTotal("rate_limited")).toBe(1);
  });

  it("counts a core failure and stays silent instead of throwing into the provider loop", async () => {
    const rpc = fakeRpc(() => new Error("universe not found"));
    const core = new CoreClient({ apiKey: "lsk_test_connector", endpoint: "http://core.test/rpc", fetch: rpc.fetch });
    const metrics = new ConnectorMetrics();
    const error = vi.fn();
    const gate = createInboundGate({
      client: core.forUniverse(UNIVERSE_A),
      accountId: "tg-main",
      rateLimit: new FixedWindowRateLimiter({ limit: 10, windowMs: 60_000, maxKeys: 10 }),
      metrics,
      log: { warn: vi.fn(), error },
    });
    await expect(gate.admit(inbound)).resolves.toEqual({ outcome: "failed", reply: null });
    expect(metrics.inboundTotal("failed")).toBe(1);
    expect(error).toHaveBeenCalledOnce();
  });
});

describe("core client", () => {
  it("stamps universe calls and keeps deployment calls universe-free", async () => {
    const rpc = fakeRpc((method) =>
      method === "deployment/channels/accounts/list" ? { accounts: [] } : { decision: "bound" },
    );
    const core = new CoreClient({ apiKey: "lsk_test_connector", endpoint: "http://core.test/rpc", fetch: rpc.fetch });
    await core.deployment().call("deployment/channels/accounts/list", { includeDisabled: false });
    await core.forUniverse(UNIVERSE_A).call("channels/inbound/admit", { accountId: "a", inbound });
    expect(rpc.calls[0]?.headers.has(UNIVERSE_HEADER)).toBe(false);
    expect(rpc.calls[0]?.headers.get("authorization")).toBe("Bearer lsk_test_connector");
    expect(rpc.calls[1]?.headers.get(UNIVERSE_HEADER)).toBe(UNIVERSE_A);
    expect(core.forUniverse(UNIVERSE_A)).toBe(core.forUniverse(UNIVERSE_A));
  });
});
