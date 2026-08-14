import { LightspeedClient } from "@lightspeed/agent-client";
import { ApplicationFailure } from "@temporalio/common";
import type {
  ChannelMediaActivities,
  PrepareChannelMediaInput,
} from "../../contracts/media.js";
import { parseChannelInboundMediaV1 } from "../../contracts/media.js";
import { mediaByteLimit } from "../../media/validation.js";

export interface TelegramFileApi {
  getFile(fileId: string): Promise<{ file_path?: string; file_size?: number }>;
}

export interface TelegramMediaActivityConfig {
  accountId: string;
  botToken: string;
  lightspeedEndpoint: string;
  api: TelegramFileApi;
  fetch?: typeof fetch;
}

export function createTelegramMediaActivities(
  config: TelegramMediaActivityConfig,
): ChannelMediaActivities {
  const request = config.fetch ?? fetch;
  return {
    async prepareChannelMedia(input) {
      const media = validateInput(input, config.accountId);
      const limit = mediaByteLimit(media.kind, media.mime);
      if (media.byteSize !== undefined && media.byteSize > limit) {
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
        const response = await request(telegramFileUrl(config.botToken, remote.file_path));
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
        const client = new LightspeedClient({
          endpoint: config.lightspeedEndpoint,
          ...(config.fetch === undefined ? {} : { fetch: config.fetch }),
          headers: { "x-lightspeed-universe": input.universeId },
        });
        const response = await client.call("blobs/put", {
          blobs: [{ bytesBase64: Buffer.from(bytes).toString("base64") }],
        });
        const blob = response.result.blobs?.[0];
        if (blob === undefined) {
          throw new Error("missing blob result");
        }
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

function validateInput(input: PrepareChannelMediaInput, accountId: string) {
  if (typeof input.universeId !== "string" || input.universeId.length === 0) {
    throw mediaRejected("universeId is required");
  }
  const media = parseChannelInboundMediaV1(input.media);
  if (
    input.route.provider !== "telegram" ||
    input.route.accountId !== accountId ||
    media.provider !== "telegram"
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
