import type { ChannelControlPlane } from "../../control-plane/bindings.js";
import { PAIRING_REQUIRED_REPLY } from "../../ingress/admit.js";

export interface WhatsAppPairingAnnouncementConfig {
  accountId: string;
  controlPlane: Pick<ChannelControlPlane, "pairingRequired">;
  announcedChats: Set<string>;
  send(chatId: string, text: string): Promise<void>;
}

/** Best-effort join announcement, deduplicated for the connector lifetime. */
export async function announceWhatsAppGroupPairing(
  config: WhatsAppPairingAnnouncementConfig,
  chatId: string | null | undefined,
): Promise<boolean> {
  if (chatId == null || !chatId.endsWith("@g.us") || config.announcedChats.has(chatId)) {
    return false;
  }
  const required = await config.controlPlane.pairingRequired(
    { provider: "whatsapp", accountId: config.accountId, chatId },
    "group",
  );
  if (!required) return false;
  await config.send(chatId, PAIRING_REQUIRED_REPLY);
  config.announcedChats.add(chatId);
  return true;
}
