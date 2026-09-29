import { z } from "zod";

/// Attachments a composer message may carry. The runtime accepts these media
/// types as run input and every agent adapter hands them to the model
/// natively: images and PDFs as media, the text types inlined as text. The
/// size limits mirror the runtime's admission limits so a refusal happens
/// at pick time instead of after the upload.
const MiB = 1024 * 1024;
export const MAX_MESSAGE_ATTACHMENTS = 8;
export const MAX_ATTACHMENT_BYTES = 10 * MiB;
/// Anthropic's Messages API rejects larger images than the runtime does.
export const MAX_ANTHROPIC_IMAGE_BYTES = 5 * MiB;

export type AttachmentKind = "image" | "document";
export interface AttachmentType {
  mime: string;
  kind: AttachmentKind;
  maxBytes: number;
  /// Short type name for chips and refusals, e.g. "PNG" or "PDF".
  label: string;
}

const TYPES: readonly AttachmentType[] = [
  { mime: "image/png", kind: "image", maxBytes: MAX_ATTACHMENT_BYTES, label: "PNG" },
  { mime: "image/jpeg", kind: "image", maxBytes: MAX_ATTACHMENT_BYTES, label: "JPEG" },
  { mime: "image/webp", kind: "image", maxBytes: MAX_ATTACHMENT_BYTES, label: "WebP" },
  { mime: "image/gif", kind: "image", maxBytes: MAX_ATTACHMENT_BYTES, label: "GIF" },
  { mime: "application/pdf", kind: "document", maxBytes: MAX_ATTACHMENT_BYTES, label: "PDF" },
  { mime: "text/plain", kind: "document", maxBytes: MiB, label: "Text" },
  { mime: "text/markdown", kind: "document", maxBytes: MiB, label: "Markdown" },
  { mime: "text/csv", kind: "document", maxBytes: MiB, label: "CSV" },
  { mime: "application/json", kind: "document", maxBytes: MiB, label: "JSON" },
];
const BY_EXTENSION: Readonly<Record<string, string>> = {
  png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", webp: "image/webp", gif: "image/gif",
  pdf: "application/pdf", txt: "text/plain", text: "text/plain", log: "text/plain",
  md: "text/markdown", markdown: "text/markdown", csv: "text/csv", json: "application/json",
};
export const ATTACHMENT_MIMES = TYPES.map((type) => type.mime);
/// For a file input's `accept`: MIME types plus extensions browsers often
/// report without one (Markdown in particular).
export const ATTACHMENT_ACCEPT = [...ATTACHMENT_MIMES, ...Object.keys(BY_EXTENSION).map((ext) => `.${ext}`)].join(",");
/// A one-line list for tooltips and refusals.
export const ATTACHMENT_SUMMARY = "Images (PNG, JPEG, WebP, GIF), PDFs, and text files (TXT, Markdown, CSV, JSON)";

/// The accepted type for a picked file, or null. Browsers report an empty or
/// vendor MIME type for some text formats, so the extension decides when the
/// reported type is not one the model accepts.
export function attachmentType(mime: string, name: string): AttachmentType | null {
  const reported = mime.split(";")[0]!.trim().toLowerCase();
  const direct = TYPES.find((type) => type.mime === reported);
  if (direct) return direct;
  const ext = name.includes(".") ? name.slice(name.lastIndexOf(".") + 1).toLowerCase() : "";
  const inferred = BY_EXTENSION[ext];
  return TYPES.find((type) => type.mime === inferred) ?? null;
}

/// The effective byte limit for a type on the session's agent API.
export function attachmentLimit(type: AttachmentType, apiKind: string | undefined): number {
  return type.kind === "image" && apiKind === "anthropic:messages"
    ? Math.min(type.maxBytes, MAX_ANTHROPIC_IMAGE_BYTES)
    : type.maxBytes;
}

export const attachmentUploadSchema = z.object({
  bytesBase64: z.string().min(4).max(4 * Math.ceil(MAX_ATTACHMENT_BYTES / 3))
    .regex(/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/),
}).strict();

export const messageAttachmentSchema = z.object({
  blobRef: z.string().regex(/^sha256:[a-f0-9]{64}$/),
  mime: z.enum(ATTACHMENT_MIMES as [string, ...string[]]),
  kind: z.enum(["image", "document"]),
  name: z.string().trim().min(1).max(256),
}).strict().refine(
  ({ mime, kind }) => TYPES.find((type) => type.mime === mime)?.kind === kind,
  { message: "The attachment kind does not match its type.", path: ["kind"] },
);
export type MessageAttachment = z.infer<typeof messageAttachmentSchema>;

const routeName = z.string().min(1).max(512).refine((value) => value.trim() === value, "Remove surrounding whitespace.");

/// Per-message model choices. They ride the run as overrides and never
/// change the stored session configuration. Provider and API kind must match
/// the session's pinned route; the runtime validates that and the effort tier.
export const messageRunOptionsSchema = z.object({
  model: z.object({ providerId: routeName, apiKind: routeName, model: routeName }).strict().optional(),
  reasoningEffort: z.string().trim().min(1).max(32).optional(),
}).strict();
export type MessageRunOptions = z.infer<typeof messageRunOptionsSchema>;

const hasContent = ({ text, attachments }: { text: string; attachments: unknown[] }) =>
  text.trim().length > 0 || attachments.length > 0;
const contentMessage = { message: "Write a message or attach a file.", path: ["text"] };

export const sessionMessageSchema = z.object({
  text: z.string().max(100_000).default(""),
  submissionId: z.string().min(1).max(200),
  attachments: z.array(messageAttachmentSchema).max(MAX_MESSAGE_ATTACHMENTS).default([]),
  options: messageRunOptionsSchema.optional(),
}).refine(hasContent, contentMessage);

/// Steering has no run options: it joins a run that already chose them.
/// Unknown top-level keys are dropped, so a client cannot supply its own
/// origin; the route stamps the signed-in user.
export const sessionSteerSchema = z.object({
  text: z.string().max(100_000).default(""),
  attachments: z.array(messageAttachmentSchema).max(MAX_MESSAGE_ATTACHMENTS).default([]),
}).refine(hasContent, contentMessage);

/// Effort tiers each agent API accepts, used when provider discovery reports
/// none for a model. Keep aligned with the runtime's validation lists.
const EFFORT_TIERS: Readonly<Record<string, readonly string[]>> = {
  "openai:responses": ["none", "minimal", "low", "medium", "high", "xhigh"],
  "openai:completions": ["none", "minimal", "low", "medium", "high", "xhigh", "max"],
  "anthropic:messages": ["none", "low", "medium", "high", "xhigh", "max"],
};
export function reasoningEffortTiers(apiKind: string): readonly string[] {
  return EFFORT_TIERS[apiKind] ?? [];
}

/// Run input items for a composer message: media first, so the model reads
/// the files before the words that refer to them.
export function messageInputItems(text: string, attachments: readonly MessageAttachment[], origin: string) {
  return [
    ...attachments.map((attachment) => ({
      type: "media" as const,
      origin,
      blobRef: attachment.blobRef,
      mime: attachment.mime,
      kind: attachment.kind,
      name: attachment.name,
    })),
    ...(text.trim() ? [{ type: "text" as const, text, origin }] : []),
  ];
}

/// The run-start config for per-message options, or undefined when none.
export function messageRunConfig(options: MessageRunOptions | undefined) {
  if (!options) return undefined;
  const generation = options.reasoningEffort ? { reasoningEffort: options.reasoningEffort } : {};
  const config = {
    ...(options.model ? { model: options.model } : {}),
    ...(Object.keys(generation).length ? { generation } : {}),
  };
  return Object.keys(config).length ? config : undefined;
}
