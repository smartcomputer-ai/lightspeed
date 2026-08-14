import {
  DisconnectReason,
  fetchLatestBaileysVersion,
  makeWASocket,
  normalizeMessageContent,
  useMultiFileAuthState,
  type WASocket,
} from "baileys";
import qrcode from "qrcode-terminal";
import { Connection, WorkflowClient } from "@temporalio/client";
import { NativeConnection, Worker } from "@temporalio/worker";
import { createDb } from "@lightspeed/platform-db";
import { createDbChannelControlPlane } from "../control-plane/bindings.js";
import { channelDeliveryTaskQueue } from "../identity/ids.js";
import {
  admitInbound,
  PAIRING_CONFIRMED_REPLY,
  PAIRING_REQUIRED_REPLY,
} from "../ingress/admit.js";
import {
  createWhatsAppDeliveryActivities,
  createWhatsAppMediaActivities,
  createWhatsAppPresenceActivities,
  announceWhatsAppGroupPairing,
  describeWhatsAppMedia,
  parseWhatsAppMediaLocatorKey,
  WhatsAppSocketRegistry,
  type WhatsAppDeliveryApi,
} from "../providers/whatsapp/index.js";
import {
  matchesAnyJid,
  normalizeWhatsAppInbound,
} from "../providers/whatsapp/ingress.js";
import {
  ConnectorHealthTracker,
  ConnectorLifecycle,
  parseHealthPort,
  startConnectorHealthServer,
} from "./connector-lifecycle.js";
import { ConnectorMetrics } from "./connector-metrics.js";
import { FixedWindowRateLimiter, parsePositiveInteger } from "./rate-limit.js";
import { installTemporalMetrics } from "./temporal-metrics.js";

const address = process.env.TEMPORAL_ADDRESS ?? "localhost:7233";
const namespace = process.env.TEMPORAL_NAMESPACE ?? "default";
const accountId = requiredEnvironment("CHANNELS_WHATSAPP_ACCOUNT_ID");
const authDir = requiredEnvironment("CHANNELS_WHATSAPP_AUTH_DIR");
const databaseUrl = requiredEnvironment(
  "LIGHTSPEED_PLATFORM_DATABASE_URL",
  "LSBOT_DATABASE_URL",
);
const lightspeedEndpoint = requiredEnvironment("LIGHTSPEED_ENDPOINT");
const mediaLocatorKey = parseWhatsAppMediaLocatorKey(
  requiredEnvironment("CHANNELS_WHATSAPP_MEDIA_LOCATOR_KEY"),
);
const printQr = process.env.CHANNELS_WHATSAPP_PRINT_QR !== "false";
const taskQueue = channelDeliveryTaskQueue("whatsapp", accountId);
const health = new ConnectorHealthTracker("whatsapp", accountId);
const metrics = new ConnectorMetrics();
const registry = new WhatsAppSocketRegistry();
const ingressRateLimit = new FixedWindowRateLimiter({
  limit: parsePositiveInteger(process.env.CHANNELS_INGRESS_MAX_PER_MINUTE, 120),
  windowMs: 60_000,
  maxKeys: 10_000,
});
const database = createDb(databaseUrl);
const channelControlPlane = createDbChannelControlPlane(database.db);
const clientConnection = await Connection.connect({ address });
const workflowClient = new WorkflowClient({ connection: clientConnection, namespace });
installTemporalMetrics("channels-whatsapp", 9_092);
const { state, saveCreds } = await useMultiFileAuthState(authDir);
const { version } = await fetchLatestBaileysVersion();
let stopped = false;
let reconnectTimer: NodeJS.Timeout | undefined;
let socket: WASocket | undefined;
const pairingAnnouncements = new Set<string>();

function connect(): void {
  if (stopped) {
    return;
  }
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
            : describeWhatsAppMedia(accountId, mediaLocatorKey, {
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
            : describeWhatsAppMedia(accountId, mediaLocatorKey, {
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
            : describeWhatsAppMedia(accountId, mediaLocatorKey, {
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
        const ownJids = new Set(
          [accountId, next.user?.id, next.user?.lid, next.user?.phoneNumber].filter(
            (jid): jid is string => typeof jid === "string" && jid.length > 0,
          ),
        );
        const inbound = normalizeWhatsAppInbound(
          { accountId, ownJids },
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
        if (!ingressRateLimit.allow(`${inbound.route.chatId}\0${inbound.senderId}`)) {
          metrics.recordInbound("rate_limited");
          console.warn(`channels: WhatsApp ingress rate limited chat ${inbound.route.chatId}`);
          continue;
        }
        const admitted = await admitInbound(workflowClient, channelControlPlane, inbound);
        metrics.recordInbound(admitted.status);
        if (admitted.status === "pairing_required" || admitted.status === "paired") {
          await next.sendMessage(
            remoteJid,
            {
              text:
                admitted.status === "paired"
                  ? PAIRING_CONFIRMED_REPLY
                  : PAIRING_REQUIRED_REPLY,
            },
            { quoted: message },
          );
        } else if (admitted.status === "unbound") {
          console.warn(`channels: WhatsApp ignored unbound chat ${remoteJid}`);
        }
      } catch (error) {
        metrics.recordInbound("failed");
        console.error("channels: WhatsApp ingress handler failed", error);
      }
    }
  });
  const announcePairing = async (remoteJid: string | null | undefined) => {
    const announced = await announceWhatsAppGroupPairing(
      {
        accountId,
        controlPlane: channelControlPlane,
        announcedChats: pairingAnnouncements,
        send: async (chatId, text) => {
          await next.sendMessage(chatId, { text });
        },
      },
      remoteJid,
    );
    if (announced) {
      console.log(`channels: announced pairing in WhatsApp group ${remoteJid}`);
    }
  };
  next.ev.on("groups.upsert", async (groups) => {
    for (const group of groups) {
      try {
        await announcePairing(group.id);
      } catch (error) {
        console.warn(`channels: WhatsApp group pairing announcement failed`, error);
      }
    }
  });
  next.ev.on("group-participants.update", async (update) => {
    if (update.action !== "add") return;
    const ownJids = new Set(
      [accountId, next.user?.id, next.user?.lid, next.user?.phoneNumber].filter(
        (jid): jid is string => typeof jid === "string" && jid.length > 0,
      ),
    );
    const selfAdded = update.participants.some((participant) =>
      [participant.id, participant.lid, participant.phoneNumber].some(
        (jid) => typeof jid === "string" && matchesAnyJid(jid, ownJids),
      ),
    );
    if (!selfAdded) return;
    try {
      await announcePairing(update.id);
    } catch (error) {
      console.warn(`channels: WhatsApp group pairing announcement failed`, error);
    }
  });
  next.ev.on("connection.update", (update) => {
    if (socket !== next) {
      return;
    }
    if (update.qr !== undefined) {
      health.markIngressDisconnected("waiting for QR scan");
      if (printQr) {
        console.log("channels: scan the WhatsApp QR code");
        qrcode.generate(update.qr, { small: true });
      }
    }
    if (update.connection === "open") {
      registry.set(deliverySocket);
      health.markIngressConnected();
      console.log(`channels: WhatsApp ${accountId} connected`);
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
      health.markIngressDisconnected("logged out");
      console.error("channels: WhatsApp logged out; clear the auth directory and pair again");
      return;
    }
    health.markReconnectScheduled(`socket closed (${statusCode ?? "unknown"})`);
    reconnectTimer = setTimeout(connect, 3_000);
  });
}

connect();
const connection = await NativeConnection.connect({ address });
const worker = await Worker.create({
  connection,
  namespace,
  taskQueue,
  activities: {
    ...createWhatsAppDeliveryActivities({
      accountId,
      api: registry.deliveryApi(),
    }),
    ...createWhatsAppMediaActivities({
      accountId,
      locatorKey: mediaLocatorKey,
      lightspeedEndpoint,
    }),
    ...createWhatsAppPresenceActivities({
      accountId,
      api: registry.presenceApi(),
    }),
  },
});
health.markActivityWorkerReady();
const healthServer = await startConnectorHealthServer(health, {
  host: process.env.CHANNELS_HEALTH_HOST ?? "0.0.0.0",
  port: parseHealthPort(
    process.env.CHANNELS_WHATSAPP_HEALTH_PORT ?? process.env.CHANNELS_HEALTH_PORT,
    8_092,
  ),
  metrics,
});

console.log(`channels: WhatsApp ${accountId} polling activities on ${namespace}/${taskQueue}`);
console.log(`channels: WhatsApp health listening on port ${healthServer.port}`);
const lifecycle = new ConnectorLifecycle(health, () => {
  stopped = true;
  if (reconnectTimer !== undefined) {
    clearTimeout(reconnectTimer);
    reconnectTimer = undefined;
  }
  worker.shutdown();
  socket?.end(undefined);
});
const onSigint = () => lifecycle.requestStop("SIGINT");
const onSigterm = () => lifecycle.requestStop("SIGTERM");
process.once("SIGINT", onSigint);
process.once("SIGTERM", onSigterm);
try {
  await worker.run();
} finally {
  process.off("SIGINT", onSigint);
  process.off("SIGTERM", onSigterm);
  await lifecycle.finish(async () => {
    await clientConnection.close();
    await connection.close();
    await database.pool.end();
    await healthServer.close();
  });
}

function requiredEnvironment(name: string, legacyName?: string): string {
  const value = process.env[name] ?? (legacyName ? process.env[legacyName] : undefined);
  if (value === undefined || value.length === 0) {
    throw new TypeError(`${name} is required`);
  }
  return value;
}

function optionalNumber(value: unknown): number | undefined {
  if (value === null || value === undefined) return undefined;
  const number = Number(value);
  return Number.isSafeInteger(number) && number >= 0 ? number : undefined;
}
