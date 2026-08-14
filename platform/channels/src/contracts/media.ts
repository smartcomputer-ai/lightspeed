import type { ChannelProvider, ChannelRoute } from "./channel.js";
import {
  audioMime,
  documentMime,
  imageMime,
  mediaByteLimit,
} from "../media/validation.js";

export type ChannelMediaKind = "image" | "audio" | "document";

/** A provider-owned attachment reference. It is safe to persist in Temporal history. */
export interface ChannelInboundMediaV1 {
  version: 1;
  provider: ChannelProvider;
  fileId: string;
  kind: ChannelMediaKind;
  mime: string;
  name?: string;
  byteSize?: number;
}

/** Lightspeed-native input items. Media items reference CAS; they never contain bytes. */
export type ChannelInputItem =
  | { type: "text"; text: string }
  | { type: "textRef"; blobRef: string }
  | {
      type: "media";
      blobRef: string;
      kind: ChannelMediaKind;
      mime: string;
      name?: string | null;
    };

export interface PrepareChannelMediaInput {
  universeId: string;
  route: ChannelRoute;
  media: ChannelInboundMediaV1;
}

export interface PrepareChannelMediaResult {
  item: Extract<ChannelInputItem, { type: "media" }>;
}

export interface ChannelMediaActivities {
  prepareChannelMedia(input: PrepareChannelMediaInput): Promise<PrepareChannelMediaResult>;
}

export const MAX_CHANNEL_MEDIA_PER_MESSAGE = 8;

export function parseChannelInboundMediaV1(value: unknown): ChannelInboundMediaV1 {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new TypeError("channel inbound media must be an object");
  }
  const media = value as Record<string, unknown>;
  if (media.version !== 1) {
    throw new TypeError("channel inbound media version must be 1");
  }
  if (media.provider !== "telegram" && media.provider !== "whatsapp") {
    throw new TypeError("channel inbound media provider is invalid");
  }
  nonEmptyString(media.fileId, "media.fileId");
  if (media.kind !== "image" && media.kind !== "audio" && media.kind !== "document") {
    throw new TypeError("channel inbound media kind is invalid");
  }
  nonEmptyString(media.mime, "media.mime");
  if (media.name !== undefined) {
    nonEmptyString(media.name, "media.name");
  }
  if (
    media.byteSize !== undefined &&
    (!Number.isSafeInteger(media.byteSize) || (media.byteSize as number) < 0)
  ) {
    throw new TypeError("media.byteSize must be a non-negative safe integer");
  }
  const parsed = media as unknown as ChannelInboundMediaV1;
  const admittedMime =
    parsed.kind === "image"
      ? imageMime(parsed.mime)
      : parsed.kind === "audio"
        ? audioMime(parsed.name, parsed.mime)
        : documentMime(parsed.name, parsed.mime);
  if (admittedMime === null) {
    throw new TypeError(`unsupported ${parsed.kind} MIME`);
  }
  const normalized = { ...parsed, mime: admittedMime };
  const limit = mediaByteLimit(normalized.kind, normalized.mime);
  if (normalized.byteSize !== undefined && normalized.byteSize > limit) {
    throw new RangeError(`media exceeds the ${limit} byte limit`);
  }
  return normalized;
}

export function mediaLabel(media: Pick<ChannelInboundMediaV1, "kind" | "name">): string {
  return media.name === undefined ? `[${media.kind}]` : `[${media.kind}: ${media.name}]`;
}

function nonEmptyString(value: unknown, name: string): asserts value is string {
  if (typeof value !== "string" || value.length === 0) {
    throw new TypeError(`${name} must be a non-empty string`);
  }
}
