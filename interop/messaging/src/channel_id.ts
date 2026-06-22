/// Message envelopes display channel ids as `#id` so they are easy for the
/// model to reference. Channel APIs need the raw id.
export function cleanChannelMessageId(messageId: string): string {
  return messageId.trim().replace(/^#/, "").trim();
}
