import { describe, expect, it, vi } from "vitest";
import { PAIRING_REQUIRED_REPLY } from "../src/ingress/admit.js";
import { announceWhatsAppGroupPairing } from "../src/providers/whatsapp/pairing.js";

describe("WhatsApp group pairing announcement", () => {
  it("announces an unpaired matching group once per connector lifetime", async () => {
    const pairingRequired = vi.fn(async () => true);
    const send = vi.fn(async () => undefined);
    const config = {
      accountId: "primary",
      controlPlane: { pairingRequired },
      announcedChats: new Set<string>(),
      send,
    };
    await expect(announceWhatsAppGroupPairing(config, "family@g.us")).resolves.toBe(true);
    await expect(announceWhatsAppGroupPairing(config, "family@g.us")).resolves.toBe(false);
    expect(pairingRequired).toHaveBeenCalledWith(
      { provider: "whatsapp", accountId: "primary", chatId: "family@g.us" },
      "group",
    );
    expect(send).toHaveBeenCalledOnce();
    expect(send).toHaveBeenCalledWith("family@g.us", PAIRING_REQUIRED_REPLY);
  });

  it("ignores direct chats and groups without a pairing gate", async () => {
    const pairingRequired = vi.fn(async () => false);
    const send = vi.fn(async () => undefined);
    const config = {
      accountId: "primary",
      controlPlane: { pairingRequired },
      announcedChats: new Set<string>(),
      send,
    };
    await expect(
      announceWhatsAppGroupPairing(config, "41791111111@s.whatsapp.net"),
    ).resolves.toBe(false);
    await expect(announceWhatsAppGroupPairing(config, "open@g.us")).resolves.toBe(false);
    expect(send).not.toHaveBeenCalled();
  });
});
