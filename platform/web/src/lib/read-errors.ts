/** Transport failures are recoverable; permission and invalid-data errors are not. */
export function isTransientReadError(error: unknown): boolean {
  if (error instanceof TypeError || (error instanceof DOMException && error.name === "TimeoutError")) return true;
  if (!error || typeof error !== "object" || !("status" in error)) return false;
  const status = error.status;
  return typeof status === "number" && (status >= 500 || status === 408 || status === 429);
}

export function retryRead(failureCount: number, error: unknown): boolean {
  return failureCount < 1 && isTransientReadError(error);
}
