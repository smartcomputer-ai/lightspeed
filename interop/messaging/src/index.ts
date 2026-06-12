export type {
  BridgeRuntimeConfig,
  ForgeBridgeConfig,
  MessagingBridgeConfig,
  TelegramBridgeConfig,
  WhatsAppBridgeConfig,
} from "./config.js";
export { loadBridgeConfig } from "./config.js";
export { extractAssistantText, ForgeSessionBridge } from "./forge.js";
export type { ForgeReply, ForgeRoomEvent, ForgeTurn, ForgeTurnMedia } from "./forge.js";
export { classifyInbound, formatEnvelope, parseControlCommand, shouldQuoteChunk } from "./policy.js";
export type {
  ActivationPolicy,
  Classification,
  ClassifyInput,
  ClassifyOptions,
  ControlCommand,
  EnvelopeInput,
  GroupActivation,
  ReplyToMode,
} from "./policy.js";
export { RoomBuffer, TurnDebouncer } from "./batcher.js";
export type { RoomBufferOptions, RoomEventItem, TurnDebouncerOptions } from "./batcher.js";
export { MessagingBridgeRuntime } from "./runtime.js";
export type {
  ChannelPolicy,
  HandleInboundOptions,
  InboundMedia,
  MessagingBridgeRuntimeOptions,
  NormalizedInbound,
} from "./runtime.js";
export { JsonBridgeStore } from "./store.js";
export type { BindingInit, BindingState, BridgeState, ConversationState, MessageState } from "./store.js";
export { startTelegramBridge } from "./telegram.js";
export type { RunningBridge } from "./telegram.js";
export { startWhatsAppBridge } from "./whatsapp.js";
export type { RunningWhatsAppBridge } from "./whatsapp.js";
export { extractTriggeredText, splitMessageText } from "./text.js";
export type { TriggerOptions } from "./text.js";
