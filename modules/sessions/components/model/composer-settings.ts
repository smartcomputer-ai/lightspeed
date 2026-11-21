import { sessionSteerSchema, type AttachmentKind, type MessageAttachment } from "@lightspeed-ai/platform-shared";
import { defineSettingFamily, registerDeviceExpiry } from "@lightspeed-ai/platform-web/modules/settings";
import type { RunChoice } from "./composer-model";

/// A ready attachment saved beside a composer draft.
export interface StoredAttachment {
  id: string;
  name: string;
  mime: string;
  kind: AttachmentKind;
  size: number;
  blobRef: string;
}

/// A queued message kept for recovery while it is converted to steering.
export interface StoredQueuedRecovery {
  id: string;
  error: string;
  message: { text: string; attachments: MessageAttachment[] };
}

const EMPTY_RUN_CHOICE: RunChoice = Object.freeze({});
const EMPTY_LIST: readonly never[] = Object.freeze([]);
const record = (value: unknown): Record<string, unknown> | undefined =>
  value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : undefined;

/// Drafts and the choices kept beside them, for sessions since deleted too:
/// dropped after a month without use.
export const DRAFT_PREFIX = "lightspeed:sessions:draft:";
const QUEUED_STEERING_PREFIX = "lightspeed:sessions:queued-steering:";
registerDeviceExpiry([DRAFT_PREFIX, QUEUED_STEERING_PREFIX]);

/// Drafts change on every keystroke: they reach storage a moment later,
/// and at once when the page is hidden.
const DRAFT_WRITE_DELAY_MS = 250;

export const composerSettings = {
  /// Unsent text for one conversation.
  composerDraft: defineSettingFamily("composerDraft", (draftKey: string) => ({
    persistence: "device" as const,
    key: draftKey,
    format: "text" as const,
    defaultValue: "",
    parse: (stored: unknown) => (typeof stored === "string" ? stored : undefined),
    isEmpty: (text: string) => text === "",
    writeDelayMs: DRAFT_WRITE_DELAY_MS,
  })),
  /// The model and effort a composer sends with, where they differ from the session's.
  composerRunChoice: defineSettingFamily("composerRunChoice", (draftKey: string) => ({
    persistence: "device" as const,
    key: `${draftKey}:run`,
    defaultValue: EMPTY_RUN_CHOICE,
    parse: (stored: unknown): RunChoice | undefined => {
      const value = record(stored);
      if (!value) return undefined;
      return {
        ...(typeof value.model === "string" && value.model ? { model: value.model } : {}),
        ...(typeof value.reasoningEffort === "string" && value.reasoningEffort ? { reasoningEffort: value.reasoningEffort } : {}),
      };
    },
    isEmpty: (choice: RunChoice) => Object.keys(choice).length === 0,
  })),
  /// A composer's uploaded attachments, kept with its draft.
  composerAttachments: defineSettingFamily("composerAttachments", (draftKey: string) => ({
    persistence: "device" as const,
    key: `${draftKey}:attachments`,
    defaultValue: EMPTY_LIST as readonly StoredAttachment[],
    parse: (stored: unknown): readonly StoredAttachment[] | undefined => {
      if (!Array.isArray(stored)) return undefined;
      return stored.flatMap((value): StoredAttachment[] => {
        const item = record(value);
        return item && typeof item.id === "string" && typeof item.name === "string" && typeof item.mime === "string"
          && (item.kind === "image" || item.kind === "document") && typeof item.size === "number"
          && typeof item.blobRef === "string"
          ? [{ id: item.id, name: item.name, mime: item.mime, kind: item.kind, size: item.size, blobRef: item.blobRef }]
          : [];
      });
    },
    isEmpty: (items: readonly StoredAttachment[]) => items.length === 0,
  })),
  /// Queued messages being converted to steering, kept so a reload or a lost
  /// response cannot lose them; by session scope.
  queuedSteering: defineSettingFamily("queuedSteering", (scope: string) => ({
    persistence: "device" as const,
    key: `${QUEUED_STEERING_PREFIX}${scope}`,
    defaultValue: EMPTY_LIST as readonly StoredQueuedRecovery[],
    parse: (stored: unknown): readonly StoredQueuedRecovery[] | undefined => {
      if (!Array.isArray(stored)) return undefined;
      return stored.flatMap((value): StoredQueuedRecovery[] => {
        const item = record(value);
        const input = sessionSteerSchema.safeParse(item?.message);
        if (!item || !input.success || typeof item.id !== "string" || typeof item.error !== "string") return [];
        return [{ id: item.id, error: item.error, message: { text: input.data.text, attachments: input.data.attachments } }];
      });
    },
    isEmpty: (items: readonly StoredQueuedRecovery[]) => items.length === 0,
  })),
};
