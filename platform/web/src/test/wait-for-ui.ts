import { act } from "react";
import { vi } from "vitest";

/** Retry an assertion after flushing React and queued query notifications.
 * The assertion determines readiness; yielding a task is not a loading budget.
 * Keep interactions outside this callback so retries cannot repeat a mutation.
 */
export async function waitForUi<T>(assertion: () => T): Promise<T> {
  return vi.waitFor(async () => {
    await act(async () => {
      await new Promise<void>((resolve) => setTimeout(resolve, 0));
    });
    return assertion();
  });
}
