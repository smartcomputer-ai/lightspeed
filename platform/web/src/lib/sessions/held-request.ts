import { ApiError } from "@/api";

export function isOperationPending(error: unknown): boolean {
  return error instanceof ApiError
    && typeof error.body === "object"
    && error.body !== null
    && "kind" in error.body
    && error.body.kind === "operationPending";
}

/// A session holds a run start or a steering while it compacts, which can
/// outlast the server's wait. The request is still queued then, so sending the
/// same submission id again is what follows it, and can never land it twice.
/// Null when the caller stopped watching; the request lands regardless.
export async function postUntilAccepted<T>(
  post: () => Promise<T>,
  stillWatching: () => boolean,
): Promise<T | null> {
  for (;;) {
    try {
      return await post();
    } catch (error) {
      if (!isOperationPending(error)) throw error;
      if (!stillWatching()) return null;
    }
  }
}
