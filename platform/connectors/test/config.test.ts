import { describe, expect, it } from "vitest";
import { parseAccountSelectors, parseHostConfig, parseProviders } from "../src/host/config.js";
import { UNIVERSE_A, UNIVERSE_B } from "./fixtures.js";

const key = Buffer.alloc(32, 1).toString("base64");

describe("connector host configuration", () => {
  it("defaults to every provider and every account", () => {
    const config = parseHostConfig({
      LIGHTSPEED_CONNECTOR_API_KEY: "lsk_test_connector",
      LIGHTSPEED_API_URL: "http://127.0.0.1:18080/rpc",
      LIGHTSPEED_CONNECTOR_WHATSAPP_AUTH_DIR: "/var/lib/wa",
      LIGHTSPEED_CONNECTOR_WHATSAPP_MEDIA_LOCATOR_KEY: key,
    });
    expect(config).toMatchObject({
      apiUrl: "http://127.0.0.1:18080/rpc",
      providers: ["telegram", "whatsapp"],
      accounts: null,
      discoveryIntervalMs: 30_000,
      temporal: { address: "localhost:7233", namespace: "default" },
      ingressMaxPerMinute: 120,
      health: { host: "0.0.0.0", port: 8_090 },
      metrics: { host: "0.0.0.0", port: 9_090 },
    });
    expect(config.whatsapp?.authDir).toBe("/var/lib/wa");
    expect(config.whatsapp?.mediaLocatorKey.byteLength).toBe(32);
  });

  it("needs the WhatsApp session directory and locator key only when WhatsApp is served", () => {
    expect(() => parseHostConfig({ LIGHTSPEED_API_URL: "http://core/rpc" })).toThrow(
      "LIGHTSPEED_CONNECTOR_WHATSAPP_AUTH_DIR is required",
    );
    const telegramOnly = parseHostConfig({
      LIGHTSPEED_CONNECTOR_API_KEY: "lsk_test_connector",
      LIGHTSPEED_API_URL: "http://core/rpc",
      LIGHTSPEED_CONNECTOR_PROVIDERS: "telegram",
      LIGHTSPEED_CONNECTOR_ACCOUNTS: `${UNIVERSE_A}/tg-main, ${UNIVERSE_B}/tg-b,${UNIVERSE_A}/tg-main`,
      LIGHTSPEED_CONNECTOR_DISCOVERY_INTERVAL_MS: "5000",
      LIGHTSPEED_CONNECTOR_HEALTH_PORT: "0",
      TEMPORAL_ADDRESS: "temporal:7233",
      TEMPORAL_NAMESPACE: "prod",
    });
    expect(telegramOnly.whatsapp).toBeNull();
    expect(telegramOnly.providers).toEqual(["telegram"]);
    expect(telegramOnly.accounts).toEqual([
      { universeId: UNIVERSE_A, accountId: "tg-main" },
      { universeId: UNIVERSE_B, accountId: "tg-b" },
    ]);
    expect(telegramOnly.discoveryIntervalMs).toBe(5_000);
    expect(telegramOnly.health.port).toBe(0);
    expect(telegramOnly.temporal).toEqual({ address: "temporal:7233", namespace: "prod" });
  });

  it("requires the core endpoint and rejects unknown providers", () => {
    expect(() => parseHostConfig({})).toThrow("LIGHTSPEED_API_URL is required");
    expect(() => parseProviders("signal")).toThrow("invalid LIGHTSPEED_CONNECTOR_PROVIDERS entry");
    expect(parseProviders(" whatsapp , telegram, whatsapp")).toEqual(["whatsapp", "telegram"]);
    expect(parseAccountSelectors("  ")).toBeNull();
    expect(() => parseAccountSelectors("tg-main")).toThrow(/expected <universeId>\/<accountId>/);
  });
});
