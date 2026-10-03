import { describe, expect, it } from "vitest";
import {
  WORKFLOW_CONTRACT_VECTORS,
  connectorTaskQueue,
  sessionWorkflowId,
} from "@lightspeed-ai/client/workflow";
import { accountKey, parseAccountSelector } from "../src/core/identity.js";
import { UNIVERSE_A, UNIVERSE_B } from "./fixtures.js";

describe("connector identities", () => {
  it("derives the account task queue exactly as the Rust core does", () => {
    const vector = WORKFLOW_CONTRACT_VECTORS.channels;
    expect(
      connectorTaskQueue(vector.inputs.universeId, vector.inputs.provider, vector.inputs.accountId),
    ).toBe(vector.connectorTaskQueue);
    expect(vector.connectorTaskQueue).toMatch(/^lightspeed-connector-telegram-[0-9a-f]{24}$/);
  });

  it("changes the queue across universes, providers, and accounts", () => {
    const base = connectorTaskQueue(UNIVERSE_A, "telegram", "primary");
    expect(connectorTaskQueue(UNIVERSE_A, "telegram", "primary")).toBe(base);
    expect(connectorTaskQueue(UNIVERSE_B, "telegram", "primary")).not.toBe(base);
    expect(connectorTaskQueue(UNIVERSE_A, "whatsapp", "primary")).not.toBe(base);
    expect(connectorTaskQueue(UNIVERSE_A, "telegram", "secondary")).not.toBe(base);
    expect(base).not.toContain("primary");
  });

  it("keys served accounts by universe and account id", () => {
    expect(accountKey(UNIVERSE_A.toUpperCase(), "primary")).toBe(`${UNIVERSE_A}/primary`);
    expect(parseAccountSelector(`${UNIVERSE_A}/tg-main`)).toEqual({
      universeId: UNIVERSE_A,
      accountId: "tg-main",
    });
    expect(() => parseAccountSelector("tg-main")).toThrow(/expected <universeId>\/<accountId>/);
    expect(() => parseAccountSelector(`${UNIVERSE_A}/`)).toThrow(/expected/);
    expect(() => parseAccountSelector("nope/tg-main")).toThrow(/UUID/);
  });

  it("composes the Lightspeed holder workflow id for a bot's routed session", () => {
    expect(sessionWorkflowId(UNIVERSE_A, "bot:v1:concierge:k-tg-0123abcd")).toBe(
      `${UNIVERSE_A}/bot:v1:concierge:k-tg-0123abcd`,
    );
  });
});
