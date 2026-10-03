import type { LightspeedClient } from "@lightspeed-ai/client";
import type {
  PrepareChannelMediaInput,
  PrepareChannelMediaResult,
} from "@lightspeed-ai/client/workflow";
import { ApplicationFailure } from "@temporalio/common";
import type { TokenSource } from "../../core/leases.js";
import { parseChannelInboundMedia } from "../../media/inbound.js";
import { mediaByteLimit } from "../../media/validation.js";

export interface TelegramFileApi {
  getFile(fileId: string): Promise<{ file_path?: string; file_size?: number }>;
}

export interface ChannelMediaActivities {
  prepareChannelMedia(input: PrepareChannelMediaInput): Promise<PrepareChannelMediaResult>;
}

export interface TelegramMediaActivityConfig {
  universeId: string;
  accountId: string;
  /** The leased bot token; the download URL embeds it. */
  botToken: TokenSource;
  /** Universe-scoped core client for `blobs/put`. */
  core: Pick<LightspeedClient, "call">;
  api: TelegramFileApi;
  fetch?: typeof fetch;
}

/**
 * `prepareChannelMedia`: download the Telegram file on the account's worker
 * and store it in the universe's CAS; the result is a reference, never bytes.
 */
export function createTelegramMediaActivities(
  config: TelegramMediaActivityConfig,
): ChannelMediaActivities {
  const request = config.fetch ?? fetch;
  return {
    async prepareChannelMedia(input) {
      const media = validateInput(input, config);
      const limit = mediaByteLimit(media.kind, media.mime);
      if (media.byteSize != null && media.byteSize > limit) {
        throw mediaRejected("declared file size exceeds the supported limit");
      }

      let remote: Awaited<ReturnType<TelegramFileApi["getFile"]>>;
      try {
        remote = await config.api.getFile(media.fileId);
      } catch {
        throw mediaTransferFailed();
      }
      if (remote.file_path === undefined || remote.file_path.length === 0) {
        throw mediaRejected("Telegram did not return a downloadable file path");
      }
      if (remote.file_size !== undefined && remote.file_size > limit) {
        throw mediaRejected("remote file size exceeds the supported limit");
      }

      let bytes: Uint8Array;
      try {
        const token = await config.botToken.get();
        const response = await request(telegramFileUrl(token, remote.file_path));
        if (response.status === 401) {
          config.botToken.invalidate();
        }
        if (!response.ok) {
          throw new Error("download rejected");
        }
        const contentLength = response.headers.get("content-length");
        if (contentLength !== null && Number(contentLength) > limit) {
          throw mediaRejected("download size exceeds the supported limit");
        }
        bytes = await readResponseUpTo(response, limit);
      } catch (error) {
        if (error instanceof ApplicationFailure) throw error;
        throw mediaTransferFailed();
      }
      try {
        const response = await config.core.call("blobs/put", {
          blobs: [{ bytesBase64: Buffer.from(bytes).toString("base64") }],
        });
        const blob = response.result.blobs?.[0];
        if (blob === undefined) {
          throw new Error("missing blob result");
        }
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

function validateInput(
  input: PrepareChannelMediaInput,
  config: Pick<TelegramMediaActivityConfig, "accountId" | "universeId">,
) {
  if (typeof input.universeId !== "string" || input.universeId.length === 0) {
    throw mediaRejected("universeId is required");
  }
  const media = parseChannelInboundMedia(input.media);
  if (
    input.route.provider !== "telegram" ||
    input.route.accountId !== config.accountId ||
    input.universeId.toLowerCase() !== config.universeId.toLowerCase()
  ) {
    throw mediaRejected("media is routed to the wrong provider worker");
  }
  return media;
}

function telegramFileUrl(token: string, filePath: string): string {
  const path = filePath.split("/");
  if (path.some((part) => part.length === 0 || part === "." || part === "..")) {
    throw mediaRejected("Telegram returned an invalid file path");
  }
  return `https://api.telegram.org/file/bot${token}/${path.map(encodeURIComponent).join("/")}`;
}

async function readResponseUpTo(response: Response, limit: number): Promise<Uint8Array> {
  if (response.body === null) {
    const bytes = new Uint8Array(await response.arrayBuffer());
    if (bytes.byteLength > limit) {
      throw mediaRejected("downloaded file exceeds the supported limit");
    }
    return bytes;
  }
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    for (;;) {
      const read = await reader.read();
      if (read.done) break;
      total += read.value.byteLength;
      if (total > limit) {
        await reader.cancel();
        throw mediaRejected("downloaded file exceeds the supported limit");
      }
      chunks.push(read.value);
    }
  } finally {
    reader.releaseLock();
  }
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return bytes;
}

function mediaRejected(message: string): ApplicationFailure {
  return ApplicationFailure.nonRetryable(message, "ChannelMediaRejected");
}

function mediaTransferFailed(): ApplicationFailure {
  return ApplicationFailure.create({
    message: "Telegram media transfer failed",
    type: "ChannelMediaTransferFailed",
  });
}
