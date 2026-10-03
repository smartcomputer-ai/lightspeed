import { createCipheriv, createDecipheriv, randomBytes } from "node:crypto";
import type { ChannelInboundMedia, LightspeedClient } from "@lightspeed-ai/sdk";
import type {
  PrepareChannelMediaInput,
  PrepareChannelMediaResult,
} from "@lightspeed-ai/sdk/workflow";
import { ApplicationFailure } from "@temporalio/common";
import { downloadContentFromMessage } from "baileys";
import { parseChannelInboundMedia } from "../../media/inbound.js";
import {
  audioMime,
  documentMime,
  imageMime,
  mediaByteLimit,
} from "../../media/validation.js";

type WhatsAppMediaType = "image" | "audio" | "document";

interface WhatsAppMediaLocatorPayloadV1 {
  version: 1;
  mediaType: WhatsAppMediaType;
  mediaKeyBase64: string;
  directPath?: string;
  url?: string;
}

export interface WhatsAppMediaSource {
  messageId: string;
  mediaType: WhatsAppMediaType;
  reportedMime?: string | null | undefined;
  fileName?: string | null | undefined;
  byteSize?: number | undefined;
  mediaKey?: Uint8Array | null | undefined;
  directPath?: string | null | undefined;
  url?: string | null | undefined;
  voiceNote?: boolean | undefined;
}

export interface ChannelMediaActivities {
  prepareChannelMedia(input: PrepareChannelMediaInput): Promise<PrepareChannelMediaResult>;
}

export interface WhatsAppMediaActivityConfig {
  universeId: string;
  accountId: string;
  locatorKey: Uint8Array;
  /** Universe-scoped core client for `blobs/put`. */
  core: Pick<LightspeedClient, "call">;
  download?: (
    locator: { mediaKey: Uint8Array; directPath?: string; url?: string },
    type: WhatsAppMediaType,
  ) => Promise<AsyncIterable<Uint8Array>>;
}

export function parseWhatsAppMediaLocatorKey(value: string): Uint8Array {
  const key = Buffer.from(value, "base64");
  if (key.byteLength !== 32) {
    throw new TypeError(
      "LIGHTSPEED_CONNECTOR_WHATSAPP_MEDIA_LOCATOR_KEY must be 32 bytes encoded as base64",
    );
  }
  return key;
}

/** Locators are bound to the account inside its universe; another account cannot open them. */
export function whatsAppMediaScope(universeId: string, accountId: string): string {
  return `${universeId.toLowerCase()}/${accountId}`;
}

/**
 * Describe an attachment for the inbound envelope: only a sealed locator
 * (media key, path) travels; the bytes stay with WhatsApp until the
 * conversation asks for them.
 */
export function describeWhatsAppMedia(
  scope: string,
  locatorKey: Uint8Array,
  source: WhatsAppMediaSource,
): ChannelInboundMedia | null {
  if (source.mediaKey == null || (source.directPath == null && source.url == null)) {
    return null;
  }
  const kind = source.mediaType === "image" ? "image" : source.mediaType;
  const mime =
    kind === "image"
      ? imageMime(source.reportedMime ?? "image/jpeg")
      : kind === "audio"
        ? audioMime(source.fileName, source.reportedMime ?? (source.voiceNote ? "audio/ogg" : null))
        : documentMime(source.fileName, source.reportedMime);
  if (mime === null) return null;
  if (source.byteSize !== undefined && source.byteSize > mediaByteLimit(kind, mime)) return null;
  const name =
    source.fileName ??
    (source.voiceNote ? "voice.ogg" : kind === "image" ? "image" : kind);
  return {
    fileId: sealWhatsAppMediaLocator(locatorKey, scope, {
      version: 1,
      mediaType: source.mediaType,
      mediaKeyBase64: Buffer.from(source.mediaKey).toString("base64"),
      ...(source.directPath == null ? {} : { directPath: source.directPath }),
      ...(source.url == null ? {} : { url: source.url }),
    }),
    kind,
    mime,
    name,
    ...(source.byteSize === undefined ? {} : { byteSize: source.byteSize }),
  };
}

export function createWhatsAppMediaActivities(
  config: WhatsAppMediaActivityConfig,
): ChannelMediaActivities {
  assertKey(config.locatorKey);
  const scope = whatsAppMediaScope(config.universeId, config.accountId);
  const download = config.download ?? (async (locator, type) =>
    downloadContentFromMessage(locator, type));
  return {
    async prepareChannelMedia(input) {
      const media = parseChannelInboundMedia(input.media);
      if (
        input.route.provider !== "whatsapp" ||
        input.route.accountId !== config.accountId ||
        typeof input.universeId !== "string" ||
        input.universeId.toLowerCase() !== config.universeId.toLowerCase()
      ) {
        throw rejected("media is routed to the wrong provider worker");
      }
      const locator = openWhatsAppMediaLocator(config.locatorKey, scope, media.fileId);
      if (locator.mediaType !== media.kind) {
        throw rejected("media locator kind does not match its envelope");
      }
      const limit = mediaByteLimit(media.kind, media.mime);
      let bytes: Buffer;
      try {
        const stream = await download(
          {
            mediaKey: Buffer.from(locator.mediaKeyBase64, "base64"),
            ...(locator.directPath === undefined ? {} : { directPath: locator.directPath }),
            ...(locator.url === undefined ? {} : { url: locator.url }),
          },
          locator.mediaType,
        );
        bytes = await readStreamUpTo(stream, limit);
      } catch (error) {
        if (error instanceof ApplicationFailure) throw error;
        throw ApplicationFailure.create({
          message: "WhatsApp media transfer failed",
          type: "ChannelMediaTransferFailed",
        });
      }
      try {
        const response = await config.core.call("blobs/put", {
          blobs: [{ bytesBase64: bytes.toString("base64") }],
        });
        const blob = response.result.blobs?.[0];
        if (blob === undefined) throw new Error("missing blob result");
        return {
          item: {
            blobRef: blob.blobRef,
            kind: media.kind,
            mime: media.mime,
            ...(media.name == null ? {} : { name: media.name }),
          },
        };
      } catch {
        throw ApplicationFailure.create({
          message: "Lightspeed media upload failed",
          type: "ChannelMediaTransferFailed",
        });
      }
    },
  };
}

function sealWhatsAppMediaLocator(
  key: Uint8Array,
  scope: string,
  payload: WhatsAppMediaLocatorPayloadV1,
): string {
  assertKey(key);
  const nonce = randomBytes(12);
  const cipher = createCipheriv("aes-256-gcm", key, nonce);
  cipher.setAAD(Buffer.from(scope, "utf8"));
  const ciphertext = Buffer.concat([
    cipher.update(JSON.stringify(payload), "utf8"),
    cipher.final(),
  ]);
  return `wam1.${Buffer.concat([nonce, cipher.getAuthTag(), ciphertext]).toString("base64url")}`;
}

function openWhatsAppMediaLocator(
  key: Uint8Array,
  scope: string,
  sealed: string,
): WhatsAppMediaLocatorPayloadV1 {
  assertKey(key);
  try {
    if (!sealed.startsWith("wam1.")) throw new Error("invalid prefix");
    const bytes = Buffer.from(sealed.slice(5), "base64url");
    if (bytes.byteLength < 29) throw new Error("invalid length");
    const decipher = createDecipheriv("aes-256-gcm", key, bytes.subarray(0, 12));
    decipher.setAAD(Buffer.from(scope, "utf8"));
    decipher.setAuthTag(bytes.subarray(12, 28));
    const raw = JSON.parse(
      Buffer.concat([decipher.update(bytes.subarray(28)), decipher.final()]).toString("utf8"),
    ) as Record<string, unknown>;
    if (
      raw.version !== 1 ||
      (raw.mediaType !== "image" && raw.mediaType !== "audio" && raw.mediaType !== "document") ||
      typeof raw.mediaKeyBase64 !== "string" ||
      Buffer.from(raw.mediaKeyBase64, "base64").byteLength === 0 ||
      (typeof raw.directPath !== "string" && typeof raw.url !== "string")
    ) {
      throw new Error("invalid payload");
    }
    return raw as unknown as WhatsAppMediaLocatorPayloadV1;
  } catch {
    throw rejected("WhatsApp media locator is invalid or expired");
  }
}

async function readStreamUpTo(
  stream: AsyncIterable<Uint8Array>,
  limit: number,
): Promise<Buffer> {
  const chunks: Buffer[] = [];
  let total = 0;
  for await (const chunk of stream) {
    total += chunk.byteLength;
    if (total > limit) throw rejected("downloaded file exceeds the supported limit");
    chunks.push(Buffer.from(chunk));
  }
  return Buffer.concat(chunks, total);
}

function assertKey(key: Uint8Array): void {
  if (key.byteLength !== 32) throw new TypeError("WhatsApp media locator key must be 32 bytes");
}

function rejected(message: string): ApplicationFailure {
  return ApplicationFailure.nonRetryable(message, "ChannelMediaRejected");
}
