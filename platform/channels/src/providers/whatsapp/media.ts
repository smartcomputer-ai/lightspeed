import { createCipheriv, createDecipheriv, randomBytes } from "node:crypto";
import { LightspeedClient } from "@lightspeed/agent-client";
import { ApplicationFailure } from "@temporalio/common";
import { downloadContentFromMessage } from "baileys";
import type { ChannelInboundMediaV1, ChannelMediaActivities } from "../../contracts/media.js";
import { parseChannelInboundMediaV1 } from "../../contracts/media.js";
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

export interface WhatsAppMediaActivityConfig {
  accountId: string;
  locatorKey: Uint8Array;
  lightspeedEndpoint: string;
  fetch?: typeof fetch;
  download?: (
    locator: { mediaKey: Uint8Array; directPath?: string; url?: string },
    type: WhatsAppMediaType,
  ) => Promise<AsyncIterable<Uint8Array>>;
}

export function parseWhatsAppMediaLocatorKey(value: string): Uint8Array {
  const key = Buffer.from(value, "base64");
  if (key.byteLength !== 32) {
    throw new TypeError(
      "LIGHTSPEED_CHANNELS_WHATSAPP_MEDIA_LOCATOR_KEY must be 32 bytes encoded as base64",
    );
  }
  return key;
}

export function describeWhatsAppMedia(
  accountId: string,
  locatorKey: Uint8Array,
  source: WhatsAppMediaSource,
): ChannelInboundMediaV1 | null {
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
    version: 1,
    provider: "whatsapp",
    fileId: sealWhatsAppMediaLocator(locatorKey, accountId, {
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
  const download = config.download ?? (async (locator, type) =>
    downloadContentFromMessage(locator, type));
  return {
    async prepareChannelMedia(input) {
      const media = parseChannelInboundMediaV1(input.media);
      if (
        input.route.provider !== "whatsapp" ||
        input.route.accountId !== config.accountId ||
        media.provider !== "whatsapp"
      ) {
        throw rejected("media is routed to the wrong provider worker");
      }
      const locator = openWhatsAppMediaLocator(config.locatorKey, config.accountId, media.fileId);
      if (locator.mediaType !== media.kind && !(locator.mediaType === "document" && media.kind === "document")) {
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
        const client = new LightspeedClient({
          endpoint: config.lightspeedEndpoint,
          ...(config.fetch === undefined ? {} : { fetch: config.fetch }),
          headers: { "x-lightspeed-universe": input.universeId },
        });
        const response = await client.call("blobs/put", {
          blobs: [{ bytesBase64: bytes.toString("base64") }],
        });
        const blob = response.result.blobs?.[0];
        if (blob === undefined) throw new Error("missing blob result");
        return {
          item: {
            type: "media",
            blobRef: blob.blobRef,
            kind: media.kind,
            mime: media.mime,
            ...(media.name === undefined ? {} : { name: media.name }),
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
  accountId: string,
  payload: WhatsAppMediaLocatorPayloadV1,
): string {
  assertKey(key);
  const nonce = randomBytes(12);
  const cipher = createCipheriv("aes-256-gcm", key, nonce);
  cipher.setAAD(Buffer.from(accountId, "utf8"));
  const ciphertext = Buffer.concat([
    cipher.update(JSON.stringify(payload), "utf8"),
    cipher.final(),
  ]);
  return `wam1.${Buffer.concat([nonce, cipher.getAuthTag(), ciphertext]).toString("base64url")}`;
}

function openWhatsAppMediaLocator(
  key: Uint8Array,
  accountId: string,
  sealed: string,
): WhatsAppMediaLocatorPayloadV1 {
  assertKey(key);
  try {
    if (!sealed.startsWith("wam1.")) throw new Error("invalid prefix");
    const bytes = Buffer.from(sealed.slice(5), "base64url");
    if (bytes.byteLength < 29) throw new Error("invalid length");
    const decipher = createDecipheriv("aes-256-gcm", key, bytes.subarray(0, 12));
    decipher.setAAD(Buffer.from(accountId, "utf8"));
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
