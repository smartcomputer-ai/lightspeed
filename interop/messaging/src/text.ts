export interface TriggerOptions {
  botUsername?: string | null;
  mentionNames?: readonly string[];
  prefixes: readonly string[];
  requireTrigger: boolean;
}

export function extractTriggeredText(text: string, options: TriggerOptions): string | null {
  const trimmed = text.trim();
  if (!trimmed) {
    return null;
  }

  for (const prefix of options.prefixes) {
    const normalizedPrefix = prefix.trim();
    if (!normalizedPrefix) {
      continue;
    }
    const slashMatch = new RegExp(`^${escapeRegExp(normalizedPrefix)}(?:@[\\w_]+)?(?:\\s+|$)`, "i");
    if (slashMatch.test(trimmed)) {
      return trimmed.replace(slashMatch, "").trim();
    }
    if (trimmed.toLowerCase().startsWith(normalizedPrefix.toLowerCase())) {
      return trimmed.slice(normalizedPrefix.length).trim();
    }
  }

  const mentionNames = new Set(
    [
      ...(options.botUsername ? [options.botUsername] : []),
      ...(options.mentionNames ?? []),
    ]
      .map((name) => name.trim().replace(/^@/, "").toLowerCase())
      .filter(Boolean),
  );
  for (const mention of mentionNames) {
    const pattern = new RegExp(`^@${escapeRegExp(mention)}(?:[:,]?\\s+|$)`, "i");
    if (pattern.test(trimmed)) {
      return trimmed.replace(pattern, "").trim();
    }
  }

  return options.requireTrigger ? null : trimmed;
}

export function splitMessageText(text: string, maxChars: number): string[] {
  const limit = Math.max(1, Math.floor(maxChars));
  if (text.length <= limit) {
    return [text];
  }
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
  }
  if (remaining) {
    chunks.push(remaining);
  }
  return chunks;
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
