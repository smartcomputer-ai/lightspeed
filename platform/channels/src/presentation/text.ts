import { ChannelDeliveryError } from "../contracts/delivery.js";

export function splitMessageText(
  text: string,
  maxChars: number,
  maxChunks = 32,
): string[] {
  const limit = Math.max(1, Math.floor(maxChars));
  const chunks: string[] = [];
  let remaining = text;
  while (remaining.length > limit) {
    let cut = remaining.lastIndexOf("\n", limit);
    if (cut < limit * 0.5) {
      cut = remaining.lastIndexOf(" ", limit);
    }
    if (cut < limit * 0.5) {
      cut = limit;
    }
    chunks.push(remaining.slice(0, cut).trimEnd());
    remaining = remaining.slice(cut).trimStart();
    if (chunks.length >= maxChunks) {
      throw new ChannelDeliveryError(
        `message exceeds the ${maxChunks}-chunk provider delivery limit`,
        false,
      );
    }
  }
  if (remaining.length > 0) {
    chunks.push(remaining);
  }
  return chunks;
}
