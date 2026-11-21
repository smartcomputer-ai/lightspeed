import { DRAFT_PREFIX } from "./composer-settings";

/** Shared by normal sessions and every bot conversation showing that session. */
export function sessionDraftKey(universeId: string, sessionId: string): string {
  return `${DRAFT_PREFIX}${JSON.stringify([universeId, sessionId])}`;
}
