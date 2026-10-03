import type { ChannelInboundMedia, ChannelMediaKind } from "@lightspeed-ai/sdk";
import { audioMime, documentMime, imageMime, mediaByteLimit } from "./validation.js";

export const MAX_CHANNEL_MEDIA_PER_MESSAGE = 8;

/**
 * Validate a provider-owned attachment reference as it arrives in an
 * activity payload: kind, MIME (normalized to the admitted set), and the
 * declared size against the kind's limit. Never bytes.
 */
export function parseChannelInboundMedia(value: unknown): ChannelInboundMedia {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new TypeError("channel inbound media must be an object");
  }
  const media = value as Record<string, unknown>;
  nonEmptyString(media.fileId, "media.fileId");
  if (media.kind !== "image" && media.kind !== "audio" && media.kind !== "document") {
    throw new TypeError("channel inbound media kind is invalid");
  }
  nonEmptyString(media.mime, "media.mime");
  const name = media.name === undefined || media.name === null ? undefined : media.name;
  if (name !== undefined) {
    nonEmptyString(name, "media.name");
  }
  const byteSize =
    media.byteSize === undefined || media.byteSize === null ? undefined : media.byteSize;
  if (byteSize !== undefined && (!Number.isSafeInteger(byteSize) || (byteSize as number) < 0)) {
    throw new TypeError("media.byteSize must be a non-negative safe integer");
  }
  const kind = media.kind as ChannelMediaKind;
  const admittedMime =
    kind === "image"
      ? imageMime(media.mime as string)
      : kind === "audio"
        ? audioMime(name, media.mime as string)
        : documentMime(name, media.mime as string);
  if (admittedMime === null) {
    throw new TypeError(`unsupported ${kind} MIME`);
  }
  const limit = mediaByteLimit(kind, admittedMime);
  if (byteSize !== undefined && (byteSize as number) > limit) {
    throw new RangeError(`media exceeds the ${limit} byte limit`);
  }
  return {
    fileId: media.fileId as string,
    kind,
    mime: admittedMime,
    ...(name === undefined ? {} : { name }),
    ...(byteSize === undefined ? {} : { byteSize: byteSize as number }),
  };
}

export function mediaPlaceholder(media: ChannelInboundMedia | undefined): string {
  if (media === undefined) return "";
  if (media.kind === "image") return "(sent an image)";
  if (media.kind === "audio") {
    return media.name === "voice.ogg"
      ? "(sent a voice note)"
      : `(sent audio: ${media.name ?? "audio"})`;
  }
  return `(sent a file: ${media.name ?? "document"})`;
}

function nonEmptyString(value: unknown, name: string): asserts value is string {
  if (typeof value !== "string" || value.length === 0) {
    throw new TypeError(`${name} must be a non-empty string`);
  }
}
