import { ChannelDeliveryError } from "../../contracts/delivery.js";
import type { WhatsAppDeliveryApi } from "./delivery.js";
import type { WhatsAppPresenceApi } from "./presence.js";

type WhatsAppConnectorApi = WhatsAppDeliveryApi & WhatsAppPresenceApi;

/** Keeps reconnecting Baileys sockets out of activity payloads and workflow history. */
export class WhatsAppSocketRegistry {
  private currentSocket: WhatsAppConnectorApi | null = null;

  set(socket: WhatsAppConnectorApi): void {
    this.currentSocket = socket;
  }

  clear(socket: WhatsAppConnectorApi): void {
    if (this.currentSocket === socket) {
      this.currentSocket = null;
    }
  }

  get(): WhatsAppConnectorApi {
    if (this.currentSocket === null) {
      throw new ChannelDeliveryError("WhatsApp socket is not connected", true);
    }
    return this.currentSocket;
  }

  connected(): boolean {
    return this.currentSocket !== null;
  }

  deliveryApi(): WhatsAppDeliveryApi {
    return {
      sendMessage: (jid, content, options) => this.get().sendMessage(jid, content, options),
    };
  }

  presenceApi(): WhatsAppPresenceApi {
    return {
      sendPresenceUpdate: (state, jid) => this.get().sendPresenceUpdate(state, jid),
    };
  }
}
