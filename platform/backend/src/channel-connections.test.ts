import { describe, expect, it, vi } from "vitest";
import {
  readTelegramBotIdentity,
  TelegramConnectionError,
  telegramChannelAccountId,
  whatsAppChannelAccountId,
} from "./channel-connections.js";

describe("Telegram channel connections", () => {
  it("validates the token and derives the provider identity", async () => {
    const request = vi.fn(async (_input: string | URL | Request) => new Response(JSON.stringify({
      ok: true,
      result: {
        id: 123456,
        is_bot: true,
        first_name: "Support Bot",
        username: "Northwind_Support_Bot",
      },
    })));

    await expect(readTelegramBotIdentity("123:secret", {
      fetch: request as typeof fetch,
    })).resolves.toEqual({
      id: 123456,
      firstName: "Support Bot",
      username: "Northwind_Support_Bot",
    });
    expect(String(request.mock.calls[0]?.[0])).toContain("123:secret/getMe");
  });

  it("turns a rejected token into a safe user-facing error", async () => {
    const request = vi.fn(async () => new Response(JSON.stringify({
      ok: false,
      description: "Unauthorized",
    }), { status: 401 }));

    const error = await readTelegramBotIdentity("bad-token", {
      fetch: request as typeof fetch,
    }).catch((reason: unknown) => reason);
    expect(error).toBeInstanceOf(TelegramConnectionError);
    expect(error).toMatchObject({ message: "Unauthorized", status: 400 });
    expect(String(error)).not.toContain("bad-token");
  });

  it("derives stable authored account ids", () => {
    expect(telegramChannelAccountId("Northwind_Support_Bot")).toBe(
      "telegram-northwind-support-bot",
    );
    expect(whatsAppChannelAccountId("+41 79 123 45 67")).toBe(
      "whatsapp-41-79-123-45-67",
    );
  });
});
