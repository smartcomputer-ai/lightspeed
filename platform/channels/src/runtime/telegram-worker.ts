import { Bot } from "grammy";
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
import { createTelegramDeliveryActivities } from "../providers/telegram/delivery.js";
import { normalizeTelegramInbound } from "../providers/telegram/ingress.js";
import { createTelegramMediaActivities } from "../providers/telegram/media.js";
import { createTelegramPresenceActivities } from "../providers/telegram/presence.js";
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
const botToken = requiredEnvironment("LIGHTSPEED_CHANNELS_TELEGRAM_BOT_TOKEN");
const accountId = requiredEnvironment("LIGHTSPEED_CHANNELS_TELEGRAM_ACCOUNT_ID");
const databaseUrl = requiredEnvironment("LIGHTSPEED_PLATFORM_DATABASE_URL");
const lightspeedEndpoint = requiredEnvironment("LIGHTSPEED_ENDPOINT");
const taskQueue = channelDeliveryTaskQueue("telegram", accountId);
const health = new ConnectorHealthTracker("telegram", accountId);
const metrics = new ConnectorMetrics();
const ingressRateLimit = new FixedWindowRateLimiter({
  limit: parsePositiveInteger(process.env.LIGHTSPEED_CHANNELS_INGRESS_MAX_PER_MINUTE, 120),
  windowMs: 60_000,
  maxKeys: 10_000,
});

const bot = new Bot(botToken);
await bot.init();
const database = createDb(databaseUrl);
const channelControlPlane = createDbChannelControlPlane(database.db);
const clientConnection = await Connection.connect({ address });
const workflowClient = new WorkflowClient({ connection: clientConnection, namespace });

installTemporalMetrics("channels-telegram", 9_091);

bot.catch((error) => {
  metrics.recordInbound("failed");
  console.error("channels: Telegram ingress handler failed", error.error);
});
bot.on("message", async (ctx) => {
  const message = ctx.message;
  if (message.from === undefined) {
    return;
  }
  const inbound = normalizeTelegramInbound(
    {
      accountId,
      botId: bot.botInfo.id,
      ...(bot.botInfo.username === undefined ? {} : { botUsername: bot.botInfo.username }),
    },
    {
      messageId: message.message_id,
      chatId: message.chat.id,
      chatType: message.chat.type,
      ...(message.message_thread_id === undefined
        ? {}
        : { threadId: message.message_thread_id }),
      senderId: message.from.id,
      ...(message.from.username === undefined ? {} : { senderUsername: message.from.username }),
      ...(message.from.first_name === undefined
        ? {}
        : { senderFirstName: message.from.first_name }),
      ...(message.from.last_name === undefined
        ? {}
        : { senderLastName: message.from.last_name }),
      timestampMs: message.date * 1_000,
      ...(message.text === undefined ? {} : { text: message.text }),
      ...(message.caption === undefined ? {} : { caption: message.caption }),
      ...(message.entities === undefined
        ? {}
        : {
            entities: message.entities.map(({ type, offset, length }) => ({
              type,
              offset,
              length,
            })),
          }),
      ...(message.caption_entities === undefined
        ? {}
        : {
            captionEntities: message.caption_entities.map(({ type, offset, length }) => ({
              type,
              offset,
              length,
            })),
          }),
      ...(message.reply_to_message?.from?.id === undefined
        ? {}
        : { replyToSenderId: message.reply_to_message.from.id }),
      ...(message.photo === undefined
        ? {}
        : {
            photos: message.photo.map((photo) => ({
              fileId: photo.file_id,
              width: photo.width,
              height: photo.height,
              ...(photo.file_size === undefined ? {} : { fileSize: photo.file_size }),
            })),
          }),
      ...(message.document === undefined
        ? {}
        : {
            document: {
              fileId: message.document.file_id,
              ...(message.document.file_size === undefined
                ? {}
                : { fileSize: message.document.file_size }),
              ...(message.document.file_name === undefined
                ? {}
                : { fileName: message.document.file_name }),
              ...(message.document.mime_type === undefined
                ? {}
                : { mimeType: message.document.mime_type }),
            },
          }),
      ...(message.voice === undefined
        ? {}
        : {
            voice: {
              fileId: message.voice.file_id,
              ...(message.voice.file_size === undefined ? {} : { fileSize: message.voice.file_size }),
              ...(message.voice.mime_type === undefined ? {} : { mimeType: message.voice.mime_type }),
            },
          }),
      ...(message.audio === undefined
        ? {}
        : {
            audio: {
              fileId: message.audio.file_id,
              ...(message.audio.file_size === undefined ? {} : { fileSize: message.audio.file_size }),
              ...(message.audio.file_name === undefined ? {} : { fileName: message.audio.file_name }),
              ...(message.audio.mime_type === undefined ? {} : { mimeType: message.audio.mime_type }),
            },
          }),
    },
  );
  if (inbound === null) {
    return;
  }
  if (!ingressRateLimit.allow(`${inbound.route.chatId}\0${inbound.senderId}`)) {
    metrics.recordInbound("rate_limited");
    console.warn(`channels: Telegram ingress rate limited chat ${inbound.route.chatId}`);
    return;
  }
  const admitted = await admitInbound(workflowClient, channelControlPlane, inbound);
  metrics.recordInbound(admitted.status);
  if (admitted.status === "pairing_required" || admitted.status === "paired") {
    await bot.api.sendMessage(
      message.chat.id,
      admitted.status === "paired" ? PAIRING_CONFIRMED_REPLY : PAIRING_REQUIRED_REPLY,
      {
        ...(message.message_thread_id === undefined
          ? {}
          : { message_thread_id: message.message_thread_id }),
        reply_parameters: { message_id: message.message_id },
      },
    );
  } else if (admitted.status === "unbound") {
    console.warn(`channels: Telegram ignored unbound chat ${inbound.route.chatId}`);
  }
});

const connection = await NativeConnection.connect({ address });
const worker = await Worker.create({
  connection,
  namespace,
  taskQueue,
  activities: {
    ...createTelegramDeliveryActivities({
      accountId,
      api: {
        sendMessage: (chatId, text, options) => bot.api.sendMessage(chatId, text, options),
        editMessageText: (chatId, messageId, text, options) =>
          bot.api.editMessageText(chatId, messageId, text, options),
        setMessageReaction: (chatId, messageId, reactions) =>
          bot.api.setMessageReaction(chatId, messageId, reactions as never),
      },
    }),
    ...createTelegramMediaActivities({
      accountId,
      botToken,
      lightspeedEndpoint,
      api: { getFile: (fileId) => bot.api.getFile(fileId) },
    }),
    ...createTelegramPresenceActivities({
      accountId,
      api: {
        sendChatAction: (chatId, action, options) =>
          bot.api.sendChatAction(chatId, action, options),
      },
    }),
  },
});
health.markActivityWorkerReady();
const healthServer = await startConnectorHealthServer(health, {
  host: process.env.LIGHTSPEED_CHANNELS_HEALTH_HOST ?? "0.0.0.0",
  port: parseHealthPort(
    process.env.LIGHTSPEED_CHANNELS_TELEGRAM_HEALTH_PORT ??
      process.env.LIGHTSPEED_CHANNELS_HEALTH_PORT,
    8_091,
  ),
  metrics,
});

console.log(
  `channels: Telegram ${bot.botInfo.username} polling activities on ${namespace}/${taskQueue}`,
);
console.log(`channels: Telegram health listening on port ${healthServer.port}`);
const lifecycle = new ConnectorLifecycle(health, () => {
  worker.shutdown();
  if (bot.isRunning()) {
    void bot.stop().catch((error: unknown) => {
      console.error("channels: Telegram stop failed", error);
    });
  }
});
const onSigint = () => lifecycle.requestStop("SIGINT");
const onSigterm = () => lifecycle.requestStop("SIGTERM");
process.once("SIGINT", onSigint);
process.once("SIGTERM", onSigterm);
try {
  await Promise.all([
    worker.run(),
    bot.start({
      onStart: () => {
        health.markIngressConnected();
        console.log(`channels: Telegram ${bot.botInfo.username} ingress ready`);
      },
    }),
  ]);
} finally {
  process.off("SIGINT", onSigint);
  process.off("SIGTERM", onSigterm);
  await lifecycle.finish(async () => {
    if (bot.isRunning()) {
      await bot.stop();
    }
    await clientConnection.close();
    await connection.close();
    await database.pool.end();
    await healthServer.close();
  });
}

function requiredEnvironment(name: string): string {
  const value = process.env[name];
  if (value === undefined || value.length === 0) {
    throw new TypeError(`${name} is required`);
  }
  return value;
}
