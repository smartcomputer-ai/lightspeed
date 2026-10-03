import { mkdir } from "node:fs/promises";
import {
  DisconnectReason,
  fetchLatestBaileysVersion,
  makeWASocket,
  normalizeMessageContent,
  useMultiFileAuthState,
  type WASocket,
} from "baileys";
import qrcode from "qrcode-terminal";
import type { LightspeedClient } from "@lightspeed-ai/client";
import type {
  ConnectorActivities,
  InboundGate,
  IngressHealth,
  ProviderConnector,
} from "../connector.js";
import { createWhatsAppDeliveryActivities, type WhatsAppDeliveryApi } from "./delivery.js";
import { normalizeWhatsAppInbound } from "./ingress.js";
import {
  createWhatsAppMediaActivities,
  describeWhatsAppMedia,
  whatsAppMediaScope,
} from "./media.js";
import { createWhatsAppPresenceActivities } from "./presence.js";
import { WhatsAppSocketRegistry } from "./socket-registry.js";

export interface WhatsAppConnectorConfig {
  universeId: string;
  accountId: string;
  /** The account's WhatsApp identity (phone number / JID), one of its own JIDs. */
  providerAccountId: string;
  /** Directory holding this account's Baileys session state. */
  authDir: string;
  /** Print the pairing QR code on the terminal (`settings.printQr`). */
  printQr: boolean;
  mediaLocatorKey: Uint8Array;
  gate: InboundGate;
  health: IngressHealth;
  /** Universe-scoped core client (media uploads). */
  core: Pick<LightspeedClient, "call">;
  log?: Pick<Console, "log" | "warn" | "error">;
  reconnectDelayMs?: number;
}

/**
 * One WhatsApp account: a Baileys socket for ingress (reconnecting in-process)
 * and the account's activities through the socket registry, so a reconnect
 * never invalidates a running activity worker.
 */
export function createWhatsAppConnector(config: WhatsAppConnectorConfig): ProviderConnector {
  const log = config.log ?? console;
  const reconnectDelayMs = config.reconnectDelayMs ?? 3_000;
  const registry = new WhatsAppSocketRegistry();
  const mediaScope = whatsAppMediaScope(config.universeId, config.accountId);
  let stopped = false;
  let socket: WASocket | undefined;
  let reconnectTimer: NodeJS.Timeout | undefined;
  let finish: (() => void) | undefined;

  const activities: ConnectorActivities = {
    ...createWhatsAppDeliveryActivities({
      accountId: config.accountId,
      api: registry.deliveryApi(),
    }),
    ...createWhatsAppMediaActivities({
      universeId: config.universeId,
      accountId: config.accountId,
      locatorKey: config.mediaLocatorKey,
      core: config.core,
    }),
    ...createWhatsAppPresenceActivities({
      accountId: config.accountId,
      api: registry.presenceApi(),
    }),
  };

  async function run(): Promise<void> {
    await mkdir(config.authDir, { recursive: true });
    const { state, saveCreds } = await useMultiFileAuthState(config.authDir);
    const { version } = await fetchLatestBaileysVersion();
    if (stopped) return;
    const done = new Promise<void>((resolve) => {
      finish = resolve;
    });

    const ownJids = (next: WASocket): Set<string> =>
      new Set(
        [config.providerAccountId, next.user?.id, next.user?.lid, next.user?.phoneNumber].filter(
          (jid): jid is string => typeof jid === "string" && jid.length > 0,
        ),
      );

    const connect = (): void => {
      if (stopped) return;
      reconnectTimer = undefined;
      const next = makeWASocket({
        auth: state,
        markOnlineOnConnect: false,
        printQRInTerminal: false,
        syncFullHistory: false,
        version,
      });
      socket = next;
      const deliverySocket: WhatsAppDeliveryApi & {
        sendPresenceUpdate(state: "composing" | "paused", jid: string): Promise<void>;
      } = {
        sendMessage: (jid, content, options) =>
          next.sendMessage(jid, content as never, options as never),
        sendPresenceUpdate: (presence, jid) => next.sendPresenceUpdate(presence, jid),
      };

      next.ev.on("creds.update", saveCreds);
      next.ev.on("messages.upsert", async (upsert) => {
        if (upsert.type !== "notify") {
          return;
        }
        for (const message of upsert.messages) {
          try {
            const content = normalizeMessageContent(message.message ?? undefined);
            const remoteJid = message.key.remoteJid;
            const messageId = message.key.id;
            if (content === undefined || remoteJid == null || messageId == null) {
              continue;
            }
            const contextInfo =
              content.extendedTextMessage?.contextInfo ??
              content.imageMessage?.contextInfo ??
              content.videoMessage?.contextInfo ??
              content.documentMessage?.contextInfo ??
              content.audioMessage?.contextInfo;
            const text =
              content.conversation ??
              content.extendedTextMessage?.text ??
              content.imageMessage?.caption ??
              content.videoMessage?.caption ??
              content.documentMessage?.caption ??
              "";
            const media = [
              content.imageMessage == null
                ? null
                : describeWhatsAppMedia(mediaScope, config.mediaLocatorKey, {
                    messageId,
                    mediaType: "image",
                    reportedMime: content.imageMessage.mimetype,
                    byteSize: optionalNumber(content.imageMessage.fileLength),
                    mediaKey: content.imageMessage.mediaKey,
                    directPath: content.imageMessage.directPath,
                    url: content.imageMessage.url,
                  }),
              content.documentMessage == null
                ? null
                : describeWhatsAppMedia(mediaScope, config.mediaLocatorKey, {
                    messageId,
                    mediaType: "document",
                    reportedMime: content.documentMessage.mimetype,
                    fileName: content.documentMessage.fileName,
                    byteSize: optionalNumber(content.documentMessage.fileLength),
                    mediaKey: content.documentMessage.mediaKey,
                    directPath: content.documentMessage.directPath,
                    url: content.documentMessage.url,
                  }),
              content.audioMessage == null
                ? null
                : describeWhatsAppMedia(mediaScope, config.mediaLocatorKey, {
                    messageId,
                    mediaType: "audio",
                    reportedMime: content.audioMessage.mimetype,
                    byteSize: optionalNumber(content.audioMessage.fileLength),
                    mediaKey: content.audioMessage.mediaKey,
                    directPath: content.audioMessage.directPath,
                    url: content.audioMessage.url,
                    voiceNote: content.audioMessage.ptt ?? false,
                  }),
            ].filter((entry) => entry !== null);
            const inbound = normalizeWhatsAppInbound(
              { ownJids: ownJids(next) },
              {
                messageId,
                remoteJid,
                ...(message.key.participant == null
                  ? {}
                  : { participantJid: message.key.participant }),
                ...(message.pushName == null ? {} : { pushName: message.pushName }),
                timestampMs: Number(message.messageTimestamp ?? 0) * 1_000,
                text,
                ...(media.length === 0 ? {} : { media }),
                ...(contextInfo?.mentionedJid == null
                  ? {}
                  : { mentionedJids: contextInfo.mentionedJid }),
                ...(contextInfo?.participant == null
                  ? {}
                  : { quotedParticipantJid: contextInfo.participant }),
                ...(message.key.fromMe == null ? {} : { fromMe: message.key.fromMe }),
              },
            );
            if (inbound === null) {
              continue;
            }
            const verdict = await config.gate.admit(inbound);
            if (verdict.reply !== null) {
              await next.sendMessage(remoteJid, { text: verdict.reply }, { quoted: message });
            }
          } catch (error) {
            log.error(`connectors: WhatsApp ${config.accountId} ingress handler failed`, error);
          }
        }
      });
      next.ev.on("connection.update", (update) => {
        if (socket !== next) {
          return;
        }
        if (update.qr !== undefined) {
          config.health.markIngressDisconnected("waiting for QR scan");
          if (config.printQr) {
            log.log(`connectors: scan the WhatsApp QR code for ${config.accountId}`);
            qrcode.generate(update.qr, { small: true });
          }
        }
        if (update.connection === "open") {
          registry.set(deliverySocket);
          config.health.markIngressConnected();
          log.log(`connectors: WhatsApp ${config.accountId} connected`);
        }
        if (update.connection !== "close") {
          return;
        }
        registry.clear(deliverySocket);
        if (stopped) {
          return;
        }
        const statusCode = (update.lastDisconnect?.error as { output?: { statusCode?: number } })
          ?.output?.statusCode;
        if (statusCode === DisconnectReason.loggedOut) {
          config.health.markIngressDisconnected("logged out");
          log.error(
            `connectors: WhatsApp ${config.accountId} logged out; clear its auth directory and pair again`,
          );
          return;
        }
        config.health.markReconnectScheduled(`socket closed (${statusCode ?? "unknown"})`);
        reconnectTimer = setTimeout(connect, reconnectDelayMs);
      });
    };

    connect();
    await done;
  }

  async function stop(): Promise<void> {
    stopped = true;
    if (reconnectTimer !== undefined) {
      clearTimeout(reconnectTimer);
      reconnectTimer = undefined;
    }
    const current = socket;
    socket = undefined;
    current?.end(undefined);
    finish?.();
  }

  return { activities, run, stop };
}

function optionalNumber(value: unknown): number | undefined {
  if (value === null || value === undefined) return undefined;
  const number = Number(value);
  return Number.isSafeInteger(number) && number >= 0 ? number : undefined;
}
