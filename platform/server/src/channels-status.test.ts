import { describe, expect, it, vi } from "vitest";
import { readChannelsStatus } from "./channels-status.js";

describe("Channels platform status", () => {
  it("aggregates connector health without failing the entire status view", async () => {
    const request = vi.fn(async (input: string | URL | Request) => {
      if (String(input).includes("telegram")) {
        return new Response(JSON.stringify({ state: "ready" }), { status: 200 });
      }
      throw new Error("connection refused");
    });

    await expect(
      readChannelsStatus(
        ["http://channels-telegram:8091", "http://channels-whatsapp:8092"],
        { fetch: request as typeof fetch },
      ),
    ).resolves.toEqual([
      {
        url: "http://channels-telegram:8091",
        reachable: true,
        httpStatus: 200,
        health: { state: "ready" },
      },
      {
        url: "http://channels-whatsapp:8092",
        reachable: false,
        httpStatus: null,
        error: "connection refused",
      },
    ]);
  });
});
