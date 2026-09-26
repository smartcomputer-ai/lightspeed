import { describe, expect, it, vi } from "vitest";
import { CHANNEL_CONNECTOR_ACTIVITIES, connectorTaskQueue } from "@lightspeed-ai/agent-client/workflow";
import type { NativeConnection } from "@temporalio/worker";
import { CoreClient } from "../src/core/client.js";
import { AccountRunner, whatsAppAuthDir, type WorkerLike } from "../src/host/account-runner.js";
import type { ProviderConnector } from "../src/providers/connector.js";
import { UNIVERSE_A, account, fakeRpc } from "./fixtures.js";

function fakeWorker() {
  let state = "INITIALIZED";
  let finish!: () => void;
  const done = new Promise<void>((resolve) => {
    finish = resolve;
  });
  const worker: WorkerLike & { taskQueue?: string; activities?: Record<string, unknown> } = {
    run: vi.fn(() => {
      state = "RUNNING";
      return done;
    }),
    shutdown: vi.fn(() => {
      state = "STOPPING";
      finish();
    }),
    getState: () => state,
  };
  return worker;
}

function fakeConnector(options: { failAfterStart?: Error } = {}) {
  let finish!: () => void;
  let fail!: (error: Error) => void;
  const done = new Promise<void>((resolve, reject) => {
    finish = resolve;
    fail = reject;
  });
  const connector: ProviderConnector & { started: number; stopped: number; crash: (error: Error) => void } = {
    activities: {
      deliverChannelMessage: vi.fn(async () => ({ version: 1, provider: "telegram" as const, messageIds: ["1"] })),
      prepareChannelMedia: vi.fn(async () => ({ item: { blobRef: "sha256:x", kind: "image" as const, mime: "image/jpeg" } })),
      maintainChannelTyping: vi.fn(async () => undefined),
    },
    started: 0,
    stopped: 0,
    run: () => {
      connector.started += 1;
      if (options.failAfterStart) fail(options.failAfterStart);
      return done;
    },
    stop: async () => {
      connector.stopped += 1;
      finish();
    },
    crash: (error) => fail(error),
  };
  return connector;
}

const deps = () => ({
  core: new CoreClient({ apiKey: "lsk_test_connector", endpoint: "http://core.test/rpc", fetch: fakeRpc(() => ({})).fetch }),
  temporal: { connection: {} as NativeConnection, namespace: "default" },
  whatsapp: null,
  ingressMaxPerMinute: 120,
  log: { log: vi.fn(), warn: vi.fn(), error: vi.fn() },
});

describe("account runner", () => {
  it("registers the three manifest activities on the account's derived queue and stops cleanly", async () => {
    const worker = fakeWorker();
    const connector = fakeConnector();
    const created: Array<{ taskQueue: string; namespace: string; activities: Record<string, unknown> }> = [];
    const runner = new AccountRunner(account({ accountId: "tg-main" }), {
      ...deps(),
      createConnector: () => connector,
      createWorker: async (options) => {
        created.push(options);
        return worker;
      },
    });
    expect(runner.taskQueue).toBe(connectorTaskQueue(UNIVERSE_A, "telegram", "tg-main"));

    runner.start();
    await vi.waitFor(() => expect(connector.started).toBe(1));
    expect(created[0]).toMatchObject({ taskQueue: runner.taskQueue, namespace: "default" });
    expect(Object.keys(created[0]!.activities).sort()).toEqual(
      Object.values(CHANNEL_CONNECTOR_ACTIVITIES).sort(),
    );
    expect(runner.health()).toMatchObject({ state: "disconnected", activityWorkerReady: true });

    await runner.stop();
    expect(worker.shutdown).toHaveBeenCalledOnce();
    expect(connector.stopped).toBe(1);
    expect(runner.failed()).toBe(false);
    expect(runner.health().state).toBe("stopped");
  });

  it("halts the worker and reports failed when provider ingress dies", async () => {
    const worker = fakeWorker();
    const connector = fakeConnector({ failAfterStart: new Error("token revoked for good") });
    const runner = new AccountRunner(account({ accountId: "tg-main" }), {
      ...deps(),
      createConnector: () => connector,
      createWorker: async () => worker,
    });
    runner.start();
    await vi.waitFor(() => expect(runner.failed()).toBe(true));
    expect(worker.shutdown).toHaveBeenCalledOnce();
    expect(runner.health()).toMatchObject({ state: "failed", lastError: "token revoked for good" });
    await runner.stop();
  });

  it("refuses a Telegram account without a credential grant and a WhatsApp account without host settings", async () => {
    const noGrant = new AccountRunner(account({ accountId: "tg-main", credentialGrantId: null }), {
      ...deps(),
      createWorker: async () => fakeWorker(),
    });
    noGrant.start();
    await vi.waitFor(() => expect(noGrant.failed()).toBe(true));
    expect(noGrant.health().lastError).toMatch(/no credential grant/);

    const noWhatsApp = new AccountRunner(
      account({ accountId: "wa-main", provider: "whatsapp", credentialGrantId: null }),
      { ...deps(), createWorker: async () => fakeWorker() },
    );
    noWhatsApp.start();
    await vi.waitFor(() => expect(noWhatsApp.failed()).toBe(true));
    expect(noWhatsApp.health().lastError).toMatch(/LIGHTSPEED_CONNECTOR_WHATSAPP_AUTH_DIR/);
  });

  it("keeps one WhatsApp session directory per universe and account", () => {
    expect(whatsAppAuthDir("/var/lib/wa", { universeId: UNIVERSE_A.toUpperCase(), accountId: "wa-main" })).toBe(
      `/var/lib/wa/${UNIVERSE_A}/wa-main`,
    );
    expect(() => whatsAppAuthDir("/var/lib/wa", { universeId: UNIVERSE_A, accountId: "../etc" })).toThrow(
      /not a directory name/,
    );
  });
});
