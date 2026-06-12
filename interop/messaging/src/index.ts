export type {
  ForgeBridgeConfig,
  MessagingBridgeConfig,
  TelegramBridgeConfig,
  WhatsAppBridgeConfig,
} from "./config.js";
export { loadBridgeConfig } from "./config.js";
export { extractLatestAssistantText, ForgeSessionBridge } from "./forge.js";
export type { ForgeInboundText, ForgeReply } from "./forge.js";
export { MessagingBridgeRuntime } from "./runtime.js";
export type {
  HandleInboundOptions,
  InboundTextMessage,
  MessagingBridgeRuntimeOptions,
} from "./runtime.js";
export { JsonBridgeStore } from "./store.js";
export type { BridgeState, ConversationState, MessageState } from "./store.js";
export { startTelegramBridge } from "./telegram.js";
export type { RunningBridge } from "./telegram.js";
export { startWhatsAppBridge } from "./whatsapp.js";
export type { RunningWhatsAppBridge } from "./whatsapp.js";
export { extractTriggeredText, splitMessageText } from "./text.js";
export type { TriggerOptions } from "./text.js";
