import { describe, expect, it, vi } from "vitest";
import type { DeploymentChannelAccountView } from "@lightspeed-ai/agent-client";
import { CoreClient } from "../src/core/client.js";
import type { AccountRunnerLike } from "../src/host/account-runner.js";
import { ConnectorHost } from "../src/host/host.js";
import { ConnectorHealthTracker } from "../src/host/lifecycle.js";
import { ConnectorMetrics } from "../src/host/metrics.js";
import { UNIVERSE_A, UNIVERSE_B, account, fakeRpc } from "./fixtures.js";

class FakeRunner implements AccountRunnerLike {
  readonly key: string;
  readonly metrics = new ConnectorMetrics();
  readonly tracker: ConnectorHealthTracker;
  started = 0;
  stopped = 0;
  private hasFailed = false;

  constructor(readonly account: DeploymentChannelAccountView) {
    this.key = `${account.universeId}/${account.accountId}`;
    this.tracker = new ConnectorHealthTracker(account);
  }
  start(): void {
    this.started += 1;
    this.tracker.markActivityWorkerReady();
    this.tracker.markIngressConnected();
  }
  async stop(): Promise<void> {
    this.stopped += 1;
    this.tracker.markStopped();
  }
  fail(): void {
    this.hasFailed = true;
    this.tracker.markFailed("boom");
  }
  failed(): boolean {
    return this.hasFailed;
  }
  health() {
    return this.tracker.health();
  }
}

describe("connector host", () => {
  it("reconciles runners across discovery passes", async () => {
    let listed: DeploymentChannelAccountView[] = [
      account({ accountId: "tg-main" }),
      account({ accountId: "wa-main", provider: "whatsapp", credentialGrantId: null }),
      account({ accountId: "tg-b", universeId: UNIVERSE_B }),
    ];
    const rpc = fakeRpc((method, params) => {
      expect(method).toBe("deployment/channels/accounts/list");
      expect(params).toEqual({ includeDisabled: false });
      return { accounts: listed };
    });
    const runners = new Map<string, FakeRunner[]>();
    const host = new ConnectorHost(
      {
        providers: ["telegram"],
        accounts: null,
        discoveryIntervalMs: 60_000,
        health: null,
      },
      {
        core: new CoreClient({ apiKey: "lsk_test_connector", endpoint: "http://core.test/rpc", fetch: rpc.fetch }),
        createRunner: (candidate) => {
          const runner = new FakeRunner(candidate);
          const list = runners.get(runner.key) ?? [];
          list.push(runner);
          runners.set(runner.key, list);
          return runner;
        },
        log: { log: vi.fn(), warn: vi.fn(), error: vi.fn() },
        now: () => 42,
      },
    );

    await host.start();
    expect(host.served().sort()).toEqual([`${UNIVERSE_B}/tg-b`, `${UNIVERSE_A}/tg-main`].sort());
    expect(rpc.calls[0]?.headers.has("x-lightspeed-universe")).toBe(false);
    expect(host.snapshot()).toMatchObject({ state: "ready", discovery: { passes: 1, lastSuccessAtMs: 42 } });

    // Revision bump restarts; disappearance stops; a new account starts; a failed runner restarts.
    runners.get(`${UNIVERSE_B}/tg-b`)![0]!.fail();
    expect(host.snapshot().state).toBe("degraded");
    listed = [
      account({ accountId: "tg-main", revision: 2 }),
      account({ accountId: "tg-b", universeId: UNIVERSE_B }),
      account({ accountId: "tg-new" }),
    ];
    await host.discover();
    expect(runners.get(`${UNIVERSE_A}/tg-main`)).toHaveLength(2);
    expect(runners.get(`${UNIVERSE_A}/tg-main`)![0]!.stopped).toBe(1);
    expect(runners.get(`${UNIVERSE_B}/tg-b`)).toHaveLength(2);
    expect(runners.get(`${UNIVERSE_A}/tg-new`)).toHaveLength(1);
    expect(host.served().sort()).toEqual(
      [`${UNIVERSE_B}/tg-b`, `${UNIVERSE_A}/tg-main`, `${UNIVERSE_A}/tg-new`].sort(),
    );

    listed = [account({ accountId: "tg-main", revision: 2 })];
    await host.discover();
    expect(host.served()).toEqual([`${UNIVERSE_A}/tg-main`]);
    expect(runners.get(`${UNIVERSE_A}/tg-new`)![0]!.stopped).toBe(1);
    expect(host.metrics()).toContain("connector_host_accounts 1");

    await host.stop();
    expect(runners.get(`${UNIVERSE_A}/tg-main`)![1]!.stopped).toBe(1);
    expect(host.snapshot().state).toBe("stopped");
  });

  it("keeps serving through a failed discovery pass and reports it", async () => {
    let fail = false;
    const rpc = fakeRpc(() => (fail ? new Error("core unavailable") : { accounts: [account({ accountId: "tg-main" })] }));
    const host = new ConnectorHost(
      { providers: ["telegram", "whatsapp"], accounts: null, discoveryIntervalMs: 60_000, health: null },
      {
        core: new CoreClient({ apiKey: "lsk_test_connector", endpoint: "http://core.test/rpc", fetch: rpc.fetch }),
        createRunner: (candidate) => new FakeRunner(candidate),
        log: { log: vi.fn(), warn: vi.fn(), error: vi.fn() },
      },
    );
    await host.start();
    fail = true;
    await host.discover();
    expect(host.served()).toEqual([`${UNIVERSE_A}/tg-main`]);
    expect(host.snapshot()).toMatchObject({
      state: "ready",
      discovery: { passes: 2, lastError: expect.stringContaining("core unavailable") },
    });
    await host.stop();
  });

  it("serves health, readiness, and metrics for every account on one listener", async () => {
    const rpc = fakeRpc(() => ({ accounts: [account({ accountId: "tg-main" })] }));
    const host = new ConnectorHost(
      { providers: ["telegram"], accounts: null, discoveryIntervalMs: 60_000, health: { host: "127.0.0.1", port: 0 } },
      {
        core: new CoreClient({ apiKey: "lsk_test_connector", endpoint: "http://core.test/rpc", fetch: rpc.fetch }),
        createRunner: (candidate) => new FakeRunner(candidate),
        log: { log: vi.fn(), warn: vi.fn(), error: vi.fn() },
      },
    );
    await host.start();
    try {
      const base = `http://127.0.0.1:${host.healthPort}`;
      const ready = await fetch(`${base}/readyz`);
      expect(ready.status).toBe(200);
      await expect(ready.json()).resolves.toMatchObject({
        state: "ready",
        accounts: [{ accountId: "tg-main", state: "ready" }],
      });
      expect((await fetch(`${base}/healthz`)).status).toBe(200);
      const metrics = await (await fetch(`${base}/metrics`)).text();
      expect(metrics).toContain(`channels_connector_ready{universe_id="${UNIVERSE_A}",provider="telegram",account_id="tg-main"} 1`);
      expect((await fetch(`${base}/nope`)).status).toBe(404);
    } finally {
      await host.stop();
    }
  });
});
