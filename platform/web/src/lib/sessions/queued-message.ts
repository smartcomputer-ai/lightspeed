import type { RunView } from "@lightspeed-ai/sdk";
import { sessionSteerSchema } from "@lightspeed-ai/platform-shared";
import { ApiError } from "@/api";
import type { ComposerMessage } from "@/components/session/composer";
import { SETTINGS, settings } from "@/lib/settings";

export interface QueuedMessageRecovery {
  id: string;
  scope: string;
  message: ComposerMessage;
  error: string;
  sending?: boolean;
}

// A cancelled run can be restored only once, including across reloads and
// lost POST responses. Run submission ids are scoped to the session.
export const queuedRecoverySubmissionId = (runId: string) => `queued-steer:${runId}`;

export function steeringWasNotSubmitted(error: unknown): boolean {
  return error instanceof ApiError && error.status === 409
    && typeof error.body === "object" && error.body !== null
    && "kind" in error.body && error.body.kind === "steering_not_submitted";
}

export function readQueuedRecovery(scope: string): QueuedMessageRecovery[] {
  return settings.get(SETTINGS.queuedSteering(scope)).map((item) => ({
    id: item.id,
    scope,
    error: item.error,
    message: { text: item.message.text, attachments: item.message.attachments.map((attachment) => ({ ...attachment, size: 0 })) },
  }));
}

/** Save before cancellation so interrupted conversions can be recovered after reload. */
export function storeQueuedRecovery(scope: string, id: string, recovery?: QueuedMessageRecovery) {
  const saved = readQueuedRecovery(scope).filter((item) => item.id !== id);
  if (recovery) saved.push(recovery);
  settings.set(SETTINGS.queuedSteering(scope), saved.map((item) => ({
    id: item.id, error: item.error, message: {
      text: item.message.text,
      attachments: item.message.attachments.map(({ blobRef, mime, kind, name }) => ({ blobRef, mime, kind, name })),
    },
  }))).catch(() => {
    // Recovery still works within this page when storage is blocked.
  });
}

/** Recover complete input before cancelling; queue labels are bounded previews. */
export async function queuedMessage(
  run: RunView,
  loadText: (blobRef: string) => Promise<string>,
): Promise<ComposerMessage> {
  const texts: string[] = [];
  const attachments = [];
  for (const item of run.source.items) {
    switch (item.type) {
      case "text": texts.push(item.text); break;
      case "textRef": texts.push(await loadText(item.blobRef)); break;
      case "media": attachments.push({
        blobRef: item.blobRef, mime: item.mime, kind: item.kind,
        name: item.name ?? (item.kind === "image" ? "Image" : "Document"),
      }); break;
      default: throw new Error("This queued message contains input that cannot be converted to steering.");
    }
  }
  // Validate against the existing route before removing anything from the queue.
  const result = sessionSteerSchema.safeParse({ text: texts.join("\n\n"), attachments });
  if (!result.success) throw new Error("This queued message cannot be converted to steering.");
  return { text: result.data.text, attachments: result.data.attachments.map((attachment) => ({ ...attachment, size: 0 })) };
}
